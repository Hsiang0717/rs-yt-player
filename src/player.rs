use crate::buffer::{BufferStats, TrackedAudioBuffer, DEFAULT_BUFFER_CAPACITY};
use crate::stream_reader::PcmSource;
use anyhow::{Context, Result};
use rodio::{OutputStreamHandle, Sink};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::info;

struct PlaybackTiming {
    base_offset_secs: f32,
    start_instant: Option<Instant>,
    total_duration_secs: f32,
    is_paused: bool,
}

#[derive(Clone)]
pub struct AudioPlayer {
    sink: Arc<Mutex<Sink>>,
    volume: Arc<Mutex<f32>>,
    current_process: Arc<Mutex<Option<Child>>>,
    current_stream_url: Arc<Mutex<Option<String>>>,
    timing: Arc<Mutex<PlaybackTiming>>,
    buffer_stats: Arc<Mutex<Option<BufferStats>>>,
}

impl AudioPlayer {
    pub fn new(stream_handle: &OutputStreamHandle) -> Result<Self> {
        let sink = Sink::try_new(stream_handle)
            .context("Failed to create audio Sink")?;

        Ok(Self {
            sink: Arc::new(Mutex::new(sink)),
            volume: Arc::new(Mutex::new(1.0)),
            current_process: Arc::new(Mutex::new(None)),
            current_stream_url: Arc::new(Mutex::new(None)),
            timing: Arc::new(Mutex::new(PlaybackTiming {
                base_offset_secs: 0.0,
                start_instant: None,
                total_duration_secs: 0.0,
                is_paused: false,
            })),
            buffer_stats: Arc::new(Mutex::new(None)),
        })
    }

    /// Stream audio directly from an HTTP URL starting at base_offset (default 0s).
    pub fn play_stream(&self, stream_url: &str, duration_secs: f32) -> Result<()> {
        self.play_stream_from(stream_url, 0.0, duration_secs)
    }

    /// Stream audio starting from a specific second offset.
    pub fn play_stream_from(&self, stream_url: &str, offset_secs: f32, duration_secs: f32) -> Result<()> {
        self.stop();

        info!("Starting ffmpeg audio stream pipeline from {:.1}s...", offset_secs);

        let offset_str = format!("{:.3}", offset_secs);
        let mut args = vec![
            "-nostdin",
        ];

        // Seek before input for lightning-fast server-side keyframe seek
        if offset_secs > 0.0 {
            args.push("-ss");
            args.push(&offset_str);
        }

        args.extend_from_slice(&[
            "-reconnect", "1",
            "-reconnect_streamed", "1",
            "-reconnect_delay_max", "5",
            "-i", stream_url,
            "-f", "s16le",
            "-acodec", "pcm_s16le",
            "-ac", "2",
            "-ar", "44100",
            "pipe:1",
        ]);

        let ffmpeg_bin = if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let candidate1 = exe_dir.join("bin").join("ffmpeg.exe");
                if candidate1.exists() {
                    candidate1
                } else {
                    let candidate2 = exe_dir.join("ffmpeg.exe");
                    if candidate2.exists() {
                        candidate2
                    } else {
                        std::path::PathBuf::from("ffmpeg")
                    }
                }
            } else {
                std::path::PathBuf::from("ffmpeg")
            }
        } else {
            std::path::PathBuf::from("ffmpeg")
        };

        let mut cmd = Command::new(ffmpeg_bin);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd
            .spawn()
            .context("Failed to spawn ffmpeg pipeline")?;

        let stdout = child.stdout.take().context("Failed to capture ffmpeg stdout pipe")?;
        let (tracked_buf, stats) = TrackedAudioBuffer::new(stdout, DEFAULT_BUFFER_CAPACITY);
        *self.buffer_stats.lock().unwrap() = Some(stats);
        let source = PcmSource::new(tracked_buf, 44100, 2);

        // Store child handle and current URL
        *self.current_process.lock().unwrap() = Some(child);
        *self.current_stream_url.lock().unwrap() = Some(stream_url.to_string());

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

    /// Seek to a specific target second in the currently playing track.
    pub fn seek_to(&self, target_secs: f32) -> Result<()> {
        let stream_url = {
            let url_guard = self.current_stream_url.lock().unwrap();
            url_guard.clone()
        };

        if let Some(url) = stream_url {
            let total_dur = self.timing.lock().unwrap().total_duration_secs;
            self.play_stream_from(&url, target_secs, total_dur)?;
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
        self.sink.lock().unwrap().play();
        let mut timing = self.timing.lock().unwrap();
        timing.start_instant = Some(Instant::now());
        timing.is_paused = false;
        info!("Audio resumed from {:.1}s.", timing.base_offset_secs);
    }

    pub fn stop(&self) {
        if let Ok(mut proc_lock) = self.current_process.lock() {
            if let Some(mut child) = proc_lock.take() {
                let _ = child.kill();
            }
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
        let stats_guard = self.buffer_stats.lock().unwrap();
        if let Some(stats) = &*stats_guard {
            let buffered = timing.base_offset_secs + stats.buffered_secs();
            if timing.total_duration_secs > 0.0 {
                buffered.min(timing.total_duration_secs)
            } else {
                buffered
            }
        } else {
            timing.base_offset_secs
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

    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        let timing = self.timing.lock().unwrap();
        timing.start_instant.is_some() && !timing.is_paused
    }

    /// Check if the current audio track has finished playing
    pub fn has_ended(&self) -> bool {
        let timing = self.timing.lock().unwrap();
        if timing.is_paused || timing.start_instant.is_none() {
            return false;
        }
        let elapsed = if let Some(instant) = timing.start_instant {
            instant.elapsed().as_secs_f32()
        } else {
            0.0
        };
        let current = timing.base_offset_secs + elapsed;

        // If total duration is known, check if we reached the end (within 0.5s)
        if timing.total_duration_secs > 3.0 && current >= (timing.total_duration_secs - 0.5) {
            return true;
        }

        // Only consider sink.empty() if the ffmpeg child process has actually EXITED!
        if let Ok(mut proc_lock) = self.current_process.lock() {
            if let Some(child) = proc_lock.as_mut() {
                if let Ok(Some(_status)) = child.try_wait() {
                    // Child process has exited, AND sink has emptied its buffered samples
                    if self.sink.lock().unwrap().empty() {
                        return true;
                    }
                }
            }
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
