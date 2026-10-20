//! What a platform's microphone and speaker do for `audio::VoiceAudio`, so the conversation,
//! the panel and dictation share one path whatever the system: the Mac's AVAudioEngine
//! (`native/macos/GpuiVoiceAudio.m`) or cpal on Windows and Linux (`audio_cpal.rs`). Starting is
//! each backend's own constructor; dropping the backend stops it, and no callback runs after the
//! drop returns.

pub(crate) trait VoiceAudioBackend: Send + Sync {
    /// Queues agent audio (24 kHz mono samples) after what is playing.
    fn enqueue(&self, samples: &[i16]);
    /// Plays the agent's audio `rate` times as fast, pitch kept.
    fn set_rate(&self, rate: f32);
    /// Stops the agent's audio at once and drops what was queued. Answers how many samples were
    /// dropped, in the provider's samples whatever the playback rate.
    fn flush(&self) -> u64;
}
