//! The desktop half of the manager voice agent and of Grok dictation. gxserver owns the provider,
//! the keys, the tools and the idle hang-up (`server/src/voice_agent/`); this app streams the
//! microphone in, plays the agent's audio and draws state and captions.
//! Client contract: docs/2026-10-20/voice-mode/voice-agent-client-contract.md.
//!
//! - `endpoint.rs`: this computer's gxserver, which every voice socket talks to.
//! - `audio.rs`: the microphone and speaker, one front over a `VoiceAudioBackend`
//!   (`audio_backend.rs`): `audio_mac.rs` (AVAudioEngine with voice-processing I/O) on macOS,
//!   `audio_cpal.rs` on Windows and Linux with `aec.rs` (echo cancellation) and `stretch.rs` (speed);
//!   `test_mic.rs` the developer-only WAV feed that stands in for the microphone.
//! - `conversation.rs`: the conversation the work area header's Voice button starts and ends.
//! - `panel.rs`: the panel that shows it; `header_button.rs`: the Voice button.
//! - `memory.rs`: the remembered last conversation the panel offers to continue.
//! - `dictation.rs`: the chat box's and the terminal bar's Dictate: `recorder.rs` records to a
//!   file first, then gxserver keeps and transcribes it (xAI speech-to-text).

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod aec;
pub(crate) mod audio;
mod audio_backend;
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod audio_cpal;
#[cfg(target_os = "macos")]
mod audio_mac;
pub(crate) mod conversation;
pub(crate) mod dictation;
pub(crate) mod endpoint;
pub(crate) mod header_button;
pub(crate) mod memory;
pub(crate) mod panel;
pub(crate) mod recorder;
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod stretch;
mod test_mic;

pub(crate) use conversation::{VOICE_AGENT_AVAILABLE, VoiceAgent};
pub(crate) use dictation::{
    Dictate, DictateEvent, DictatePhase, clock as dictation_clock, previous_recording,
    refresh_previous,
};
pub(crate) use endpoint::set_local_voice_endpoint;
