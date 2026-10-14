use anyhow::{anyhow, Context, Result};
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::range_source::AsyncRangeMediaSource;

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
