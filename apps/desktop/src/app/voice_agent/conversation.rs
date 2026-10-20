//! The conversation the work area header's Voice button starts and ends: one `/api/voice` socket,
//! the echo-cancelled microphone streaming into it, the agent's audio playing back, and the state,
//! captions and tool lines the voice panel draws.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use ghostex_gx_chat_client::voice::{VoiceInbound, VoiceSink, VoiceSocket};
use gpui::{Pixels, Task, WindowHandle};
use serde_json::{Value, json};

use super::audio::{AudioStartError, MICROPHONE_KEPT_STOPPING, SILENT_MICROPHONE, VoiceAudio};
use super::endpoint::local_voice_endpoint;
use super::memory::{VoiceMemory, VoiceMemoryState};
use super::panel::VoicePanelWindow;
use crate::*;

/// Whether this build has the Voice button.
///
/// CDXC:VoiceMode 2026-10-20 DECISION: User: "Please make the voice agent feature and the dictating feature and all of those features related to voice mode basically there on Linux and on Windows. They're not Mac only, you know what I mean?" The Voice button, the panel, Dictate, the voice memory and the settings are on macOS, Windows and Linux alike; this supersedes the macOS-only gate.
/// CDXC:VoiceMode 2026-10-20 WHY: hands-free voice needs echo cancellation, or the agent's own voice from the speakers interrupts it (gxserver's provider runs voice activity detection on everything the microphone hears). macOS has it in the voice-processing I/O unit (`native/macos/GpuiVoiceAudio.m`); Windows and Linux run Sonora's AEC3 in-process on the audio this app plays (`audio_cpal.rs`, `aec.rs`), so it works without headphones and without system setup. The GPUI web build has its own stand-in that hides it.
pub(crate) const VOICE_AGENT_AVAILABLE: bool = cfg!(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux"
));

/// How long Hang up waits for gxserver's `ended` before letting go of the conversation itself.
const HANG_UP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);
/// The panel keeps this many rows; older ones scroll away for good.
const MAX_ROWS: usize = 60;
/// A device change restarts the audio at most this many times per conversation.
pub(super) const MAX_AUDIO_RESTARTS: u32 = 5;
/// The speeds the panel's buttons and Settings › Voice agent offer (`voiceAgentSpeed`).
pub(crate) const VOICE_AGENT_SPEEDS: [f32; 6] = [1.0, 1.2, 1.4, 1.6, 1.8, 2.0];
const SPEED_SETTING: &str = "voiceAgentSpeed";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum VoicePhase {
    /// No conversation, and the panel is closed.
    #[default]
    Idle,
    /// The socket is open and gxserver is opening the provider session.
    Starting,
    /// The provider accepted the session.
    Live,
    /// Hang up was pressed; waiting for gxserver's `ended`.
    Ending,
    /// The conversation is over and the panel says why (an error, or the idle hang-up).
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoiceRole {
    User,
    Agent,
}

/// One line of the panel's running transcript.
#[derive(Clone, Debug)]
pub(crate) enum VoiceRow {
    Caption {
        role: VoiceRole,
        item_id: String,
        text: String,
        /// The agent was talked over: the caption ends where its audio was cut.
        cut: bool,
    },
    Tool {
        call_id: String,
        label: String,
        /// `None` while it runs.
        ok: Option<bool>,
    },
    Announcement {
        text: String,
    },
}

/// Why the conversation stopped or could not start, for the panel.
#[derive(Clone, Debug)]
pub(crate) struct VoiceProblem {
    /// gxserver's error code (`noKey`, `badKey`, `providerUnavailable`, ...), or `microphone`,
    /// `connection`, `idle` for what the app itself noticed.
    pub(crate) code: String,
    pub(crate) message: String,
}

impl VoiceProblem {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// Whether Settings › Voice agent is where the user fixes it.
    pub(crate) fn wants_settings(&self) -> bool {
        matches!(self.code.as_str(), "noKey" | "badKey")
    }
}

/// What the panel and the Voice button draw, and the live link behind them.
#[derive(Default)]
pub(crate) struct VoiceAgent {
    pub(crate) phase: VoicePhase,
    /// gxserver's last `state`: `connecting`, `listening`, `userSpeaking`, `thinking`,
    /// `speaking` or `toolRunning`.
    pub(crate) state: String,
    /// The running tool's label ("Checking your sessions") while `state` is `toolRunning`.
    pub(crate) tool_label: Option<String>,
    pub(crate) rows: Vec<VoiceRow>,
    pub(crate) muted: bool,
    pub(crate) problem: Option<VoiceProblem>,
    /// `xai` or `openai`, once `ready` named it.
    pub(crate) provider: Option<String>,
    /// The voice the agent speaks with now, and the provider's voices (from `ready`).
    pub(crate) voice: Option<String>,
    pub(crate) voice_options: Vec<String>,
    /// A voice sent to gxserver and not answered yet.
    voice_requested: Option<String>,
    /// A picked voice this conversation could not switch to: it speaks from the next one.
    pub(crate) next_voice: Option<String>,
    /// The panel shows the voice list instead of the transcript.
    pub(crate) voice_picker_open: bool,
    /// How fast the agent's audio plays (`voiceAgentSpeed`).
    pub(crate) speed: f32,
    /// Where the user dragged the panel: its gap to the window's trailing edge and its top, kept
    /// for the rest of the app's run.
    pub(crate) panel_place: Option<gpui::Point<Pixels>>,
    /// Where the panel was when the current drag started, in the main window's content.
    pub(crate) panel_drag_from: Option<gpui::Point<Pixels>>,
    /// The remembered last conversation gxserver named in `ready`, and what the panel shows of it.
    pub(crate) memory: Option<VoiceMemory>,
    pub(crate) memory_state: VoiceMemoryState,
    /// The "Remembering" line shows the whole summary.
    pub(crate) memory_details_open: bool,
    link: Option<VoiceLink>,
    /// Bumped per conversation, so a late message of an earlier one is ignored.
    generation: u64,
    pub(crate) panel: Option<WindowHandle<VoicePanelWindow>>,
    /// The main window size the panel was placed for.
    pub(crate) panel_placed_for: Option<gpui::Size<Pixels>>,
    pub(crate) panel_reposition: Option<Task<()>>,
    hang_up_timer: Option<Task<()>>,
}

impl VoiceAgent {
    /// Whether a conversation is open or opening (the Voice button reads as on).
    pub(crate) fn active(&self) -> bool {
        matches!(
            self.phase,
            VoicePhase::Starting | VoicePhase::Live | VoicePhase::Ending
        )
    }
}

/// The open socket and audio of one conversation.
struct VoiceLink {
    socket: Arc<VoiceSocket>,
    shared: Arc<LinkShared>,
    audio_restarts: u32,
    _events: Task<()>,
}

/// What the socket thread, the audio thread and the app share.
struct LinkShared {
    /// Microphone audio flows only after `ready`.
    streaming: AtomicBool,
    muted: AtomicBool,
    /// The playback speed as `f32` bits, for audio that (re)starts.
    rate: AtomicU32,
    playback: Mutex<Playback>,
    sender: futures::channel::mpsc::UnboundedSender<VoiceMessage>,
}

#[derive(Default)]
struct Playback {
    audio: Option<Arc<VoiceAudio>>,
    /// The conversation is over: audio that finishes starting now is stopped at once.
    closed: bool,
    /// Talked over: agent audio is dropped until the next reply starts.
    dropping: bool,
    /// Samples of the current reply handed to the speaker.
    relayed: u64,
}

enum VoiceMessage {
    Socket(VoiceInbound),
    AudioStarted,
    AudioFailed(AudioStartError),
    AudioDeviceChanged,
    MicrophoneSilent,
}

impl GhostexGpuiApp {
    /// The Voice button: starts a conversation, or hangs up the open one.
    pub(crate) fn toggle_voice_agent_conversation(&mut self, cx: &mut gpui::Context<Self>) {
        if self.voice_agent.active() {
            self.hang_up_voice_agent(cx);
        } else {
            self.start_voice_agent_conversation(cx);
        }
    }

    pub(crate) fn start_voice_agent_conversation(&mut self, cx: &mut gpui::Context<Self>) {
        if !VOICE_AGENT_AVAILABLE || self.voice_agent.active() {
            return;
        }
        let agent = &mut self.voice_agent;
        agent.generation = agent.generation.wrapping_add(1);
        agent.rows.clear();
        agent.problem = None;
        agent.tool_label = None;
        agent.provider = None;
        agent.voice = None;
        agent.voice_options.clear();
        agent.voice_requested = None;
        agent.next_voice = None;
        agent.voice_picker_open = false;
        agent.memory = None;
        agent.memory_state = VoiceMemoryState::Hidden;
        agent.memory_details_open = false;
        agent.speed =
            voice_agent_speed_setting(shared_settings::shared_sidebar_settings_snapshot().object());
        agent.muted = false;
        agent.state = "connecting".to_string();
        agent.hang_up_timer = None;
        let Some(endpoint) = local_voice_endpoint() else {
            agent.phase = VoicePhase::Ended;
            agent.problem = Some(VoiceProblem::new(
                "connection",
                "Ghostex's service is not running yet. Try again in a moment.",
            ));
            self.sync_voice_agent_panel(cx);
            return;
        };
        agent.phase = VoicePhase::Starting;
        let generation = agent.generation;
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<VoiceMessage>();
        let shared = Arc::new(LinkShared {
            streaming: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            rate: AtomicU32::new(self.voice_agent.speed.to_bits()),
            playback: Mutex::new(Playback::default()),
            sender,
        });
        let socket = Arc::new(VoiceSocket::open(
            &endpoint,
            &[("mode", "conversation"), ("client", "desktop")],
            socket_sink(shared.clone()),
        ));
        start_audio(socket.clone(), shared.clone());
        let events = cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.next().await {
                let alive = this.update(cx, |this, cx| {
                    this.receive_voice_agent_message(generation, message, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        self.voice_agent.link = Some(VoiceLink {
            socket,
            shared,
            audio_restarts: 0,
            _events: events,
        });
        self.sync_voice_agent_panel(cx);
    }

    /// Hang up: gxserver answers `ended` and closes; the panel closes with it.
    pub(crate) fn hang_up_voice_agent(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(link) = self.voice_agent.link.as_ref() else {
            self.close_voice_agent_panel(cx);
            return;
        };
        link.shared.streaming.store(false, Ordering::SeqCst);
        link.socket.send_json(&json!({ "type": "end" }));
        stop_playback(&link.shared);
        self.voice_agent.phase = VoicePhase::Ending;
        let generation = self.voice_agent.generation;
        self.voice_agent.hang_up_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HANG_UP_GRACE).await;
            let _ = this.update(cx, |this, cx| {
                if this.voice_agent.generation == generation
                    && this.voice_agent.phase == VoicePhase::Ending
                {
                    this.end_voice_agent_link();
                    this.close_voice_agent_panel(cx);
                }
            });
        }));
        self.sync_voice_agent_panel(cx);
    }

    pub(crate) fn toggle_voice_agent_mute(&mut self, cx: &mut gpui::Context<Self>) {
        let muted = !self.voice_agent.muted;
        self.voice_agent.muted = muted;
        if let Some(link) = &self.voice_agent.link {
            link.shared.muted.store(muted, Ordering::SeqCst);
        }
        self.sync_voice_agent_panel(cx);
    }

    /// "Stop talking": gxserver cancels the reply and answers `interrupted`.
    pub(crate) fn interrupt_voice_agent(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(link) = &self.voice_agent.link {
            link.socket.send_json(&json!({ "type": "interrupt" }));
            stop_playback(&link.shared);
        }
        cx.notify();
    }

    /// Sends a control frame on the live conversation's socket; false when none is live.
    pub(super) fn send_voice_agent_frame(&self, frame: &Value) -> bool {
        match &self.voice_agent.link {
            Some(link) if self.voice_agent.phase == VoicePhase::Live => {
                link.socket.send_json(frame);
                true
            }
            _ => false,
        }
    }

    /// Lets go of the socket and the audio, without telling gxserver.
    fn end_voice_agent_link(&mut self) {
        if let Some(link) = self.voice_agent.link.take() {
            link.shared.streaming.store(false, Ordering::SeqCst);
            if let Ok(mut playback) = link.shared.playback.lock() {
                playback.closed = true;
                playback.audio = None;
            }
            link.socket.close();
        }
        self.voice_agent.hang_up_timer = None;
    }

    fn receive_voice_agent_message(
        &mut self,
        generation: u64,
        message: VoiceMessage,
        cx: &mut gpui::Context<Self>,
    ) {
        if generation != self.voice_agent.generation {
            return;
        }
        match message {
            VoiceMessage::Socket(VoiceInbound::Event(event)) => {
                self.apply_voice_agent_event(&event, cx)
            }
            VoiceMessage::Socket(VoiceInbound::Audio(_)) => {}
            VoiceMessage::Socket(VoiceInbound::Closed(reason)) => {
                let was_ending = self.voice_agent.phase == VoicePhase::Ending;
                let open = matches!(
                    self.voice_agent.phase,
                    VoicePhase::Starting | VoicePhase::Live
                );
                self.end_voice_agent_link();
                if was_ending {
                    self.close_voice_agent_panel(cx);
                    return;
                }
                if open {
                    self.voice_agent.phase = VoicePhase::Ended;
                    self.voice_agent.problem.get_or_insert(VoiceProblem::new(
                        "connection",
                        reason.unwrap_or_else(|| {
                            "The voice conversation closed unexpectedly.".to_string()
                        }),
                    ));
                }
            }
            VoiceMessage::AudioStarted => {}
            VoiceMessage::AudioFailed(error) => {
                if let Some(link) = &self.voice_agent.link {
                    link.socket.send_json(&json!({ "type": "end" }));
                }
                self.end_voice_agent_link();
                self.voice_agent.phase = VoicePhase::Ended;
                self.voice_agent.problem = Some(VoiceProblem::new("microphone", error.message()));
            }
            VoiceMessage::AudioDeviceChanged => {
                let Some(link) = self.voice_agent.link.as_mut() else {
                    return;
                };
                if link.audio_restarts < MAX_AUDIO_RESTARTS {
                    link.audio_restarts += 1;
                    if let Ok(mut playback) = link.shared.playback.lock() {
                        playback.audio = None;
                    }
                    start_audio(link.socket.clone(), link.shared.clone());
                } else {
                    // The audio has stopped and is not started again: say so instead of going
                    // quiet.
                    link.socket.send_json(&json!({ "type": "end" }));
                    self.end_voice_agent_link();
                    self.voice_agent.phase = VoicePhase::Ended;
                    self.voice_agent.problem =
                        Some(VoiceProblem::new("microphone", MICROPHONE_KEPT_STOPPING));
                }
            }
            VoiceMessage::MicrophoneSilent => {
                self.push_voice_agent_row(VoiceRow::Announcement {
                    text: SILENT_MICROPHONE.to_string(),
                });
            }
        }
        self.sync_voice_agent_panel(cx);
    }

    fn apply_voice_agent_event(&mut self, event: &Value, cx: &mut gpui::Context<Self>) {
        let text = |key: &str| event[key].as_str().unwrap_or_default().to_string();
        match event["type"].as_str().unwrap_or_default() {
            "state" => {
                let state = text("state");
                if state != "toolRunning" {
                    self.voice_agent.tool_label = None;
                }
                self.voice_agent.state = state;
            }
            "ready" => {
                if self.voice_agent.phase == VoicePhase::Starting {
                    self.voice_agent.phase = VoicePhase::Live;
                }
                self.voice_agent.provider = Some(text("provider"));
                self.voice_agent.voice = Some(text("voice"));
                self.voice_agent.voice_options = event["voiceOptions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|voice| voice.as_str().map(str::to_string))
                    .collect();
                self.apply_voice_agent_memory_ready(event);
                if let Some(link) = &self.voice_agent.link {
                    link.shared.streaming.store(true, Ordering::SeqCst);
                    link.socket.send_json(&json!({
                        "type": "playbackRate",
                        "rate": self.voice_agent.speed,
                    }));
                }
                // A voice picked while connecting.
                self.apply_voice_agent_settings(
                    shared_settings::shared_sidebar_settings_snapshot().object(),
                    cx,
                );
            }
            "caption" => {
                let role = if event["role"].as_str() == Some("user") {
                    VoiceRole::User
                } else {
                    VoiceRole::Agent
                };
                let item_id = text("itemId");
                let caption = text("text");
                let cut = event["cut"].as_bool().unwrap_or(false);
                if role == VoiceRole::User && !caption.trim().is_empty() {
                    self.voice_agent_memory_user_spoke();
                }
                let position = self.voice_agent.rows.iter().rposition(
                    |row| matches!(row, VoiceRow::Caption { item_id: id, .. } if *id == item_id),
                );
                // CDXC:VoiceMode 2026-10-20 WHY: an interrupted reply ends with one `cut` caption holding only the words actually played, which can come after that reply's `final` caption; it replaces the reply's caption whenever it lands, and a cut before any word was heard removes the caption.
                if cut && caption.trim().is_empty() {
                    if let Some(position) = position {
                        self.voice_agent.rows.remove(position);
                    }
                } else {
                    let existing =
                        position.and_then(|position| match &mut self.voice_agent.rows[position] {
                            VoiceRow::Caption {
                                text, cut: was_cut, ..
                            } => Some((text, was_cut)),
                            _ => None,
                        });
                    match existing {
                        // A reply that was cut keeps the cut caption.
                        Some((_, was_cut)) if *was_cut && !cut => {}
                        Some((existing_text, was_cut)) => {
                            *existing_text = caption;
                            *was_cut = cut;
                        }
                        None if !caption.trim().is_empty() => {
                            self.push_voice_agent_row(VoiceRow::Caption {
                                role,
                                item_id,
                                text: caption,
                                cut,
                            })
                        }
                        None => {}
                    }
                }
            }
            "voice" => {
                let voice = text("voice");
                let agent = &mut self.voice_agent;
                if agent.voice_requested.as_deref() == Some(voice.as_str()) {
                    agent.voice_requested = None;
                }
                if event["applied"].as_str() == Some("now") {
                    agent.voice = Some(voice);
                    agent.next_voice = None;
                } else if agent.next_voice.as_deref() != Some(voice.as_str()) {
                    agent.next_voice = Some(voice.clone());
                    let provider = provider_name(agent.provider.as_deref().unwrap_or_default());
                    let name = voice_name(&voice);
                    let text = if event["reason"].as_str() == Some("providerLocksVoice") {
                        format!(
                            "{provider} keeps a call's voice once it has spoken, so {name} speaks from your next conversation."
                        )
                    } else {
                        format!(
                            "{provider} didn't switch voices mid-call, so {name} speaks from your next conversation."
                        )
                    };
                    self.push_voice_agent_row(VoiceRow::Announcement { text });
                }
            }
            "memory" => self.apply_voice_agent_memory_event(event),
            "interrupted" => {
                if let Some(link) = &self.voice_agent.link
                    && let Some(played_ms) = event["playedMs"].as_u64()
                {
                    link.socket.send_json(&json!({
                        "type": "playback",
                        "itemId": text("itemId"),
                        "playedMs": played_ms,
                    }));
                }
            }
            "tool" => {
                let call_id = text("callId");
                let label = text("label");
                if event["phase"].as_str() == Some("started") {
                    self.voice_agent.tool_label = Some(label.clone());
                    self.push_voice_agent_row(VoiceRow::Tool {
                        call_id,
                        label,
                        ok: None,
                    });
                } else if let Some(VoiceRow::Tool { ok, .. }) =
                    self.voice_agent.rows.iter_mut().rev().find(
                        |row| matches!(row, VoiceRow::Tool { call_id: id, .. } if *id == call_id),
                    )
                {
                    *ok = Some(event["ok"].as_bool().unwrap_or(true));
                }
            }
            "announcement" => {
                let announcement = text("text");
                if !announcement.trim().is_empty() {
                    self.push_voice_agent_row(VoiceRow::Announcement { text: announcement });
                }
            }
            "error" => {
                let problem = VoiceProblem::new(&text("code"), error_sentence(event));
                if event["fatal"].as_bool() == Some(false) {
                    self.push_voice_agent_row(VoiceRow::Announcement {
                        text: problem.message,
                    });
                } else {
                    self.voice_agent.problem = Some(problem);
                }
            }
            "ended" => {
                let reason = text("reason");
                let was_ending = self.voice_agent.phase == VoicePhase::Ending;
                self.end_voice_agent_link();
                if reason == "user" || was_ending {
                    self.close_voice_agent_panel(cx);
                    return;
                }
                self.voice_agent.phase = VoicePhase::Ended;
                if self.voice_agent.problem.is_none() {
                    self.voice_agent.problem = Some(VoiceProblem::new(
                        if reason == "idle" { "idle" } else { "ended" },
                        ended_sentence(&reason, event["message"].as_str()),
                    ));
                }
            }
            _ => {}
        }
    }

    fn push_voice_agent_row(&mut self, row: VoiceRow) {
        let rows = &mut self.voice_agent.rows;
        rows.push(row);
        if rows.len() > MAX_ROWS {
            let excess = rows.len() - MAX_ROWS;
            rows.drain(..excess);
        }
    }

    /// Closes the panel and forgets the finished conversation.
    pub(crate) fn close_voice_agent_panel(&mut self, cx: &mut gpui::Context<Self>) {
        self.end_voice_agent_link();
        self.voice_agent.phase = VoicePhase::Idle;
        self.voice_agent.problem = None;
        self.sync_voice_agent_panel(cx);
    }

    /// The panel's speed buttons: saves `voiceAgentSpeed`, which the settings hook applies.
    pub(crate) fn set_voice_agent_speed(&mut self, speed: f32, cx: &mut gpui::Context<Self>) {
        self.save_voice_agent_setting(SPEED_SETTING, json!(speed_setting_value(speed)), cx);
    }

    /// The panel's voice list: saves the voice for the conversation's provider, which the
    /// settings hook applies.
    pub(crate) fn pick_voice_agent_voice(&mut self, voice: String, cx: &mut gpui::Context<Self>) {
        self.voice_agent.voice_picker_open = false;
        let key = voice_setting_key(self.voice_agent.provider.as_deref().unwrap_or_default());
        self.save_voice_agent_setting(key, json!(voice), cx);
        self.sync_voice_agent_panel(cx);
    }

    pub(crate) fn toggle_voice_agent_voice_picker(&mut self, cx: &mut gpui::Context<Self>) {
        let agent = &mut self.voice_agent;
        agent.voice_picker_open = !agent.voice_picker_open && !agent.voice_options.is_empty();
        self.sync_voice_agent_panel(cx);
    }

    fn save_voice_agent_setting(&mut self, key: &str, value: Value, cx: &mut gpui::Context<Self>) {
        self.handle_gpui_app_modal_update_settings_patch_message(
            &json!({ "patch": { key: value }, "source": "voicePanel" }),
            cx,
        );
    }

    /// Brings the open conversation in line with the saved speed and voice, after any settings
    /// save (the panel's buttons, Settings › Voice agent, `ghostex settings set`).
    ///
    /// CDXC:VoiceMode 2026-10-20 WHY: the speed is applied on this computer (the audio is stretched with its pitch kept: `native/macos/GpuiVoiceAudio.m` on macOS, `stretch.rs` on Windows and Linux), at once and mid-sentence, for both providers; gxserver only learns the rate (`playbackRate`) so its playback clock stays right. A voice is sent to gxserver, which switches when the provider allows and otherwise answers that it speaks from the next conversation.
    pub(crate) fn apply_voice_agent_settings(
        &mut self,
        settings: &serde_json::Map<String, Value>,
        cx: &mut gpui::Context<Self>,
    ) {
        let speed = voice_agent_speed_setting(settings);
        let agent = &mut self.voice_agent;
        let Some(link) = agent.link.as_ref() else {
            agent.speed = speed;
            return;
        };
        let mut changed = false;
        if speed != agent.speed {
            agent.speed = speed;
            link.shared.rate.store(speed.to_bits(), Ordering::SeqCst);
            if let Ok(playback) = link.shared.playback.lock()
                && let Some(audio) = &playback.audio
            {
                audio.set_rate(speed);
            }
            if agent.phase == VoicePhase::Live {
                link.socket
                    .send_json(&json!({ "type": "playbackRate", "rate": speed }));
            }
            changed = true;
        }
        if agent.phase == VoicePhase::Live
            && let Some(provider) = agent.provider.as_deref()
            && let Some(voice) = settings
                .get(voice_setting_key(provider))
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
            && agent.voice_options.contains(&voice)
        {
            if agent.voice.as_deref() == Some(voice.as_str()) {
                if agent.next_voice.take().is_some() {
                    changed = true;
                }
            } else if agent.voice_requested.as_deref() != Some(voice.as_str())
                && agent.next_voice.as_deref() != Some(voice.as_str())
            {
                link.socket
                    .send_json(&json!({ "type": "voice", "voice": voice }));
                agent.voice_requested = Some(voice);
                changed = true;
            }
        }
        if changed {
            self.sync_voice_agent_panel(cx);
        }
    }

    /// Settings › Voice agent, from the panel's "Open Settings".
    pub(crate) fn open_gpui_settings_voice_agent_page(&mut self, cx: &mut gpui::Context<Self>) {
        let modal = GpuiAppModalKind::Settings;
        let sidebar_state_message = self.gpui_app_modal_sidebar_state_message_for_open(modal, cx);
        let mut open_message = json!({
            "initialTab": "voiceAgent",
            "modal": modal.modal_id(),
            "type": "open",
        });
        open_message["latestSidebarStateMessage"] = sidebar_state_message.clone();
        self.open_gpui_app_modal_window(modal, open_message, sidebar_state_message, None, cx);
    }
}

/// Where the socket thread sends what it reads. Agent audio goes straight to the speaker here,
/// on the socket thread, and an interruption silences it here too, without waiting for the main
/// thread.
fn socket_sink(shared: Arc<LinkShared>) -> VoiceSink {
    Arc::new(move |inbound| {
        let inbound = match inbound {
            VoiceInbound::Audio(pcm) => {
                if let Ok(mut playback) = shared.playback.lock()
                    && !playback.dropping
                {
                    if let Some(audio) = &playback.audio {
                        audio.enqueue(&pcm);
                    }
                    playback.relayed += (pcm.len() / 2) as u64;
                }
                return;
            }
            VoiceInbound::Event(mut event) => {
                match event["type"].as_str() {
                    Some("audioStart") => {
                        if let Ok(mut playback) = shared.playback.lock() {
                            playback.dropping = false;
                            playback.relayed = 0;
                        }
                    }
                    Some("interrupted") => {
                        if let Ok(mut playback) = shared.playback.lock() {
                            let dropped = playback
                                .audio
                                .as_ref()
                                .map(|audio| audio.flush())
                                .unwrap_or(0);
                            playback.dropping = true;
                            // 24 samples per millisecond at 24 kHz.
                            event["playedMs"] =
                                json!(playback.relayed.saturating_sub(dropped) / 24);
                        }
                    }
                    _ => {}
                }
                VoiceInbound::Event(event)
            }
            closed => closed,
        };
        let _ = shared.sender.unbounded_send(VoiceMessage::Socket(inbound));
    })
}

/// Silences the agent at once (Hang up, Stop talking).
fn stop_playback(shared: &LinkShared) {
    if let Ok(mut playback) = shared.playback.lock() {
        if let Some(audio) = &playback.audio {
            audio.flush();
        }
        playback.dropping = true;
    }
}

/// Opens the echo-cancelled microphone and the speaker on a thread of their own (macOS may ask for
/// microphone access first), streaming the microphone into `socket` once `ready` arrived.
fn start_audio(socket: Arc<VoiceSocket>, shared: Arc<LinkShared>) {
    let _ = std::thread::Builder::new()
        .name("ghostex-voice-audio-start".into())
        .spawn(move || {
            let capture_shared = shared.clone();
            let changed_shared = shared.clone();
            let silent_shared = shared.clone();
            let started = VoiceAudio::start(
                true,
                true,
                Box::new(move |samples| {
                    if capture_shared.streaming.load(Ordering::Relaxed)
                        && !capture_shared.muted.load(Ordering::Relaxed)
                    {
                        let bytes = samples
                            .iter()
                            .flat_map(|sample| sample.to_le_bytes())
                            .collect();
                        socket.send_audio(bytes);
                    }
                }),
                Box::new(move || {
                    let _ = changed_shared
                        .sender
                        .unbounded_send(VoiceMessage::AudioDeviceChanged);
                }),
                Box::new(move || {
                    let _ = silent_shared
                        .sender
                        .unbounded_send(VoiceMessage::MicrophoneSilent);
                }),
            );
            let message = match started {
                Ok(audio) => {
                    audio.set_rate(f32::from_bits(shared.rate.load(Ordering::SeqCst)));
                    if let Ok(mut playback) = shared.playback.lock()
                        && !playback.closed
                    {
                        playback.audio = Some(Arc::new(audio));
                    }
                    VoiceMessage::AudioStarted
                }
                Err(error) => VoiceMessage::AudioFailed(error),
            };
            let _ = shared.sender.unbounded_send(message);
        });
}

/// "Grok" / "GPT Realtime", for "Grok · Eve".
pub(crate) fn provider_name(provider: &str) -> &'static str {
    match provider {
        "openai" => "GPT Realtime",
        _ => "Grok",
    }
}

/// "eve" → "Eve".
pub(crate) fn voice_name(voice: &str) -> String {
    let mut chars = voice.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn voice_setting_key(provider: &str) -> &'static str {
    match provider {
        "openai" => "voiceAgentOpenaiVoice",
        _ => "voiceAgentXaiVoice",
    }
}

/// `voiceAgentSpeed` as one of the offered speeds (1 when unset or unknown).
pub(crate) fn voice_agent_speed_setting(settings: &serde_json::Map<String, Value>) -> f32 {
    let saved = match settings.get(SPEED_SETTING) {
        Some(Value::String(text)) => text.trim().parse::<f32>().ok(),
        Some(Value::Number(number)) => number.as_f64().map(|speed| speed as f32),
        _ => None,
    };
    saved
        .and_then(|saved| {
            VOICE_AGENT_SPEEDS
                .into_iter()
                .find(|speed| (speed - saved).abs() < 0.01)
        })
        .unwrap_or(1.0)
}

/// The setting's stored value for a speed: "1", "1.2", … "2".
fn speed_setting_value(speed: f32) -> String {
    let text = format!("{speed:.1}");
    text.strip_suffix(".0").map(str::to_string).unwrap_or(text)
}

/// gxserver's sentence for an error frame (a missing key's names the selected provider), with the
/// panel's wording when it sent none.
fn error_sentence(event: &Value) -> String {
    let message = event["message"].as_str().map(str::trim).unwrap_or_default();
    match event["code"].as_str() {
        Some("noKey") if message.is_empty() => {
            "Add your API key in Settings › Voice agent to talk to the voice agent.".to_string()
        }
        Some("badKey") if message.is_empty() => {
            "Your API key was refused. Check it in Settings › Voice agent.".to_string()
        }
        Some("badKey") => format!("{message} Check it in Settings › Voice agent."),
        Some("providerUnavailable") if message.is_empty() => {
            "The voice provider can't be reached right now. Try again in a moment.".to_string()
        }
        _ if !message.is_empty() => message.to_string(),
        _ => "The voice agent ran into a problem.".to_string(),
    }
}

/// Why a conversation ended, for the panel.
fn ended_sentence(reason: &str, message: Option<&str>) -> String {
    match reason {
        "idle" => "Hung up after 5 minutes with nobody talking.".to_string(),
        "providerClosed" => "The voice provider ended the conversation.".to_string(),
        "replaced" => "This conversation continued on another device.".to_string(),
        "serverShutdown" => {
            "Ghostex's service restarted, which ended the conversation.".to_string()
        }
        "detachedTooLong" => "The conversation ended while this window was away.".to_string(),
        _ => message
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .unwrap_or("The conversation ended.")
            .to_string(),
    }
}
