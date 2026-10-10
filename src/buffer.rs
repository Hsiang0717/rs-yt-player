use std::collections::VecDeque;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

pub const DEFAULT_BUFFER_CAPACITY: usize = 2 * 1024 * 1024; // 2 MB compressed audio read-ahead buffer
#[allow(dead_code)]
pub const PCM_BYTES_PER_SEC: f32 = 44100.0 * 2.0 * 2.0; // 176,400 bytes/sec

struct SharedBufferState {
    data: VecDeque<u8>,
    total_ingested_bytes: u64,
    is_closed: bool,
    error: Option<String>,
}

#[derive(Clone)]
pub struct BufferStats {
    state: Arc<(Mutex<SharedBufferState>, Condvar, Condvar)>,
    bytes_per_sec: f32,
}

impl BufferStats {
    pub fn total_ingested_bytes(&self) -> u64 {
        let (lock, _, _) = &*self.state;
        lock.lock().unwrap().total_ingested_bytes
    }

    pub fn buffered_secs(&self) -> f32 {
        self.total_ingested_bytes() as f32 / self.bytes_per_sec
    }

    pub fn is_buffering(&self) -> bool {
        let (lock, _, _) = &*self.state;
        let state = lock.lock().unwrap();
        state.data.is_empty() && !state.is_closed
    }

    pub fn stream_error(&self) -> Option<String> {
        let (lock, _, _) = &*self.state;
        lock.lock().unwrap().error.clone()
    }
}

pub struct TrackedAudioBuffer {
    state: Arc<(Mutex<SharedBufferState>, Condvar, Condvar)>,
    capacity: usize,
    _producer_handle: Option<thread::JoinHandle<()>>,
}

impl TrackedAudioBuffer {
    #[allow(dead_code)]
    pub fn new<R: Read + Send + 'static>(reader: R, capacity: usize) -> (Self, BufferStats) {
        Self::new_with_rate(reader, capacity, PCM_BYTES_PER_SEC)
    }

    pub fn new_with_rate<R: Read + Send + 'static>(
        mut reader: R,
        capacity: usize,
        bytes_per_sec: f32,
    ) -> (Self, BufferStats) {
        let effective_capacity = capacity.max(1024);
        let state = Arc::new((
            Mutex::new(SharedBufferState {
                data: VecDeque::with_capacity(effective_capacity.min(64 * 1024)),
                total_ingested_bytes: 0,
                is_closed: false,
                error: None,
            }),
            Condvar::new(), // not_empty
            Condvar::new(), // not_full
        ));

        let state_producer = Arc::clone(&state);
        let producer_handle = thread::Builder::new()
            .name("audio-buffer-producer".to_string())
            .spawn(move || {
                let chunk_size = effective_capacity.min(16 * 1024);
                let mut chunk = vec![0u8; chunk_size];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => {
                            // EOF from source
                            let (lock, not_empty, _) = &*state_producer;
                            let mut s = lock.lock().unwrap();
                            s.is_closed = true;
                            not_empty.notify_all();
                            break;
                        }
                        Ok(n) => {
                            let mut written = 0;
                            while written < n {
                                let (lock, not_empty, not_full) = &*state_producer;
                                let mut s = lock.lock().unwrap();

                                while s.data.len() >= effective_capacity && !s.is_closed {
                                    s = not_full.wait(s).unwrap();
                                }

                                if s.is_closed {
                                    return;
                                }

                                let space = effective_capacity - s.data.len();
                                let to_write = (n - written).min(space);
                                s.data.extend(&chunk[written..written + to_write]);
                                s.total_ingested_bytes += to_write as u64;
                                written += to_write;
                                not_empty.notify_one();
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            // Pipe broken or closed
                            let (lock, not_empty, _) = &*state_producer;
                            let mut s = lock.lock().unwrap();
                            s.error = Some(error.to_string());
                            s.is_closed = true;
                            not_empty.notify_all();
                            break;
                        }
                    }
                }
            })
            .ok();

        let stats = BufferStats {
            state: Arc::clone(&state),
            bytes_per_sec: bytes_per_sec.max(1.0),
        };

        let buffer = Self {
            state,
            capacity: effective_capacity,
            _producer_handle: producer_handle,
        };

        (buffer, stats)
    }

    #[allow(dead_code)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl Read for TrackedAudioBuffer {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let (lock, not_empty, not_full) = &*self.state;
        let mut s = lock.lock().unwrap();

        loop {
            if !s.data.is_empty() {
                let to_read = buf.len().min(s.data.len());
                for b in &mut buf[..to_read] {
                    *b = s.data.pop_front().unwrap();
                }
                not_full.notify_one();
                return Ok(to_read);
            }

            if s.is_closed {
                return Ok(0); // EOF
            }

            s = not_empty.wait(s).unwrap();
        }
    }
}

impl Drop for TrackedAudioBuffer {
    fn drop(&mut self) {
        let (lock, not_empty, not_full) = &*self.state;
        let mut s = lock.lock().unwrap();
        s.is_closed = true;
        not_empty.notify_all();
        not_full.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Error};

    #[test]
    fn test_tracked_audio_buffer() {
        let fake_data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        let cursor = Cursor::new(fake_data.clone());
        let (mut buffer, stats) = TrackedAudioBuffer::new(cursor, 1024);

        let mut output = Vec::new();
        let mut chunk = [0u8; 256];
        loop {
            let n = buffer.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            output.extend_from_slice(&chunk[..n]);
        }

        assert_eq!(output, fake_data);
        assert_eq!(stats.total_ingested_bytes(), 10000);
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(Error::other("simulated stream interruption"))
        }
    }

    #[test]
    fn tracked_buffer_exposes_runtime_stream_errors() {
        let (_buffer, stats) = TrackedAudioBuffer::new(FailingReader, 1024);
        for _ in 0..100 {
            if stats.stream_error().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        assert_eq!(
            stats.stream_error().as_deref(),
            Some("simulated stream interruption")
        );
        assert!(!stats.is_buffering());
    }
}
