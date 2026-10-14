use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use tracing::debug;

#[allow(dead_code)]
pub const DEFAULT_CHUNK_SIZE: usize = 512 * 1024; // 512 KB

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

pub(crate) fn build_blocking_client(
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
    pub(crate) url: String,
    pub(crate) total_size: u64,
    pub(crate) pos: u64,
    pub(crate) buffer: Vec<u8>,
    pub(crate) buffer_start: u64,
    pub(crate) chunk_size: usize,
    pub(crate) url_refresher: Option<UrlRefresher>,
    pub(crate) request_headers: Option<HashMap<String, String>>,
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

    pub fn total_size(&self) -> u64 {
        self.total_size
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
