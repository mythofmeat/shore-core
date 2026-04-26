//! Audio playback for TTS streams using rodio.
//!
//! Decodes base64-encoded int16-LE PCM chunks arriving over SWP and feeds
//! them into a rodio sink for progressive playback.

use base64::Engine as _;
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, OutputStreamHandle, Sink};
use tracing::{debug, error, warn};

pub struct AudioPlayer {
    _stream: OutputStream,
    handle: OutputStreamHandle,
    sink: Option<Sink>,
    sample_rate: u32,
    channels: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("failed to open audio output: {0}")]
    OutputError(String),
    #[error("failed to create audio sink: {0}")]
    SinkError(String),
}

impl AudioPlayer {
    pub fn new() -> Result<Self, AudioError> {
        let (stream, handle) =
            OutputStream::try_default().map_err(|e| AudioError::OutputError(e.to_string()))?;
        Ok(Self {
            _stream: stream,
            handle,
            sink: None,
            sample_rate: 24000,
            channels: 1,
        })
    }

    pub fn start(&mut self, sample_rate: u32, channels: u16) {
        if let Some(old) = self.sink.take() {
            old.stop();
        }

        self.sample_rate = sample_rate;
        self.channels = channels;

        match Sink::try_new(&self.handle) {
            Ok(sink) => {
                debug!(sample_rate, channels, "Audio playback started");
                self.sink = Some(sink);
            }
            Err(e) => {
                error!(error = %e, "Failed to create audio sink");
            }
        }
    }

    pub fn feed(&self, base64_data: &str) {
        let Some(ref sink) = self.sink else {
            warn!("Audio feed called with no active sink");
            return;
        };

        let bytes = match base64::engine::general_purpose::STANDARD.decode(base64_data) {
            Ok(b) => b,
            Err(e) => {
                error!(error = %e, "Failed to decode audio chunk");
                return;
            }
        };

        let samples: Vec<f32> = bytes
            .chunks_exact(2)
            .map(|pair| {
                let sample = i16::from_le_bytes([pair[0], pair[1]]);
                sample as f32 / 32768.0
            })
            .collect();

        if !samples.is_empty() {
            let buffer = SamplesBuffer::new(self.channels, self.sample_rate, samples);
            sink.append(buffer);
        }
    }

    pub fn finish(&self) {
        debug!("Audio stream finished, draining buffer");
    }

    pub fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
            debug!("Audio playback stopped");
        }
    }

    pub fn is_playing(&self) -> bool {
        self.sink.as_ref().is_some_and(|s| !s.empty())
    }

    pub fn wait_until_done(&self) {
        if let Some(ref sink) = self.sink {
            sink.sleep_until_end();
        }
    }
}
