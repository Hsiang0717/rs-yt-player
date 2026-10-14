use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::debug;

#[allow(dead_code)]
const DEFAULT_CHUNK_SIZE: usize = 512 * 1024; // 512 KB

/// A stream reader that implements `Read + Seek` using HTTP Range requests.
pub type UrlRefresher =
    Arc<dyn Fn() -> anyhow::Result<(String, Option<HashMap<String, String>>)> + Send + Sync>;

pub fn is_url_expired(url: &str) -> bool {
    if let Some(pos) = url.find("expire=") {
        let remainder = &url[pos + 7..];
        let expire_str = remainder.split('&').next().unwrap_or("");
        if let Ok(expire_ts) = expire_str.parse::<u64>() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            return now + 60 >= expire_ts;
        }
    }
    false
}

fn build_blocking_client(
    request_headers: Option<&HashMap<String, String>>,
) -> std::io::Result<reqwest::blocking::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(request_headers) = request_headers {
        for (name, value) in request_headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
            })?;
            let value = reqwest::header::HeaderValue::from_str(value).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
            })?;
            headers.insert(name, value);
        }
    }
    reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(15))
        .default_headers(headers)
        .build()
        .map_err(std::io::Error::other)
}

#[allow(dead_code)]
pub struct HttpStreamReader {
    client: reqwest::blocking::Client,
    url: String,
    total_size: u64,
    pos: u64,
    buffer: Vec<u8>,
    buffer_start: u64,
    chunk_size: usize,
    url_refresher: Option<UrlRefresher>,
    request_headers: Option<HashMap<String, String>>,
}

#[allow(dead_code)]
impl HttpStreamReader {
    pub fn new(url: &str) -> std::io::Result<Self> {
        Self::new_with_headers(url, None)
    }

    pub fn new_with_headers(
        url: &str,
        request_headers: Option<&HashMap<String, String>>,
    ) -> std::io::Result<Self> {
        Self::new_with_headers_and_size(url, request_headers, None)
    }

    pub fn new_with_headers_and_size(
        url: &str,
        request_headers: Option<&HashMap<String, String>>,
        known_total_size: Option<u64>,
    ) -> std::io::Result<Self> {
        Self::new_with_headers_and_size_and_refresher(url, request_headers, known_total_size, None)
    }

    pub fn new_with_headers_and_size_and_refresher(
        url: &str,
        request_headers: Option<&HashMap<String, String>>,
        known_total_size: Option<u64>,
        url_refresher: Option<UrlRefresher>,
    ) -> std::io::Result<Self> {
        let client = build_blocking_client(request_headers)?;

        let total_size = if let Some(size) = known_total_size.filter(|&s| s > 0) {
            tracing::info!(
                "HttpStreamReader reused known total_size = {} bytes (skipped probe)",
                size
            );
            size
        } else {
            // Probe content length using Range: bytes=0-0
            let resp = client
                .get(url)
                .header("Range", "bytes=0-0")
                .send()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::ConnectionRefused, e))?;

            let total_size = if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                // Content-Range: bytes 0-0/12345678
                if let Some(content_range) = resp.headers().get(reqwest::header::CONTENT_RANGE) {
                    let range_str = content_range.to_str().unwrap_or("");
                    if let Some(total_str) = range_str.split('/').next_back() {
                        total_str.trim().parse::<u64>().unwrap_or(0)
                    } else {
                        0
                    }
                } else {
                    0
                }
            } else if resp.status().is_success() {
                resp.content_length().unwrap_or(0)
            } else {
                return Err(std::io::Error::other(
                    format!("Failed to probe audio URL, status: {}", resp.status()),
                ));
            };

            if total_size == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "The audio server did not report the length required for HTTP Range seeking",
                ));
            }

            tracing::info!(
                "HttpStreamReader initialized: probe_status = {:?}, total_size = {} bytes",
                resp.status(),
                total_size
            );
            total_size
        };

        let mut reader = Self {
            client,
            url: url.to_string(),
            total_size,
            pos: 0,
            buffer: Vec::new(),
            buffer_start: 0,
            chunk_size: DEFAULT_CHUNK_SIZE,
            url_refresher,
            request_headers: request_headers.cloned(),
        };

        // Pre-fetch the first chunk
        reader.fetch_chunk(0)?;

        Ok(reader)
    }

    fn fetch_chunk(&mut self, start: u64) -> std::io::Result<()> {
        if self.total_size > 0 && start >= self.total_size {
            self.buffer.clear();
            self.buffer_start = start;
            return Ok(());
        }

        let end = if self.total_size > 0 {
            std::cmp::min(start + self.chunk_size as u64 - 1, self.total_size - 1)
        } else {
            start + self.chunk_size as u64 - 1
        };

        let range_header = format!("bytes={}-{}", start, end);
        debug!("Fetching audio chunk: {}", range_header);

        let mut refreshed_during_fetch = false;
        for attempt in 0..3 {
            if !refreshed_during_fetch && is_url_expired(&self.url)
                && let Some(refresher) = &self.url_refresher
                    && let Ok((new_url, new_headers)) = refresher() {
                        tracing::info!("Proactively refreshed expired YouTube stream URL before chunk fetch.");
                        self.url = new_url;
                        if let Some(headers) = new_headers {
                            self.request_headers = Some(headers.clone());
                            if let Ok(new_client) = build_blocking_client(Some(&headers)) {
                                self.client = new_client;
                            }
                        }
                        refreshed_during_fetch = true;
                    }

            let response = match self
                .client
                .get(&self.url)
                .header("Range", &range_header)
                .send()
            {
                Ok(response) => response,
                Err(error) if attempt < 2 && (error.is_timeout() || error.is_connect()) => {
                    // Discard broken pooled TCP sockets that were severed during sleep
                    if let Ok(new_client) = build_blocking_client(self.request_headers.as_ref()) {
                        self.client = new_client;
                    }
                    if !refreshed_during_fetch
                        && let Some(refresher) = &self.url_refresher
                        && let Ok((new_url, new_headers)) = refresher()
                    {
                        tracing::info!("Proactively refreshed YouTube stream URL after network connect/timeout error.");
                        self.url = new_url;
                        if let Some(headers) = new_headers {
                            self.request_headers = Some(headers.clone());
                            if let Ok(new_client) = build_blocking_client(Some(&headers)) {
                                self.client = new_client;
                            }
                        }
                        refreshed_during_fetch = true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(300 * (attempt + 1)));
                    continue;
                }
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        error,
                    ));
                }
            };

            if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                let status = response.status();
                if (status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::GONE)
                    && !refreshed_during_fetch
                    && let Some(refresher) = &self.url_refresher
                        && let Ok((new_url, new_headers)) = refresher() {
                            tracing::info!(
                                "Refreshed YouTube stream URL after receiving HTTP {}",
                                status
                            );
                            self.url = new_url;
                            if let Some(headers) = new_headers {
                                self.request_headers = Some(headers.clone());
                                if let Ok(new_client) = build_blocking_client(Some(&headers)) {
                                    self.client = new_client;
                                }
                            }
                            refreshed_during_fetch = true;
                            continue;
                        }

                if attempt < 2
                    && (status.is_server_error()
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS)
                {
                    std::thread::sleep(std::time::Duration::from_millis(250 * (attempt + 1)));
                    continue;
                }
                return Err(std::io::Error::other(
                    format!("HTTP range request was not honored (status: {})", status),
                ));
            }

            let returned_start = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|range| range.to_str().ok())
                .and_then(|range| range.strip_prefix("bytes "))
                .and_then(|range| range.split('-').next())
                .and_then(|start| start.parse::<u64>().ok())
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Server omitted a valid Content-Range header",
                    )
                })?;
            if returned_start != start {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Server returned byte range starting at {}, expected {}",
                        returned_start, start
                    ),
                ));
            }

            let bytes = match response.bytes() {
                Ok(bytes) => bytes,
                Err(error) if attempt < 2 && (error.is_timeout() || error.is_connect()) => {
                    std::thread::sleep(std::time::Duration::from_millis(250 * (attempt + 1)));
                    continue;
                }
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        error,
                    ));
                }
            };

            self.buffer = bytes.to_vec();
            self.buffer_start = start;
            return Ok(());
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "HTTP Range request failed after retries",
        ))
    }
}

impl Read for HttpStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.total_size > 0 && self.pos >= self.total_size {
            return Ok(0); // EOF
        }

        // Check if self.pos is inside the current cached buffer
        let buffer_end = self.buffer_start + self.buffer.len() as u64;
        if self.pos < self.buffer_start || self.pos >= buffer_end {
            self.fetch_chunk(self.pos)?;
            if self.buffer.is_empty() {
                return Ok(0); // EOF
            }
        }

        let offset_in_buffer = (self.pos - self.buffer_start) as usize;
        let available = self.buffer.len() - offset_in_buffer;
        let to_copy = std::cmp::min(buf.len(), available);

        buf[..to_copy].copy_from_slice(&self.buffer[offset_in_buffer..offset_in_buffer + to_copy]);
        self.pos += to_copy as u64;

        Ok(to_copy)
    }
}

impl Seek for HttpStreamReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(offset) => self.pos as i64 + offset,
            SeekFrom::End(offset) => {
                if self.total_size == 0 {
                    tracing::warn!("SeekFrom::End failed: total_size is 0");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Cannot seek from end on unknown stream length",
                    ));
                }
                self.total_size as i64 + offset
            }
        };

        if new_pos < 0 {
            tracing::warn!("Seek failed: new_pos {} < 0", new_pos);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid seek to negative position",
            ));
        }

        tracing::info!(
            "Seek: {:?} -> new_pos: {}, total_size: {}",
            pos,
            new_pos,
            self.total_size
        );
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

const RANGE_READ_AHEAD_CAPACITY: usize = 8 * 1024 * 1024;

pub const MAX_BYTE_CACHE_BYTES: usize = 32 * 1024 * 1024; // 32 MB capacity bound

/// Sparse store of already-downloaded byte segments, shared across seeks of the same stream
/// so that seeking into a previously downloaded region needs no new HTTP request.
#[derive(Default)]
pub struct ByteCache {
    segments: std::collections::BTreeMap<u64, Vec<u8>>,
}

pub type SharedByteCache = Arc<Mutex<ByteCache>>;

impl ByteCache {
    /// Copies up to `max` cached bytes starting exactly at `pos`, if `pos` is cached.
    pub fn read_at(&self, pos: u64, max: usize) -> Option<Vec<u8>> {
        let (start, seg) = self.segments.range(..=pos).next_back()?;
        let offset = (pos - start) as usize;
        if offset >= seg.len() {
            return None;
        }
        let end = seg.len().min(offset + max);
        Some(seg[offset..end].to_vec())
    }

    pub fn total_bytes(&self) -> usize {
        self.segments.values().map(|s| s.len()).sum()
    }

    pub fn insert(&mut self, pos: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut key = pos;
        let prev = self
            .segments
            .range(..=pos)
            .next_back()
            .map(|(s, v)| (*s, v.len() as u64));
        match prev {
            Some((start, len)) if start + len >= pos => {
                key = start;
                let seg = self.segments.get_mut(&start).unwrap();
                let overlap = (start + len - pos) as usize;
                if overlap < data.len() {
                    seg.extend_from_slice(&data[overlap..]);
                }
            }
            _ => {
                self.segments.insert(pos, data.to_vec());
            }
        }
        // Merge following segments that now touch or overlap.
        loop {
            let seg_end = key + self.segments[&key].len() as u64;
            let next = self.segments.range(key + 1..).next().map(|(s, _)| *s);
            match next {
                Some(next_start) if next_start <= seg_end => {
                    let next_seg = self.segments.remove(&next_start).unwrap();
                    let skip = (seg_end - next_start) as usize;
                    if skip < next_seg.len() {
                        self.segments
                            .get_mut(&key)
                            .unwrap()
                            .extend_from_slice(&next_seg[skip..]);
                    }
                }
                _ => break,
            }
        }

        // Enforce maximum memory bound for multi-hour audio
        let mut total = self.total_bytes();
        while total > MAX_BYTE_CACHE_BYTES && self.segments.len() > 1 {
            let min_key = *self.segments.keys().next().unwrap();
            let max_key = *self.segments.keys().next_back().unwrap();
            let evict_key = if pos.abs_diff(min_key) >= pos.abs_diff(max_key) {
                min_key
            } else {
                max_key
            };
            if let Some(removed) = self.segments.remove(&evict_key) {
                total = total.saturating_sub(removed.len());
            } else {
                break;
            }
        }

        if total > MAX_BYTE_CACHE_BYTES
            && let Some((&first_key, seg)) = self.segments.iter_mut().next()
                && seg.len() > MAX_BYTE_CACHE_BYTES {
                    let excess = seg.len() - MAX_BYTE_CACHE_BYTES;
                    if pos > first_key + excess as u64 {
                        seg.drain(..excess);
                        let new_key = first_key + excess as u64;
                        let moved_seg = std::mem::take(seg);
                        self.segments.remove(&first_key);
                        self.segments.insert(new_key, moved_seg);
                    } else {
                        seg.truncate(MAX_BYTE_CACHE_BYTES);
                    }
                }
    }
}

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

pub struct DecodedAudioSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    samples: Vec<f32>,
    sample_position: usize,
    samples_to_skip: u64,
}

struct ProbeGateSource {
    inner: Box<dyn MediaSource>,
    allow_seek: Arc<std::sync::atomic::AtomicBool>,
}

impl MediaSource for ProbeGateSource {
    fn is_seekable(&self) -> bool {
        self.allow_seek.load(std::sync::atomic::Ordering::Acquire)
    }

    fn byte_len(&self) -> Option<u64> {
        self.inner.byte_len()
    }
}

impl Read for ProbeGateSource {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl Seek for ProbeGateSource {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(position)
    }
}

impl DecodedAudioSource {
    pub fn new<R: Read + Send + 'static>(reader: R, start_offset_secs: f32) -> Result<Self> {
        let source = NonSeekableSource(Mutex::new(reader));
        Self::from_media_source(Box::new(source), start_offset_secs, false)
    }

    pub fn new_seekable(source: AsyncRangeMediaSource, start_offset_secs: f32) -> Result<Self> {
        Self::from_media_source(Box::new(source), start_offset_secs, true)
    }

    fn from_media_source(
        source: Box<dyn MediaSource>,
        start_offset_secs: f32,
        seekable: bool,
    ) -> Result<Self> {
        // Crucial optimization for multi-hour audio: keep is_seekable false during probe
        // so Symphonia's ISOBMFF demuxer stops at the first fragment instead of issuing
        // hundreds or thousands of HTTP Range seeks to scan all movie fragments across the file!
        let allow_seek = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate_source = ProbeGateSource {
            inner: source,
            allow_seek: Arc::clone(&allow_seek),
        };

        let stream = MediaSourceStream::new(Box::new(gate_source), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("m4a");

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .context("Unable to identify the downloaded AAC audio stream")?;

        if seekable {
            allow_seek.store(true, std::sync::atomic::Ordering::Release);
        }

        let mut format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| anyhow!("The audio stream contains no playable track"))?;
        let track_id = track.id;
        let time_base = track
            .codec_params
            .time_base
            .context("The audio stream does not declare a seek time base")?;
        let sample_rate = track
            .codec_params
            .sample_rate
            .ok_or_else(|| anyhow!("The audio stream does not declare a sample rate"))?;
        let channels = track
            .codec_params
            .channels
            .map(|channels| channels.count() as u16)
            .unwrap_or(2);
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .context("No built-in decoder is available for this audio stream")?;
        let samples_to_skip = if seekable && start_offset_secs > 0.0 {
            let seeked = format
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time: symphonia::core::units::Time::from(start_offset_secs as f64),
                        track_id: Some(track_id),
                    },
                )
                .context("The AAC container could not seek to the requested playback time")?;
            let actual_time = time_base.calc_time(seeked.actual_ts);
            let actual_seconds = actual_time.seconds as f64 + actual_time.frac;
            ((start_offset_secs as f64 - actual_seconds).max(0.0)
                * sample_rate as f64
                * channels as f64) as u64
        } else {
            (start_offset_secs.max(0.0) * sample_rate as f32 * channels as f32) as u64
        };

        Ok(Self {
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            samples: Vec::new(),
            sample_position: 0,
            samples_to_skip,
        })
    }

    fn decode_next_packet(&mut self) -> bool {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return false;
                }
                Err(error) => {
                    tracing::warn!("Audio container read stopped: {}", error);
                    return false;
                }
            };
            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(error)) => {
                    tracing::debug!("Skipping an undecodable AAC packet: {}", error);
                    continue;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(error) => {
                    tracing::warn!("Audio decoder stopped: {}", error);
                    return false;
                }
            };
            let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
            samples.copy_interleaved_ref(decoded);
            self.samples.clear();
            self.samples.extend_from_slice(samples.samples());
            self.sample_position = 0;
            if !self.samples.is_empty() {
                return true;
            }
        }
    }
}

impl Iterator for DecodedAudioSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.sample_position >= self.samples.len() && !self.decode_next_packet() {
                return None;
            }
            if self.samples_to_skip > 0 {
                let remaining = self.samples.len() - self.sample_position;
                let skip = remaining.min(self.samples_to_skip.min(usize::MAX as u64) as usize);
                self.sample_position += skip;
                self.samples_to_skip -= skip as u64;
                continue;
            }
            let sample = self.samples[self.sample_position];
            self.sample_position += 1;
            return Some(sample);
        }
    }
}

impl rodio::Source for DecodedAudioSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

struct NonSeekableSource<R>(Mutex<R>);

impl<R: Read + Send> Read for NonSeekableSource<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0
            .get_mut()
            .map_err(|_| std::io::Error::other("Audio stream lock was poisoned"))?
            .read(buffer)
    }
}

impl<R: Send> Seek for NonSeekableSource<R> {
    fn seek(&mut self, _position: SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Audio stream is not seekable",
        ))
    }
}

impl<R: Read + Send> MediaSource for NonSeekableSource<R> {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}
