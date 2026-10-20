//! Echo cancellation for a voice conversation on systems without Apple's voice-processing unit.
//!
//! The agent's voice reaches the microphone from the speakers, and gxserver's provider runs voice
//! activity detection on everything the microphone hears, so without this the agent interrupts
//! itself. Sonora (a pure-Rust port of WebRTC's audio processing: AEC3, noise suppression and the
//! high-pass filter, BSD-3-Clause) subtracts what this app played from what the microphone heard;
//! it finds the delay between the two on its own. Both sides run at the provider's 24 kHz mono in
//! 10 ms frames.

use sonora::config::{EchoCanceller as EchoCancellerConfig, HighPassFilter, NoiseSuppression};
use sonora::{AudioProcessing, Config, StreamConfig};

/// The provider's sample rate, which both sides of the canceller run at.
pub(crate) const RATE: u32 = 24_000;
/// One 10 ms frame at `RATE`.
pub(crate) const FRAME: usize = (RATE / 100) as usize;

pub(crate) struct EchoCanceller {
    processing: AudioProcessing,
    render_pending: Vec<f32>,
    render_scratch: Vec<f32>,
    capture_pending: Vec<f32>,
    capture_frame: Vec<f32>,
}

impl EchoCanceller {
    pub(crate) fn new() -> Self {
        let stream = StreamConfig::new(RATE, 1);
        let processing = AudioProcessing::builder()
            .config(Config {
                echo_canceller: Some(EchoCancellerConfig::default()),
                high_pass_filter: Some(HighPassFilter::default()),
                noise_suppression: Some(NoiseSuppression::default()),
                ..Default::default()
            })
            .capture_config(stream)
            .render_config(stream)
            .build();
        Self {
            processing,
            render_pending: Vec::with_capacity(FRAME * 2),
            render_scratch: vec![0.0; FRAME],
            capture_pending: Vec::with_capacity(FRAME * 2),
            capture_frame: vec![0.0; FRAME],
        }
    }

    /// Hands over what is about to be played (24 kHz mono), silence included, in playing order.
    pub(crate) fn render(&mut self, samples: &[f32]) {
        self.render_pending.extend_from_slice(samples);
        let mut offset = 0;
        while self.render_pending.len() - offset >= FRAME {
            let frame = &self.render_pending[offset..offset + FRAME];
            let _ = self
                .processing
                .process_render_f32(&[frame], &mut [&mut self.render_scratch]);
            offset += FRAME;
        }
        self.render_pending.drain(..offset);
    }

    /// Cleans microphone audio (24 kHz mono) and appends the result to `out`, a frame at a time,
    /// so up to 10 ms stays inside until the next call.
    pub(crate) fn capture(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        self.capture_pending.extend_from_slice(samples);
        let mut offset = 0;
        while self.capture_pending.len() - offset >= FRAME {
            let frame = &self.capture_pending[offset..offset + FRAME];
            if self
                .processing
                .process_capture_f32(&[frame], &mut [&mut self.capture_frame])
                .is_err()
            {
                self.capture_frame.copy_from_slice(frame);
            }
            out.extend_from_slice(&self.capture_frame);
            offset += FRAME;
        }
        self.capture_pending.drain(..offset);
    }
}
