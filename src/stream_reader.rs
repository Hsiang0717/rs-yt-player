use std::io::{Read, Seek, SeekFrom};
use tracing::debug;

#[allow(dead_code)]
const DEFAULT_CHUNK_SIZE: usize = 512 * 1024; // 512 KB

/// A stream reader that implements `Read + Seek` using HTTP Range requests.
#[allow(dead_code)]
pub struct HttpStreamReader {
    client: reqwest::blocking::Client,
    url: String,
    total_size: u64,
    pos: u64,
    buffer: Vec<u8>,
    buffer_start: u64,
    chunk_size: usize,
}

#[allow(dead_code)]
impl HttpStreamReader {
    pub fn new(url: &str) -> std::io::Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

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
                if let Some(total_str) = range_str.split('/').last() {
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
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to probe audio URL, status: {}", resp.status()),
            ));
        };

        tracing::info!("HttpStreamReader initialized: probe_status = {:?}, total_size = {} bytes", resp.status(), total_size);

        let mut reader = Self {
            client,
            url: url.to_string(),
            total_size,
            pos: 0,
            buffer: Vec::new(),
            buffer_start: 0,
            chunk_size: DEFAULT_CHUNK_SIZE,
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

        let resp = self
            .client
            .get(&self.url)
            .header("Range", range_header)
            .send()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::ConnectionReset, e))?;

        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("HTTP range request failed with status: {}", resp.status()),
            ));
        }

        let bytes = resp
            .bytes()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        self.buffer = bytes.to_vec();
        self.buffer_start = start;
        Ok(())
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

        tracing::info!("Seek: {:?} -> new_pos: {}, total_size: {}", pos, new_pos, self.total_size);
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

pub struct PcmSource<R: std::io::Read + Send + 'static> {
    reader: R,
    sample_rate: u32,
    channels: u16,
}

impl<R: std::io::Read + Send + 'static> PcmSource<R> {
    pub fn new(reader: R, sample_rate: u32, channels: u16) -> Self {
        Self {
            reader,
            sample_rate,
            channels,
        }
    }
}

impl<R: std::io::Read + Send + 'static> Iterator for PcmSource<R> {
    type Item = i16;
    fn next(&mut self) -> Option<i16> {
        let mut buf = [0u8; 2];
        if self.reader.read_exact(&mut buf).is_ok() {
            Some(i16::from_le_bytes(buf))
        } else {
            None
        }
    }
}

impl<R: std::io::Read + Send + 'static> rodio::Source for PcmSource<R> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractor::Extractor;

    #[tokio::test]
    async fn test_ffmpeg_pcm_source() {
        let extractor = Extractor::new().await.unwrap();
        let meta = extractor
            .extract_audio("Rick Astley Never Gonna Give You Up")
            .await
            .unwrap();
        println!("Test meta title: {}", meta.title);

        let mut child = std::process::Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-reconnect", "1",
                "-reconnect_streamed", "1",
                "-reconnect_delay_max", "5",
                "-i", &meta.stream_url,
                "-f", "s16le",
                "-acodec", "pcm_s16le",
                "-ac", "2",
                "-ar", "44100",
                "pipe:1",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("Failed to spawn ffmpeg");

        let stdout = child.stdout.take().expect("Failed to open ffmpeg stdout");
        let reader = std::io::BufReader::with_capacity(64 * 1024, stdout);
        let mut pcm_source = PcmSource::new(reader, 44100, 2);

        // Read 100 samples to verify streaming works
        let mut sample_count = 0;
        for _ in 0..100 {
            if let Some(_sample) = pcm_source.next() {
                sample_count += 1;
            }
        }

        println!("SUCCESS! Read {} audio samples from ffmpeg stream pipe!", sample_count);
        assert_eq!(sample_count, 100);
        let _ = child.kill();
    }
}
