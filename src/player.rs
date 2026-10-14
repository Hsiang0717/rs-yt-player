use crate::buffer::{BufferStats, DEFAULT_BUFFER_CAPACITY, TrackedAudioBuffer};
use crate::stream_reader::{
    is_url_expired, AsyncRangeMediaSource, ByteCache, DecodedAudioSource, RangeBufferStats,
    SharedByteCache, UrlRefresher,
};
use anyhow::{Context, Result};
use rodio::{OutputStreamHandle, Sink};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::info;

fn spawn_ytdlp_source(
    yt_dlp_path: &Path,
    stream_url: &str,
    request_headers: Option<&HashMap<String, String>>,
    start_offset_secs: f32,
) -> Result<(DecodedAudioSource, BufferStats, Child)> {
    let mut cmd = Command::new(yt_dlp_path);
    cmd.args([
        "--no-warnings",
        "--quiet",
        "--no-part",
        "--http-chunk-size",
        "10M",
    ]);
    if let Some(headers) = request_headers {
        for (name, value) in headers {
            cmd.arg("--add-headers").arg(format!("{name}:{value}"));
        }
    }
    cmd.args(["-o", "-", stream_url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().context("Failed to spawn yt-dlp downloader")?;
    let stdout = child
        .stdout
        .take()
        .context("Failed to capture yt-dlp stdout pipe")?;
    let (tracked_buf, stats) =
        TrackedAudioBuffer::new_with_rate(stdout, DEFAULT_BUFFER_CAPACITY, 16_000.0);
    let source = match DecodedAudioSource::new(tracked_buf, start_offset_secs) {
        Ok(source) => source,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Failed to initialize the built-in AAC decoder");
        }
    };
    Ok((source, stats, child))
}

struct PlaybackTiming {
    base_offset_secs: f32,
    start_instant: Option<Instant>,
    total_duration_secs: f32,
    is_paused: bool,
}

#[derive(Clone)]
pub struct AudioPlayer {
    stream_handle: Arc<Mutex<OutputStreamHandle>>,
    sink: Arc<Mutex<Sink>>,
    volume: Arc<Mutex<f32>>,
    yt_dlp_path: std::path::PathBuf,
    current_process: Arc<Mutex<Option<Child>>>,
    current_stream_url: Arc<Mutex<Option<String>>>,
    current_stream_headers: Arc<Mutex<Option<HashMap<String, String>>>>,
    current_query: Arc<Mutex<Option<String>>>,
    timing: Arc<Mutex<PlaybackTiming>>,
    buffer_stats: Arc<Mutex<Option<BufferStats>>>,
    range_buffer_stats: Arc<Mutex<Option<RangeBufferStats>>>,
    current_stream_total_size: Arc<Mutex<Option<u64>>>,
    byte_cache: Arc<Mutex<Option<(String, SharedByteCache)>>>,
}

impl AudioPlayer {
    pub fn new(
        stream_handle: &OutputStreamHandle,
        yt_dlp_path: std::path::PathBuf,
    ) -> Result<Self> {
        let sink = Sink::try_new(stream_handle).context("Failed to create audio Sink")?;

        Ok(Self {
            stream_handle: Arc::new(Mutex::new(stream_handle.clone())),
            sink: Arc::new(Mutex::new(sink)),
            volume: Arc::new(Mutex::new(1.0)),
            yt_dlp_path,
            current_process: Arc::new(Mutex::new(None)),
            current_stream_url: Arc::new(Mutex::new(None)),
            current_stream_headers: Arc::new(Mutex::new(None)),
            current_query: Arc::new(Mutex::new(None)),
            timing: Arc::new(Mutex::new(PlaybackTiming {
                base_offset_secs: 0.0,
                start_instant: None,
                total_duration_secs: 0.0,
                is_paused: false,
            })),
            buffer_stats: Arc::new(Mutex::new(None)),
            range_buffer_stats: Arc::new(Mutex::new(None)),
            current_stream_total_size: Arc::new(Mutex::new(None)),
            byte_cache: Arc::new(Mutex::new(None)),
        })
    }

    pub fn update_stream_handle(&self, new_handle: OutputStreamHandle) {
        let mut handle_guard = self.stream_handle.lock().unwrap();
        *handle_guard = new_handle;
        if let Ok(new_sink) = Sink::try_new(&handle_guard) {
            let vol = *self.volume.lock().unwrap();
            new_sink.set_volume(vol);
            *self.sink.lock().unwrap() = new_sink;
            info!("Audio Sink successfully re-initialized with new audio output stream handle.");
        }
    }

    pub fn recreate_sink(&self) -> Result<()> {
        let handle = self.stream_handle.lock().unwrap();
        let new_sink = Sink::try_new(&handle).context("Failed to create new audio Sink")?;
        let vol = *self.volume.lock().unwrap();
        new_sink.set_volume(vol);
        *self.sink.lock().unwrap() = new_sink;
        info!("Audio Sink recreated successfully.");
        Ok(())
    }

    pub fn current_query(&self) -> Option<String> {
        self.current_query.lock().unwrap().clone()
    }

    pub fn has_track(&self) -> bool {
        self.current_stream_url.lock().unwrap().is_some() || self.current_query.lock().unwrap().is_some()
    }

    pub fn is_stream_expired_or_stale(&self) -> bool {
        if self.stream_error().is_some() {
            return true;
        }
        self.current_stream_url
            .lock()
            .unwrap()
            .as_deref()
            .map(is_url_expired)
            .unwrap_or(false)
    }

    pub fn resume_direct(&self) {
        self.sink.lock().unwrap().play();
        let mut timing = self.timing.lock().unwrap();
        timing.start_instant = Some(Instant::now());
        timing.is_paused = false;
        info!("Audio resumed directly from {:.1}s.", timing.base_offset_secs);
    }

    pub fn set_current_query(&self, query: Option<String>) {
        *self.current_query.lock().unwrap() = query;
    }

    fn create_url_refresher(&self) -> Option<UrlRefresher> {
        let yt_dlp = self.yt_dlp_path.clone();
        let query_opt = self.current_query.lock().unwrap().clone();
        query_opt.map(|query| {
            Arc::new(move || crate::extractor::extract_stream_url_sync(&yt_dlp, &query))
                as UrlRefresher
        })
    }

    /// Returns the downloaded-bytes cache for this URL, creating a fresh one when the URL changes.
    fn byte_cache_for(&self, url: &str) -> SharedByteCache {
        let mut guard = self.byte_cache.lock().unwrap();
        if let Some((cached_url, cache)) = guard.as_ref()
            && cached_url == url {
                return Arc::clone(cache);
            }
        let cache: SharedByteCache = Arc::new(Mutex::new(ByteCache::default()));
        *guard = Some((url.to_string(), Arc::clone(&cache)));
        cache
    }

    /// Stream audio directly from an HTTP URL starting at base_offset (default 0s).
    #[allow(dead_code)]
    pub fn play_stream(&self, stream_url: &str, duration_secs: f32) -> Result<()> {
        self.play_stream_from(stream_url, 0.0, duration_secs)
    }

    /// Stream audio starting from a specific second offset.
    #[allow(dead_code)]
    pub fn play_stream_from(
        &self,
        stream_url: &str,
        offset_secs: f32,
        duration_secs: f32,
    ) -> Result<()> {
        self.play_stream_from_with_headers(stream_url, None, offset_secs, duration_secs)
    }

    pub fn play_stream_from_with_headers(
        &self,
        stream_url: &str,
        request_headers: Option<HashMap<String, String>>,
        offset_secs: f32,
        duration_secs: f32,
    ) -> Result<()> {
        self.stop();

        info!(
            "Starting native AAC audio stream from {:.1}s...",
            offset_secs
        );

        // Prefer a seekable HTTP source for both normal playback and long-offset starts.
        // Besides making seeks cheap, this avoids depending on MP4 pipe probing for long media.
        let byte_cache = self.byte_cache_for(stream_url);
        let refresher = self.create_url_refresher();
        let range_result = AsyncRangeMediaSource::new_with_headers_and_size_and_refresher(
            stream_url,
            request_headers.as_ref(),
            None,
            byte_cache,
            refresher,
        )
        .context("Failed to open the AAC stream with HTTP Range requests")
        .and_then(|range_source| {
            let stats = range_source.stats();
            DecodedAudioSource::new_seekable(range_source, offset_secs)
                .context("Failed to initialize or seek the AAC stream")
                .map(|source| (source, stats))
        });
        let (source, stats, range_stats, child) = match range_result {
            Ok((source, stats)) => (source, None, Some(stats), None),
            Err(error) => {
                tracing::warn!(
                    "HTTP Range playback is unavailable ({:#}); falling back to sequential decoding.",
                    error
                );
                let (source, stats, child) = spawn_ytdlp_source(
                    &self.yt_dlp_path,
                    stream_url,
                    request_headers.as_ref(),
                    offset_secs,
                )?;
                (source, Some(stats), None, Some(child))
            }
        };

        if let Some(range_stats_ref) = &range_stats {
            *self.current_stream_total_size.lock().unwrap() = Some(range_stats_ref.total_size());
        } else {
            *self.current_stream_total_size.lock().unwrap() = None;
        }

        *self.buffer_stats.lock().unwrap() = stats;
        *self.range_buffer_stats.lock().unwrap() = range_stats;

        // Store the downloader process and current URL
        *self.current_process.lock().unwrap() = child;
        *self.current_stream_url.lock().unwrap() = Some(stream_url.to_string());
        *self.current_stream_headers.lock().unwrap() = request_headers;

        // Update timing tracker
        {
            let mut timing = self.timing.lock().unwrap();
            timing.base_offset_secs = offset_secs;
            timing.start_instant = Some(Instant::now());
            timing.total_duration_secs = duration_secs;
            timing.is_paused = false;
        }

        let sink_lock = self.sink.lock().unwrap();
        sink_lock.append(source);
        let vol = *self.volume.lock().unwrap();
        sink_lock.set_volume(vol);
        sink_lock.play();
        info!("Audio playback successfully started.");

        Ok(())
    }

    /// Internal seamless seek that prepares the new AAC range source first before swapping sink
    pub fn seek_stream_to(
        &self,
        stream_url: &str,
        request_headers: Option<HashMap<String, String>>,
        known_total_size: Option<u64>,
        offset_secs: f32,
        duration_secs: f32,
    ) -> Result<()> {
        info!(
            "Seeking native AAC audio stream to {:.1}s...",
            offset_secs
        );

        let byte_cache = self.byte_cache_for(stream_url);
        let refresher = self.create_url_refresher();
        let range_result = AsyncRangeMediaSource::new_with_headers_and_size_and_refresher(
            stream_url,
            request_headers.as_ref(),
            known_total_size,
            byte_cache,
            refresher,
        )
        .context("Failed to open the AAC stream with HTTP Range requests")
        .and_then(|range_source| {
            let stats = range_source.stats();
            DecodedAudioSource::new_seekable(range_source, offset_secs)
                .context("Failed to initialize or seek the AAC stream")
                .map(|source| (source, stats))
        });

        let (source, stats, range_stats, child) = match range_result {
            Ok((source, stats)) => (source, None, Some(stats), None),
            Err(error) => {
                tracing::warn!(
                    "HTTP Range playback is unavailable ({:#}); falling back to sequential decoding.",
                    error
                );
                let (source, stats, child) = spawn_ytdlp_source(
                    &self.yt_dlp_path,
                    stream_url,
                    request_headers.as_ref(),
                    offset_secs,
                )?;
                (source, Some(stats), None, Some(child))
            }
        };

        if let Some(range_stats_ref) = &range_stats {
            *self.current_stream_total_size.lock().unwrap() = Some(range_stats_ref.total_size());
        }

        // Clean up previous fallback downloader process if any
        if let Ok(mut proc_lock) = self.current_process.lock() {
            if let Some(mut old_child) = proc_lock.take() {
                let _ = old_child.kill();
                let _ = old_child.wait();
            }
            *proc_lock = child;
        }

        // Atomically update buffer stats without setting to None in-between
        *self.buffer_stats.lock().unwrap() = stats;
        *self.range_buffer_stats.lock().unwrap() = range_stats;

        // Atomically update timing tracker without zeroing it out!
        {
            let mut timing = self.timing.lock().unwrap();
            timing.base_offset_secs = offset_secs;
            timing.start_instant = Some(Instant::now());
            timing.total_duration_secs = duration_secs;
            timing.is_paused = false;
        }

        // Atomically replace audio in sink
        let sink_lock = self.sink.lock().unwrap();
        sink_lock.clear();
        sink_lock.append(source);
        let vol = *self.volume.lock().unwrap();
        sink_lock.set_volume(vol);
        sink_lock.play();

        info!("Audio seek to {:.1}s successfully applied.", offset_secs);
        Ok(())
    }

    /// Seek to a specific target second in the currently playing track.
    pub fn seek_to(&self, target_secs: f32) -> Result<()> {
        let (stream_url, request_headers, total_size) = {
            let url_guard = self.current_stream_url.lock().unwrap();
            let headers = self.current_stream_headers.lock().unwrap().clone();
            let total_size = *self.current_stream_total_size.lock().unwrap();
            (url_guard.clone(), headers, total_size)
        };

        if let Some(url) = stream_url {
            let total_dur = self.timing.lock().unwrap().total_duration_secs;
            self.seek_stream_to(&url, request_headers, total_size, target_secs, total_dur)?;
        }

        Ok(())
    }

    pub fn pause(&self) {
        self.sink.lock().unwrap().pause();
        let mut timing = self.timing.lock().unwrap();
        if let Some(instant) = timing.start_instant.take() {
            timing.base_offset_secs += instant.elapsed().as_secs_f32();
        }
        timing.is_paused = true;
        info!("Audio paused at {:.1}s.", timing.base_offset_secs);
    }

    pub fn resume(&self) {
        let is_expired = self
            .current_stream_url
            .lock()
            .unwrap()
            .as_deref()
            .map(is_url_expired)
            .unwrap_or(false);

        if is_expired {
            let query_opt = self.current_query.lock().unwrap().clone();
            if let Some(query) = query_opt
                && let Ok((new_url, new_headers)) =
                    crate::extractor::extract_stream_url_sync(&self.yt_dlp_path, &query)
                {
                    info!(
                        "Proactively refreshed expired YouTube stream URL on resume; continuing playback."
                    );
                    let current_pos = self.current_position();
                    let total_dur = self.total_duration();
                    if let Ok(()) =
                        self.seek_stream_to(&new_url, new_headers, None, current_pos, total_dur)
                    {
                        return;
                    }
                }
        }

        self.sink.lock().unwrap().play();
        let mut timing = self.timing.lock().unwrap();
        timing.start_instant = Some(Instant::now());
        timing.is_paused = false;
        info!("Audio resumed from {:.1}s.", timing.base_offset_secs);
    }

    pub fn stop(&self) {
        if let Ok(mut proc_lock) = self.current_process.lock()
            && let Some(mut child) = proc_lock.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        if let Ok(sink_lock) = self.sink.lock() {
            sink_lock.clear();
        }
        if let Ok(mut timing) = self.timing.lock() {
            timing.base_offset_secs = 0.0;
            timing.start_instant = None;
            timing.is_paused = false;
        }
        if let Ok(mut stats_lock) = self.buffer_stats.lock() {
            *stats_lock = None;
        }
        if let Ok(mut stats_lock) = self.range_buffer_stats.lock() {
            *stats_lock = None;
        }
        if let Ok(mut size_lock) = self.current_stream_total_size.lock() {
            *size_lock = None;
        }
        info!("Audio stopped.");
    }

    pub fn is_paused(&self) -> bool {
        self.timing.lock().unwrap().is_paused
    }

    /// Calculate the current elapsed playback position in seconds.
    pub fn current_position(&self) -> f32 {
        let timing = self.timing.lock().unwrap();
        let elapsed = if let Some(instant) = timing.start_instant {
            instant.elapsed().as_secs_f32()
        } else {
            0.0
        };
        let current = timing.base_offset_secs + elapsed;
        if timing.total_duration_secs > 0.0 {
            current.min(timing.total_duration_secs)
        } else {
            current
        }
    }

    /// Calculate the current buffered position in seconds.
    pub fn buffered_position(&self) -> f32 {
        let timing = self.timing.lock().unwrap();
        let range_stats = self.range_buffer_stats.lock().unwrap();
        if let Some(stats) = &*range_stats {
            let buffered = stats.buffered_position_secs(timing.total_duration_secs);
            let elapsed = timing
                .start_instant
                .map(|instant| instant.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            let current = timing.base_offset_secs + elapsed;
            let current = if timing.total_duration_secs > 0.0 {
                current.min(timing.total_duration_secs)
            } else {
                current
            };
            let buffered = buffered.max(current);
            if timing.total_duration_secs > 0.0 {
                return buffered.min(timing.total_duration_secs);
            }
            return buffered;
        }
        let stats_guard = self.buffer_stats.lock().unwrap();
        if let Some(stats) = &*stats_guard {
            let buffered = timing.base_offset_secs + stats.buffered_secs();
            if timing.total_duration_secs > 0.0 {
                buffered.min(timing.total_duration_secs)
            } else {
                buffered
            }
        } else {
            let elapsed = timing
                .start_instant
                .map(|instant| instant.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            let buffered = timing.base_offset_secs + elapsed;
            if timing.total_duration_secs > 0.0 {
                buffered.min(timing.total_duration_secs)
            } else {
                buffered
            }
        }
    }

    /// Calculate the current buffered progress ratio (0.0 to 1.0).
    pub fn buffer_ratio(&self) -> f32 {
        let dur = self.total_duration();
        if dur > 0.0 {
            (self.buffered_position() / dur).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    pub fn total_duration(&self) -> f32 {
        self.timing.lock().unwrap().total_duration_secs
    }

    pub fn is_buffering(&self) -> bool {
        if let Some(stats) = self.range_buffer_stats.lock().unwrap().as_ref() {
            return stats.is_buffering();
        }
        self.buffer_stats
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(BufferStats::is_buffering)
    }

    pub fn stream_error(&self) -> Option<String> {
        if let Some(stats) = self.range_buffer_stats.lock().unwrap().as_ref() {
            return stats.stream_error();
        }
        self.buffer_stats
            .lock()
            .unwrap()
            .as_ref()
            .and_then(BufferStats::stream_error)
    }

    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        let timing = self.timing.lock().unwrap();
        timing.start_instant.is_some() && !timing.is_paused
    }

    /// Check if the current audio track has finished playing
    pub fn has_ended(&self) -> bool {
        if self.stream_error().is_some() {
            return false;
        }
        let playback_active = {
            let timing = self.timing.lock().unwrap();
            !timing.is_paused && timing.start_instant.is_some()
        };
        if !playback_active {
            return false;
        }
        // Do not advance from metadata duration alone: a slow or stalled long stream can
        // outlast its wall-clock estimate. Wait for the decoder and queued audio to finish.
        let stream_finished = if let Ok(mut proc_lock) = self.current_process.lock() {
            match proc_lock.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(Some(_status))),
                None => true,
            }
        } else {
            false
        };
        if stream_finished && self.sink.lock().unwrap().empty() {
            return true;
        }

        false
    }

    /// Fast-forward or rewind by delta seconds (e.g. +5.0 or -5.0)
    #[allow(dead_code)]
    pub fn seek_relative(&self, delta_secs: f32) -> Result<()> {
        let cur = self.current_position();
        let target = (cur + delta_secs).max(0.0);
        self.seek_to(target)
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.sink.lock().unwrap().empty()
    }

    pub fn set_volume(&self, vol: f32) {
        let vol = vol.clamp(0.0, 2.0);
        *self.volume.lock().unwrap() = vol;
        self.sink.lock().unwrap().set_volume(vol);
        info!("Volume set to {:.0}%", vol * 100.0);
    }

    #[allow(dead_code)]
    pub fn volume(&self) -> f32 {
        *self.volume.lock().unwrap()
    }
}
