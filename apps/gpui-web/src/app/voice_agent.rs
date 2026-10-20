//! The web build's stand-in for the desktop's `app/voice_agent/`: the names the shared chat files
//! call, with no microphone behind them.
//!
//! CDXC:VoiceMode 2026-10-20 WHY: the page has no echo-cancelled microphone path yet (the contract's `AudioWorklet` capture is not built), so it draws neither the Voice button (its header is its own) nor Dictate (`DICTATION` is desktop only); `native_chat/dictation.rs` still names the Dictate state it would record with.

use std::time::Duration;

use gpui::{Context, EventEmitter};

/// Dictate's phases, as the desktop's `voice_agent::DictatePhase` names them; the page stays idle.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum DictatePhase {
    Idle,
    Starting,
    Recording,
    Transcribing,
    Failed { kept: (), message: String },
}

#[allow(dead_code)]
pub(crate) enum DictateEvent {
    Text(String),
    Failure { title: String, message: String },
}

/// A Dictate that never records: Dictate is not drawn on the page.
pub(crate) struct Dictate {
    phase: DictatePhase,
}

impl EventEmitter<DictateEvent> for Dictate {}

#[allow(dead_code)]
impl Dictate {
    pub(crate) fn new() -> Self {
        Self {
            phase: DictatePhase::Idle,
        }
    }

    pub(crate) fn phase(&self) -> &DictatePhase {
        &self.phase
    }

    pub(crate) fn recording(&self) -> bool {
        false
    }

    pub(crate) fn transcribing(&self) -> bool {
        false
    }

    pub(crate) fn elapsed(&self) -> Duration {
        Duration::ZERO
    }

    pub(crate) fn toggle(&mut self, _cx: &mut Context<Self>) {}

    pub(crate) fn cancel(&mut self, _cx: &mut Context<Self>) {}

    pub(crate) fn retry(&mut self, _cx: &mut Context<Self>) {}

    pub(crate) fn transcribe_previous(&mut self, _cx: &mut Context<Self>) {}
}

pub(crate) fn previous_recording() -> Option<(String, u64, i64)> {
    None
}

pub(crate) fn refresh_previous() {}

pub(crate) fn dictation_clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
