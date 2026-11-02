use anyhow::{Context, Result, anyhow};
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::range_source::AsyncRangeMediaSource;

struct DecodedBufferState {
    samples: VecDeque<f32>,
    is_eof: bool,
    is_terminated: bool,
}

pub struct DecodedAudioSource {
    sample_rate: u32,
    channels: u16,
    consumed_samples: Arc<AtomicU64>,
    loudness_gain: f32,
    state: Arc<(Mutex<DecodedBufferState>, Condvar, Condvar)>,
    capacity: usize,
    local_buf: Vec<f32>,
    local_pos: usize,
    _worker_handle: Option<thread::JoinHandle<()>>,
}

struct InternalAudioDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sample_buf: Option<SampleBuffer<f32>>,
    sample_position: usize,
    sample_len: usize,
    samples_to_skip: u64,
}

struct ProbeGateSource {
    inner: Box<dyn MediaSource>,
    allow_seek: Arc<AtomicBool>,
}

impl MediaSource for ProbeGateSource {
    fn is_seekable(&self) -> bool {
        self.allow_seek.load(Ordering::Acquire)
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

impl DecodedAudioSource {
    pub fn new<R: Read + Send + 'static>(reader: R, start_offset_secs: f32) -> Result<Self> {
        let source = NonSeekableSource(Mutex::new(reader));
        Self::from_media_source(Box::new(source), start_offset_secs, false)
    }

    pub fn new_seekable(source: AsyncRangeMediaSource, start_offset_secs: f32) -> Result<Self> {
        Self::from_media_source(Box::new(source), start_offset_secs, true)
    }

    pub fn with_loudness_gain(mut self, gain: f32) -> Self {
        self.loudness_gain = gain.clamp(0.25, 2.0);
        self
    }

    pub fn consumed_samples(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.consumed_samples)
    }

    pub fn samples_per_second(&self) -> u64 {
        (self.sample_rate as u64).saturating_mul(self.channels as u64)
    }

    fn from_media_source(
        source: Box<dyn MediaSource>,
        start_offset_secs: f32,
        seekable: bool,
    ) -> Result<Self> {
        let allow_seek = Arc::new(AtomicBool::new(false));
        let gate_source = ProbeGateSource {
            inner: source,
            allow_seek: Arc::clone(&allow_seek),
        };

        let stream =
            MediaSourceStream::new(Box::new(gate_source), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("m4a");

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .context("Unable to identify the downloaded audio stream")?;

        if seekable {
            allow_seek.store(true, Ordering::Release);
        }

        let mut format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| anyhow!("The audio stream contains no playable track"))?;
        let track_id = track.id;
        let time_base = track.codec_params.time_base;
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
            if let Ok(seeked) = format.seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: symphonia::core::units::Time::from(start_offset_secs as f64),
                    track_id: Some(track_id),
                },
            ) {
                if let Some(tb) = time_base {
                    let actual_time = tb.calc_time(seeked.actual_ts);
                    let actual_seconds = actual_time.seconds as f64 + actual_time.frac;
                    ((start_offset_secs as f64 - actual_seconds).max(0.0)
                        * sample_rate as f64
                        * channels as f64) as u64
                } else {
                    0
                }
            } else {
                (start_offset_secs.max(0.0) * sample_rate as f32 * channels as f32) as u64
            }
        } else {
            (start_offset_secs.max(0.0) * sample_rate as f32 * channels as f32) as u64
        };

        let internal = InternalAudioDecoder {
            format,
            decoder,
            track_id,
            sample_buf: None,
            sample_position: 0,
            sample_len: 0,
            samples_to_skip,
        };

        let capacity = ((sample_rate as usize * channels as usize) * 12).max(256 * 1024);
        let state = Arc::new((
            Mutex::new(DecodedBufferState {
                samples: VecDeque::with_capacity(capacity.min(64 * 1024)),
                is_eof: false,
                is_terminated: false,
            }),
            Condvar::new(), // not_empty
            Condvar::new(), // not_full
        ));

        let worker_state = Arc::clone(&state);
        let worker_handle = thread::Builder::new()
            .name("audio-decode-worker".to_string())
            .spawn(move || {
                #[cfg(windows)]
                unsafe {
                    use windows::Win32::System::Threading::{
                        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
                    };
                    let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
                }

                let mut internal = internal;
                let mut chunk = Vec::with_capacity(4096);

                loop {
                    let (lock, not_empty, not_full) = &*worker_state;
                    let mut guard = lock.lock().unwrap();
                    while !guard.is_terminated && !guard.is_eof && guard.samples.len() >= capacity {
                        guard = not_full.wait(guard).unwrap();
                    }
                    if guard.is_terminated {
                        break;
                    }
                    drop(guard);

                    chunk.clear();
                    while chunk.len() < 4096 {
                        if !internal.decode_packet_samples(&mut chunk) {
                            break;
                        }
                    }

                    let mut guard = lock.lock().unwrap();
                    if guard.is_terminated {
                        break;
                    }
                    if chunk.is_empty() {
                        guard.is_eof = true;
                        not_empty.notify_all();
                        break;
                    } else {
                        guard.samples.extend(chunk.drain(..));
                        not_empty.notify_all();
                    }
                }
            })
            .ok();

        // Warm up: wait briefly (up to 80ms) so buffer starts with initial samples ready
        {
            let (lock, not_empty, _) = &*state;
            let guard = lock.lock().unwrap();
            if guard.samples.len() < 4096 && !guard.is_eof {
                let _ = not_empty.wait_timeout(guard, Duration::from_millis(80));
            }
        }

        Ok(Self {
            sample_rate,
            channels,
            consumed_samples: Arc::new(AtomicU64::new(0)),
            loudness_gain: 1.0,
            state,
            capacity,
            local_buf: Vec::with_capacity(2048),
            local_pos: 0,
            _worker_handle: worker_handle,
        })
    }
}

impl InternalAudioDecoder {
    fn decode_packet_samples(&mut self, output: &mut Vec<f32>) -> bool {
        if !self.decode_next_packet() {
            return false;
        }
        if let Some(buf) = self.sample_buf.as_ref() {
            let samples = buf.samples();
            if self.samples_to_skip > 0 {
                let skip = (samples.len() as u64).min(self.samples_to_skip) as usize;
                self.samples_to_skip -= skip as u64;
                if skip < samples.len() {
                    output.extend_from_slice(&samples[skip..]);
                }
            } else {
                output.extend_from_slice(samples);
            }
        }
        true
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
                    tracing::debug!("Skipping an undecodable audio packet: {}", error);
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

            let required_capacity = decoded.capacity();
            let spec = *decoded.spec();
            let buf = match &mut self.sample_buf {
                Some(existing) if existing.capacity() >= required_capacity => existing,
                slot => {
                    *slot = Some(SampleBuffer::<f32>::new(required_capacity as u64, spec));
                    slot.as_mut().unwrap()
                }
            };
            buf.copy_interleaved_ref(decoded);
            self.sample_len = buf.len();
            self.sample_position = 0;
            if self.sample_len > 0 {
                return true;
            }
        }
    }
}

impl Iterator for DecodedAudioSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.local_pos < self.local_buf.len() {
            let sample = self.local_buf[self.local_pos];
            self.local_pos += 1;
            self.consumed_samples.fetch_add(1, Ordering::Relaxed);
            return Some(sample * self.loudness_gain);
        }

        self.local_buf.clear();
        self.local_pos = 0;

        let (lock, not_empty, not_full) = &*self.state;
        let mut guard = lock.lock().unwrap();
        loop {
            if !guard.samples.is_empty() {
                let count = guard.samples.len().min(2048);
                self.local_buf.extend(guard.samples.drain(..count));
                if guard.samples.len() <= self.capacity / 2 {
                    not_full.notify_one();
                }
                break;
            }
            if guard.is_eof || guard.is_terminated {
                return None;
            }
            let (new_guard, wait_res) =
                not_empty.wait_timeout(guard, Duration::from_millis(100)).unwrap();
            guard = new_guard;
            if wait_res.timed_out() && guard.samples.is_empty() && (guard.is_eof || guard.is_terminated) {
                return None;
            }
        }
        drop(guard);

        if self.local_pos < self.local_buf.len() {
            let sample = self.local_buf[self.local_pos];
            self.local_pos += 1;
            self.consumed_samples.fetch_add(1, Ordering::Relaxed);
            Some(sample * self.loudness_gain)
        } else {
            None
        }
    }
}

impl Drop for DecodedAudioSource {
    fn drop(&mut self) {
        let (lock, not_empty, not_full) = &*self.state;
        let mut guard = lock.lock().unwrap();
        guard.is_terminated = true;
        not_empty.notify_all();
        not_full.notify_all();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoded_audio_source_buffer_state() {
        let state = Arc::new((
            Mutex::new(DecodedBufferState {
                samples: VecDeque::from(vec![0.1, 0.2, 0.3]),
                is_eof: true,
                is_terminated: false,
            }),
            Condvar::new(),
            Condvar::new(),
        ));

        let mut source = DecodedAudioSource {
            sample_rate: 44100,
            channels: 2,
            consumed_samples: Arc::new(AtomicU64::new(0)),
            loudness_gain: 1.0,
            state,
            capacity: 1024,
            local_buf: Vec::new(),
            local_pos: 0,
            _worker_handle: None,
        };

        assert_eq!(source.next(), Some(0.1));
        assert_eq!(source.next(), Some(0.2));
        assert_eq!(source.next(), Some(0.3));
        assert_eq!(source.next(), None);
        assert_eq!(source.consumed_samples().load(Ordering::Relaxed), 3);
    }
}