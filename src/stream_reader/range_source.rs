use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use symphonia::core::io::MediaSource;

use super::cache::SharedByteCache;
use super::http::{HttpStreamReader, UrlRefresher};

const RANGE_READ_AHEAD_CAPACITY: usize = 8 * 1024 * 1024;

struct RangeBufferState {
    data: VecDeque<u8>,
    position: u64,
    generation: u64,
    seek_to: Option<u64>,
    is_closed: bool,
    is_terminated: bool,
    waiting_for_data: bool,
    error: Option<String>,
}

#[derive(Clone)]
pub struct RangeBufferStats {
    state: Arc<(Mutex<RangeBufferState>, Condvar, Condvar)>,
    total_size: u64,
}

impl RangeBufferStats {
    pub fn is_buffering(&self) -> bool {
        self.state.0.lock().unwrap().waiting_for_data
    }

    pub fn stream_error(&self) -> Option<String> {
        self.state.0.lock().unwrap().error.clone()
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    pub fn buffered_position_secs(&self, total_duration_secs: f32) -> f32 {
        if self.total_size == 0 || total_duration_secs <= 0.0 {
            return 0.0;
        }
        let state = self.state.0.lock().unwrap();
        let buffered_byte_position = state.position.saturating_add(state.data.len() as u64);
        (buffered_byte_position as f64 / self.total_size as f64 * total_duration_secs as f64) as f32
    }
}

/// A seekable MediaSource whose HTTP requests are prefetched off Rodio's audio thread.
pub struct AsyncRangeMediaSource {
    state: Arc<(Mutex<RangeBufferState>, Condvar, Condvar)>,
    total_size: u64,
}

impl AsyncRangeMediaSource {
    #[allow(dead_code)]
    pub fn new_with_headers(
        url: &str,
        request_headers: Option<&HashMap<String, String>>,
        cache: SharedByteCache,
    ) -> std::io::Result<Self> {
        Self::new_with_headers_and_size(url, request_headers, None, cache)
    }

    #[allow(dead_code)]
    pub fn new_with_headers_and_size(
        url: &str,
        request_headers: Option<&HashMap<String, String>>,
        known_total_size: Option<u64>,
        cache: SharedByteCache,
    ) -> std::io::Result<Self> {
        Self::new_with_headers_and_size_and_refresher(url, request_headers, known_total_size, cache, None)
    }

    pub fn new_with_headers_and_size_and_refresher(
        url: &str,
        request_headers: Option<&HashMap<String, String>>,
        known_total_size: Option<u64>,
        cache: SharedByteCache,
        url_refresher: Option<UrlRefresher>,
    ) -> std::io::Result<Self> {
        let reader = HttpStreamReader::new_with_headers_and_size_and_refresher(
            url,
            request_headers,
            known_total_size,
            url_refresher,
        )?;
        let total_size = reader.total_size;
        let state = Arc::new((
            Mutex::new(RangeBufferState {
                data: VecDeque::with_capacity(RANGE_READ_AHEAD_CAPACITY.min(64 * 1024)),
                position: 0,
                generation: 0,
                seek_to: None,
                is_closed: false,
                is_terminated: false,
                waiting_for_data: false,
                error: None,
            }),
            Condvar::new(),
            Condvar::new(),
        ));

        let producer_state = Arc::clone(&state);
        thread::Builder::new()
            .name("http-range-prefetch".to_string())
            .spawn(move || {
                let mut reader = reader;
                let mut chunk = vec![0u8; 32 * 1024];
                // `cursor` is the logical byte position being produced; `reader_pos` is where the
                // HTTP reader actually is. They diverge while serving from cache.
                let mut cursor: u64 = 0;
                let mut reader_pos: u64 = 0;
                loop {
                    let (lock, not_empty, not_full) = &*producer_state;
                    let mut current = lock.lock().unwrap();
                    while !current.is_terminated
                        && current.seek_to.is_none()
                        && (current.is_closed || current.data.len() >= RANGE_READ_AHEAD_CAPACITY)
                    {
                        current = not_full.wait(current).unwrap();
                    }
                    if current.is_terminated {
                        break;
                    }
                    if let Some(target) = current.seek_to.take() {
                        // Lazy: the HTTP reader is only repositioned on a cache miss.
                        cursor = target;
                        continue;
                    }

                    let available =
                        (RANGE_READ_AHEAD_CAPACITY - current.data.len()).min(chunk.len());
                    let generation = current.generation;
                    drop(current);

                    let cached = cache.lock().unwrap().read_at(cursor, available);
                    if let Some(bytes) = cached {
                        let mut current = lock.lock().unwrap();
                        if current.generation == generation {
                            cursor += bytes.len() as u64;
                            current.data.extend(bytes);
                            not_empty.notify_one();
                        }
                        continue;
                    }

                    if reader_pos != cursor {
                        if let Err(error) = reader.seek(SeekFrom::Start(cursor)) {
                            let mut current = lock.lock().unwrap();
                            if current.generation == generation {
                                current.error = Some(error.to_string());
                                current.is_closed = true;
                                not_empty.notify_all();
                            }
                            continue;
                        }
                        reader_pos = cursor;
                    }

                    let read_result = reader.read(&mut chunk[..available]);
                    if let Ok(read) = &read_result {
                        reader_pos += *read as u64;
                    }
                    let mut current = lock.lock().unwrap();
                    if current.generation != generation {
                        continue;
                    }
                    match read_result {
                        Ok(0) => {
                            current.is_closed = true;
                            not_empty.notify_all();
                            not_full.notify_all();
                        }
                        Ok(read) => {
                            cache.lock().unwrap().insert(cursor, &chunk[..read]);
                            cursor += read as u64;
                            current.data.extend(&chunk[..read]);
                            not_empty.notify_one();
                        }
                        Err(error) => {
                            current.error = Some(error.to_string());
                            current.is_closed = true;
                            not_empty.notify_all();
                            not_full.notify_all();
                        }
                    }
                }
            })
            .map_err(std::io::Error::other)?;

        Ok(Self { state, total_size })
    }

    pub fn stats(&self) -> RangeBufferStats {
        RangeBufferStats {
            state: Arc::clone(&self.state),
            total_size: self.total_size,
        }
    }
}

impl Read for AsyncRangeMediaSource {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let (lock, not_empty, not_full) = &*self.state;
        let mut state = lock.lock().unwrap();
        loop {
            if !state.data.is_empty() {
                state.waiting_for_data = false;
                let count = output.len().min(state.data.len());
                for byte in &mut output[..count] {
                    *byte = state.data.pop_front().unwrap();
                }
                state.position += count as u64;
                not_full.notify_one();
                return Ok(count);
            }
            if let Some(error) = state.error.clone() {
                state.waiting_for_data = false;
                return Err(std::io::Error::other(error));
            }
            if state.is_closed {
                state.waiting_for_data = false;
                return Ok(0);
            }
            state.waiting_for_data = true;
            state = not_empty.wait(state).unwrap();
        }
    }
}

impl Seek for AsyncRangeMediaSource {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let (lock, not_empty, not_full) = &*self.state;
        let mut state = lock.lock().unwrap();
        let target = match position {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::Current(offset) => state.position as i128 + offset as i128,
            SeekFrom::End(offset) => self.total_size as i128 + offset as i128,
        };
        if target < 0 || target > u64::MAX as i128 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid byte seek position",
            ));
        }
        let target = target as u64;
        state.position = target;
        state.generation += 1;
        state.seek_to = Some(target);
        state.data.clear();
        state.error = None;
        state.is_closed = false;
        state.waiting_for_data = false;
        not_empty.notify_all();
        not_full.notify_all();
        Ok(target)
    }
}

impl MediaSource for AsyncRangeMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        (self.total_size > 0).then_some(self.total_size)
    }
}

impl Drop for AsyncRangeMediaSource {
    fn drop(&mut self) {
        let (lock, not_empty, not_full) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.is_closed = true;
        state.is_terminated = true;
        state.generation += 1;
        not_empty.notify_all();
        not_full.notify_all();
    }
}
