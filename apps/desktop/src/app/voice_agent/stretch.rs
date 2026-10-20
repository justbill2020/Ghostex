//! Plays the agent's voice faster with its pitch kept, for the panel's 1x-2x speed buttons on
//! systems without a native time-pitch unit (the Mac uses AVAudioUnitTimePitch in
//! `native/macos/GpuiVoiceAudio.m`).
//!
//! WSOLA (waveform-similarity overlap-add) on 24 kHz mono: 25 ms Hann frames are laid down every
//! 12.5 ms of output while the read position in the input advances `rate` times as far, and each
//! frame is shifted (by up to 6.25 ms) to where it best continues the frame before it, so speech
//! keeps its pitch without phasing. At 1x every frame continues the previous one exactly, which
//! reproduces the input sample for sample.

use std::collections::VecDeque;

/// Frame length in samples (25 ms at 24 kHz).
const FRAME: usize = 600;
/// Output hop: half a frame, so consecutive Hann frames sum to one.
const HOP: usize = FRAME / 2;
/// How far a frame may move from its nominal position to line up with the previous one: half the
/// period of an 80 Hz voice, so a full pitch period is always in reach.
const TOLERANCE: usize = 150;
/// The slowest and fastest rates the speed buttons can ask for, with room on both sides.
const MIN_RATE: f32 = 0.5;
const MAX_RATE: f32 = 2.5;
/// Read input is dropped in pieces of about a second, so a long queued reply is not moved for
/// every frame.
const FORGET: usize = 24_000;

pub(crate) struct Stretcher {
    rate: f32,
    window: Vec<f32>,
    /// Input not yet behind the read position; index 0 is the oldest sample still needed.
    input: Vec<f32>,
    /// Where the next frame would start if it were not shifted, in `input` indices.
    nominal: f64,
    /// Start of the previous frame in `input`, when there is one.
    previous: Option<usize>,
    /// The falling half of the previous frame, waiting for the next frame's rising half.
    tail: Vec<f32>,
    output: VecDeque<f32>,
    /// How many input samples the samples in `output` stand for.
    output_input_samples: f64,
}

impl Stretcher {
    pub(crate) fn new() -> Self {
        let window = (0..FRAME)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FRAME as f32).cos())
            .collect();
        Self {
            rate: 1.0,
            window,
            input: Vec::new(),
            nominal: 0.0,
            previous: None,
            tail: vec![0.0; HOP],
            output: VecDeque::new(),
            output_input_samples: 0.0,
        }
    }

    /// Plays `rate` times as fast from the next frame on.
    pub(crate) fn set_rate(&mut self, rate: f32) {
        if rate.is_finite() {
            self.rate = rate.clamp(MIN_RATE, MAX_RATE);
        }
    }

    /// Appends input (24 kHz mono, -1..1). It is stretched only as it is pulled, so a new rate
    /// applies to what is already queued.
    pub(crate) fn push(&mut self, samples: &[f32]) {
        self.input.extend_from_slice(samples);
    }

    /// Fills `out` with as much stretched audio as is ready and answers how many samples it wrote.
    /// When the input runs out, what is left of it is played out unstretched (at most one frame),
    /// so the end of a reply is never held back.
    pub(crate) fn pull(&mut self, out: &mut [f32]) -> usize {
        while self.output.len() < out.len() && self.step() {}
        if self.output.len() < out.len() {
            self.drain();
        }
        let count = out.len().min(self.output.len());
        if count > 0 {
            let share = self.output_input_samples * count as f64 / self.output.len() as f64;
            self.output_input_samples = (self.output_input_samples - share).max(0.0);
        }
        for (slot, sample) in out.iter_mut().zip(self.output.drain(..count)) {
            *slot = sample;
        }
        count
    }

    /// How many input samples have not been heard yet (the speed does not change the count).
    pub(crate) fn pending_input_samples(&self) -> u64 {
        let unread = (self.input.len() as f64 - self.nominal).max(0.0);
        (unread + self.output_input_samples).round() as u64
    }

    /// Drops everything queued.
    pub(crate) fn clear(&mut self) {
        self.input.clear();
        self.nominal = 0.0;
        self.previous = None;
        self.output.clear();
        self.output_input_samples = 0.0;
    }

    /// Lays down one frame when enough input is there.
    fn step(&mut self) -> bool {
        let center = self.nominal.round() as usize;
        let low = center.saturating_sub(TOLERANCE);
        let high = center + TOLERANCE;
        if self.input.len() < high + FRAME {
            return false;
        }
        let start = match self.previous {
            None => {
                // The first frame continues "itself", so the audio starts at full level.
                for (i, slot) in self.tail.iter_mut().enumerate() {
                    *slot = self.input[center + i] * self.window[HOP + i];
                }
                center
            }
            Some(previous) => {
                let natural = previous + HOP;
                if (self.rate - 1.0).abs() < 0.001 && (low..=high).contains(&natural) {
                    natural
                } else {
                    best_match(&self.input, natural, low, high)
                }
            }
        };
        for i in 0..HOP {
            let sample = self.tail[i] + self.input[start + i] * self.window[i];
            self.output.push_back(sample);
        }
        for (i, slot) in self.tail.iter_mut().enumerate() {
            *slot = self.input[start + HOP + i] * self.window[HOP + i];
        }
        self.output_input_samples += HOP as f64 * self.rate as f64;
        self.previous = Some(start);
        self.nominal += HOP as f64 * self.rate as f64;
        self.forget_read_input();
        true
    }

    /// Plays out the input after the last frame unstretched and starts afresh.
    fn drain(&mut self) {
        let from = match self.previous {
            Some(previous) => previous + HOP,
            None => 0,
        };
        if from < self.input.len() {
            let count = self.input.len() - from;
            self.output.extend(self.input.drain(from..));
            self.output_input_samples += count as f64;
        }
        self.input.clear();
        self.nominal = 0.0;
        self.previous = None;
    }

    /// Drops input that no later frame can reach.
    fn forget_read_input(&mut self) {
        let Some(previous) = self.previous else {
            return;
        };
        let reachable = (self.nominal.floor() as usize)
            .saturating_sub(TOLERANCE)
            .min(previous + HOP);
        if reachable >= FORGET {
            self.input.drain(..reachable);
            self.nominal -= reachable as f64;
            self.previous = Some(previous - reachable);
        }
    }
}

/// The start in `low..=high` whose first half-frame looks most like the half-frame at `natural`
/// (normalized cross-correlation), searched every second sample and then refined.
fn best_match(input: &[f32], natural: usize, low: usize, high: usize) -> usize {
    let target = &input[natural..natural + HOP];
    let score = |start: usize| {
        let candidate = &input[start..start + HOP];
        let mut dot = 0.0f32;
        let mut energy = 0.0f32;
        for (a, b) in target.iter().zip(candidate) {
            dot += a * b;
            energy += b * b;
        }
        dot / (energy + 1e-9).sqrt()
    };
    let mut best = low;
    let mut best_score = f32::MIN;
    let mut start = low;
    while start <= high {
        let value = score(start);
        if value > best_score {
            best = start;
            best_score = value;
        }
        start += 2;
    }
    for start in [best.saturating_sub(1).max(low), (best + 1).min(high)] {
        let value = score(start);
        if value > best_score {
            best = start;
            best_score = value;
        }
    }
    best
}
