use crate::buffer::{BufferStats, DEFAULT_BUFFER_CAPACITY, TrackedAudioBuffer};
use crate::stream_reader::{
    AsyncRangeMediaSource, ByteCache, DecodedAudioSource, RangeBufferStats, SharedByteCache,
    UrlRefresher, is_url_expired,
};
use anyhow::{Context, Result};
use rodio::{OutputStreamHandle, Sink};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::info;

/// Converts YouTube's loudness_db (relative to -14 LUFS target) into a safe linear gain factor.
/// Only attenuates loud tracks or gently boosts quiet tracks (+3dB max) to prevent clipping.
pub fn loudness_db_to_gain(loudness_db: Option<f32>) -> f32 {
    let Some(db) = loudness_db else {
        return 1.0;
    };
    let clamped_db = (-db).clamp(-9.0, 3.0);
    10.0_f32.powf(clamped_db / 20.0).clamp(0.35, 1.4)
}

fn spawn_ytdlp_source(
    yt_dlp_path: &Path,
    stream_url: &str,
    request_headers: Option<&HashMap<String, String>>,
    start_offset_secs: f32,
    loudness_gain: f32,
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
        Ok(source) => source.with_loudness_gain(loudness_gain),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Failed to initialize the built-in audio decoder");
        }
    };
    Ok((source, stats, child))
}

struct PlaybackTiming {
    base_offset_secs: f32,
    start_instant: Option<Instant>,
    total_duration_secs: f32,
    is_paused: bool,
    consumed_samples: Option<Arc<AtomicU64>>,
    samples_per_sec: u64,
}

#[derive(Clone)]
pub struct AudioPlayer {
    stream_handle: Arc<Mutex<OutputStreamHandle>>,
    sink: Arc<Mutex<Sink>>,
    volume: Arc<Mutex<f32>>,
    fade_gain: Arc<Mutex<f32>>,
    loudness_gain: Arc<Mutex<f32>>,
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
            fade_gain: Arc::new(Mutex::new(1.0)),
            loudness_gain: Arc::new(Mutex::new(1.0)),
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
                consumed_samples: None,
                samples_per_sec: 0,
            })),
            buffer_stats: Arc::new(Mutex::new(None)),
            range_buffer_stats: Arc::new(Mutex::new(None)),
            current_stream_total_size: Arc::new(Mutex::new(None)),
            byte_cache: Arc::new(Mutex::new(None)),
        })
    }

    pub fn update_stream_handle(&self, new_handle: OutputStreamHandle) {
        let was_active = self.is_active() && self.has_track();
        let resume_pos = self.current_position();
        {
            let mut handle_guard = self.stream_handle.lock().unwrap();
            *handle_guard = new_handle;
            if let Ok(new_sink) = Sink::try_new(&handle_guard) {
                let eff_vol = self.effective_volume();
                new_sink.set_volume(eff_vol);
                *self.sink.lock().unwrap() = new_sink;
                info!("Audio Sink re-initialized with new output device stream handle.");
            }
        }
        if was_active {
            info!(
                "Hot-swapping active playback stream to new audio device at {:.1}s",
                resume_pos
            );
            let _ = self.seek_to(resume_pos);
        }
    }

    pub fn recreate_sink(&self) -> Result<()> {
        let handle = self.stream_handle.lock().unwrap();
        let new_sink = Sink::try_new(&handle).context("Failed to create new audio Sink")?;
        new_sink.set_volume(self.effective_volume());
        *self.sink.lock().unwrap() = new_sink;
        info!("Audio Sink recreated successfully.");
        Ok(())
    }

    pub fn set_loudness_db(&self, loudness_db: Option<f32>) {
        let gain = loudness_db_to_gain(loudness_db);
        *self.loudness_gain.lock().unwrap() = gain;
    }

    pub fn current_query(&self) -> Option<String> {
        self.current_query.lock().unwrap().clone()
    }

    pub fn has_track(&self) -> bool {
        self.current_stream_url.lock().unwrap().is_some()
            || self.current_query.lock().unwrap().is_some()
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
        info!(
            "Audio resumed directly from {:.1}s.",
            timing.base_offset_secs
        );
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

    fn byte_cache_for(&self, url: &str) -> SharedByteCache {
        let mut guard = self.byte_cache.lock().unwrap();
        if let Some((cached_url, cache)) = guard.as_ref()
            && cached_url == url
        {
            return Arc::clone(cache);
        }
        let cache: SharedByteCache = Arc::new(Mutex::new(ByteCache::default()));
        *guard = Some((url.to_string(), Arc::clone(&cache)));
        cache
    }

    #[allow(dead_code)]
    pub fn play_stream(&self, stream_url: &str, duration_secs: f32) -> Result<()> {
        self.play_stream_from(stream_url, 0.0, duration_secs)
    }

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

        let loudness_gain = *self.loudness_gain.lock().unwrap();
        info!(
            "Starting native audio stream from {:.1}s (loudness_gain={:.2})...",
            offset_secs, loudness_gain
        );

        let byte_cache = self.byte_cache_for(stream_url);
        let refresher = self.create_url_refresher();
        let range_result = AsyncRangeMediaSource::new_with_headers_and_size_and_refresher(
            stream_url,
            request_headers.as_ref(),
            None,
            byte_cache,
            refresher,
        )
        .context("Failed to open the audio stream with HTTP Range requests")
        .and_then(|range_source| {
            let stats = range_source.stats();
            DecodedAudioSource::new_seekable(range_source, offset_secs)
                .context("Failed to initialize or seek the audio stream")
                .map(|source| (source.with_loudness_gain(loudness_gain), stats))
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
                    loudness_gain,
                )?;
                (source, Some(stats), None, Some(child))
            }
        };

        if let Some(range_stats_ref) = &range_stats {
            *self.current_stream_total_size.lock().unwrap() = Some(range_stats_ref.total_size());
        } else {
            *self.current_stream_total_size.lock().unwrap() = None;
        }

        let consumed_samples = source.consumed_samples();
        let samples_per_sec = source.samples_per_second();

        *self.buffer_stats.lock().unwrap() = stats;
        *self.range_buffer_stats.lock().unwrap() = range_stats;

        *self.current_process.lock().unwrap() = child;
        *self.current_stream_url.lock().unwrap() = Some(stream_url.to_string());
        *self.current_stream_headers.lock().unwrap() = request_headers;

        {
            let mut timing = self.timing.lock().unwrap();
            timing.base_offset_secs = offset_secs;
            timing.start_instant = Some(Instant::now());
            timing.total_duration_secs = duration_secs;
            timing.is_paused = false;
            timing.consumed_samples = Some(consumed_samples);
            timing.samples_per_sec = samples_per_sec;
        }

        let sink_lock = self.sink.lock().unwrap();
        sink_lock.append(source);
        sink_lock.set_volume(self.effective_volume());
        sink_lock.play();
        info!("Audio playback successfully started.");

        Ok(())
    }

    /// Internal seamless seek that prepares the new range source first before swapping sink
    pub fn seek_stream_to(
        &self,
        stream_url: &str,
        request_headers: Option<HashMap<String, String>>,
        known_total_size: Option<u64>,
        offset_secs: f32,
        duration_secs: f32,
    ) -> Result<()> {
        let loudness_gain = *self.loudness_gain.lock().unwrap();
        info!("Seeking native audio stream to {:.1}s...", offset_secs);

        let byte_cache = self.byte_cache_for(stream_url);
        let refresher = self.create_url_refresher();
        let range_result = AsyncRangeMediaSource::new_with_headers_and_size_and_refresher(
            stream_url,
            request_headers.as_ref(),
            known_total_size,
            byte_cache,
            refresher,
        )
        .context("Failed to open the audio stream with HTTP Range requests")
        .and_then(|range_source| {
            let stats = range_source.stats();
            DecodedAudioSource::new_seekable(range_source, offset_secs)
                .context("Failed to initialize or seek the audio stream")
                .map(|source| (source.with_loudness_gain(loudness_gain), stats))
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
                    loudness_gain,
                )?;
                (source, Some(stats), None, Some(child))
            }
        };

        if let Some(range_stats_ref) = &range_stats {
            *self.current_stream_total_size.lock().unwrap() = Some(range_stats_ref.total_size());
        }

        if let Ok(mut proc_lock) = self.current_process.lock() {
            if let Some(mut old_child) = proc_lock.take() {
                let _ = old_child.kill();
                let _ = old_child.wait();
            }
            *proc_lock = child;
        }

        let consumed_samples = source.consumed_samples();
        let samples_per_sec = source.samples_per_second();

        *self.buffer_stats.lock().unwrap() = stats;
        *self.range_buffer_stats.lock().unwrap() = range_stats;

        {
            let mut timing = self.timing.lock().unwrap();
            timing.base_offset_secs = offset_secs;
            timing.start_instant = Some(Instant::now());
            timing.total_duration_secs = duration_secs;
            timing.is_paused = false;
            timing.consumed_samples = Some(consumed_samples);
            timing.samples_per_sec = samples_per_sec;
        }

        let sink_lock = self.sink.lock().unwrap();
        sink_lock.clear();
        sink_lock.append(source);
        sink_lock.set_volume(self.effective_volume());
        sink_lock.play();

        info!("Audio seek to {:.1}s successfully applied.", offset_secs);
        Ok(())
    }

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
        let current = self.current_position();
        let mut timing = self.timing.lock().unwrap();
        timing.base_offset_secs = current;
        timing.start_instant = None;
        if let Some(counter) = &timing.consumed_samples {
            counter.store(0, Ordering::Relaxed);
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
            && let Some(mut child) = proc_lock.take()
        {
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
            timing.consumed_samples = None;
            timing.samples_per_sec = 0;
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

    /// Calculate the exact playback position in seconds using consumed audio samples,
    /// completely immune to wall-clock drift during network buffering stalls.
    pub fn current_position(&self) -> f32 {
        let timing = self.timing.lock().unwrap();
        let elapsed = if let Some(counter) = &timing.consumed_samples
            && timing.samples_per_sec > 0
        {
            let samples = counter.load(Ordering::Relaxed);
            (samples as f64 / timing.samples_per_sec as f64) as f32
        } else if let Some(instant) = timing.start_instant {
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
        let current = self.current_position();
        let timing = self.timing.lock().unwrap();
        let range_stats = self.range_buffer_stats.lock().unwrap();
        if let Some(stats) = &*range_stats {
            let buffered = stats
                .buffered_position_secs(timing.total_duration_secs)
                .max(current);
            if timing.total_duration_secs > 0.0 {
                return buffered.min(timing.total_duration_secs);
            }
            return buffered;
        }
        let stats_guard = self.buffer_stats.lock().unwrap();
        if let Some(stats) = &*stats_guard {
            let buffered = (timing.base_offset_secs + stats.buffered_secs()).max(current);
            if timing.total_duration_secs > 0.0 {
                buffered.min(timing.total_duration_secs)
            } else {
                buffered
            }
        } else {
            current
        }
    }

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

    pub fn is_active(&self) -> bool {
        let timing = self.timing.lock().unwrap();
        timing.start_instant.is_some() && !timing.is_paused
    }

    pub fn has_ended(&self) -> bool {
        if self.stream_error().is_some() {
            return false;
        }
        let (playback_active, reached_duration) = {
            let timing = self.timing.lock().unwrap();
            let active = !timing.is_paused && timing.start_instant.is_some();
            let reached = timing.total_duration_secs > 0.0
                && if let Some(counter) = &timing.consumed_samples
                    && timing.samples_per_sec > 0
                {
                    let samples = counter.load(Ordering::Relaxed);
                    let elapsed = (samples as f64 / timing.samples_per_sec as f64) as f32;
                    (timing.base_offset_secs + elapsed) >= timing.total_duration_secs
                } else if let Some(instant) = timing.start_instant {
                    (timing.base_offset_secs + instant.elapsed().as_secs_f32())
                        >= timing.total_duration_secs
                } else {
                    false
                };
            (active, reached)
        };
        if !playback_active {
            return false;
        }
        let stream_finished = if let Ok(mut proc_lock) = self.current_process.lock() {
            match proc_lock.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(Some(_status))),
                None => true,
            }
        } else {
            false
        };
        (stream_finished && self.sink.lock().unwrap().empty())
            || (reached_duration && !self.is_buffering())
    }

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

    fn effective_volume(&self) -> f32 {
        let vol = *self.volume.lock().unwrap();
        let fade = *self.fade_gain.lock().unwrap();
        (vol * fade).clamp(0.0, 2.0)
    }

    pub fn set_volume(&self, vol: f32) {
        let vol = vol.clamp(0.0, 2.0);
        *self.volume.lock().unwrap() = vol;
        let eff = self.effective_volume();
        self.sink.lock().unwrap().set_volume(eff);
        info!("Volume set to {:.0}%", vol * 100.0);
    }

    pub fn set_fade_gain(&self, fade: f32) {
        let fade = fade.clamp(0.0, 1.0);
        *self.fade_gain.lock().unwrap() = fade;
        let eff = self.effective_volume();
        self.sink.lock().unwrap().set_volume(eff);
    }

    #[allow(dead_code)]
    pub fn volume(&self) -> f32 {
        *self.volume.lock().unwrap()
    }
}