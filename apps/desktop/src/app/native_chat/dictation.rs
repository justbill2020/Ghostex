//! The composer's Dictate control: the microphone records to a file first and the recording is
//! transcribed after Stop (`voice_agent::Dictate`, gxserver's `/api/transcribeDictation`); the text
//! is typed into the draft. While it records or transcribes, a row above the box shows it with an
//! ✕ that cancels (Escape cancels too); a failed transcription stays in that row with Transcribe
//! again. Hovering the microphone when a recording has no text yet offers Transcribe previous.
//!
//! CDXC:SessionChat 2026-10-08 DECISION: User asked for voice input in the chat box, to the left of the Terminal View button, on the desktop and the phone (GPUI Kit's speech input, #3333, on the desktop). Pressing the microphone starts listening and pressing it again stops; the recognized text is typed into the draft and never sent by itself.
//! CDXC:SessionChat 2026-10-08 SEE-ALSO: `apps/mobile/app/src/chat/native/composer/useDictation.ts` is the phone's control; the toolbar order is `toolbar.rs` `COMPOSER_CONTROLS` and the phone's `menus.ts`.
//! CDXC:VoiceMode 2026-10-20 SEE-ALSO: Escape, the Transcribing ✕, keeping cancelled recordings and Transcribe previous are the user's decision in `apps/desktop/src/app/voice_agent/dictation.rs`.

use super::appearance::{ChatAppearance, ChatFilledButton as _};
use super::cursor::ChatCursor as _;
use super::state::{NativeChatEvent, NativeChatView};
use crate::app::voice_agent::{Dictate, DictateEvent, DictatePhase};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityInputHandler as _,
    InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, px,
};
use gpui_component::speech::{SpeechError, SpeechState};
use serde_json::json;
use std::time::Duration;

/// The one switch for both Dictate buttons, the chat box's and the terminal bar's.
///
/// CDXC:VoiceMode 2026-10-20 DECISION: User: Dictate uses Grok dictation (xAI speech-to-text through gxserver, with the key kept on this computer) instead of the system speech recognizer. Later the same day the user said "Please make the voice agent feature and the dictating feature and all of those features related to voice mode basically there on Linux and on Windows. They're not Mac only, you know what I mean?" so Dictate is shown on macOS, Windows and Linux, superseding "Windows stays hidden" (and the 2026-10-09 decision that hid both Dictate buttons on every OS because the Windows system recognizer never returned words).
/// CDXC:SessionChat 2026-10-09 SEE-ALSO: the phone's switch is `DICTATION_ENABLED` in `apps/mobile/app/src/chat/native/composer/useDictation.ts`.
pub(crate) const DICTATION_ENABLED: bool = true;

/// Whether this build can dictate: the desktop app on macOS, Windows and Linux, where the app's own
/// audio captures the microphone (`voice_agent/audio.rs`). The web build has no microphone, so the
/// control is not drawn there.
pub(super) const DICTATION: bool = DICTATION_ENABLED
    && cfg!(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    ));

/// How long the pointer rests on the microphone before Transcribe previous is offered.
const HOVER_DELAY: Duration = Duration::from_millis(350);

/// The app toast for a dictation problem no composer row shows (the microphone refused, nothing
/// waiting for Transcribe previous), from the composer or the terminal bar.
///
/// CDXC:SessionChat 2026-10-09 WHY: User: "pls fix dictate feature it doesnt work at all (at least on windows)". A `sessionChatHostAction` "toast" is dropped by the app, so pressing the microphone did nothing visible. This raw `{type: "toast"}` message reaches the app's toast window from both buttons.
pub(crate) fn dictation_toast(title: &str, message: &str) -> serde_json::Value {
    json!({
        "type": "toast",
        "level": "error",
        "title": title,
        "description": message,
        "durationMs": 12_000,
    })
}

/// Where to allow the microphone when the system refused it (the system recognizer's wording).
const MICROPHONE_ACCESS_HINT: &str = if cfg!(target_os = "windows") {
    "Ghostex can't use the microphone. Turn on Microphone access and Let desktop apps access your microphone in Windows Settings > Privacy & security > Microphone."
} else {
    "Ghostex can't use the microphone. Allow Ghostex under Microphone in System Settings > Privacy & Security."
};

/// What to install when the system cannot recognize its language.
const SPEECH_LANGUAGE_HINT: &str = if cfg!(target_os = "windows") {
    "Windows has no speech recognition for your language. Add a speech language in Windows Settings > Time & language > Speech."
} else {
    "Your Mac can't recognize your language on the device. Check that Dictation is on in System Settings > Keyboard."
};

/// The system speech recognizer's failure toast. Unused with [`system_recognizer_speech_state`].
#[allow(dead_code)]
pub(crate) fn dictation_failure_toast(error: &SpeechError) -> serde_json::Value {
    let description = match error {
        SpeechError::PermissionDenied => MICROPHONE_ACCESS_HINT.to_owned(),
        SpeechError::NoInputDevice => {
            "No microphone was found. Connect one, or choose an input device in your sound settings."
                .to_owned()
        }
        SpeechError::Unsupported => SPEECH_LANGUAGE_HINT.to_owned(),
        SpeechError::Recognizer(error) => format!("{error:#}"),
        SpeechError::Input(error) => format!("The microphone stopped working: {error:#}"),
    };
    dictation_toast("Dictation failed", &description)
}

/// The system speech recognizer's dictation (GPUI Kit's `SystemRecognizer`), which Dictate used
/// before Grok dictation.
///
/// CDXC:VoiceMode 2026-10-20 DECISION: User: "keep but mark the old code as unused". Unused since 2026-10-20: Dictate records and transcribes through `voice_agent::Dictate`. Returning to the system recognizer means driving a `SpeechState` from this again at the two Dictate buttons (here and `render/terminal_agent_action_bar/dictation.rs`).
#[allow(dead_code)]
pub(crate) fn system_recognizer_speech_state(cx: &mut Context<SpeechState>) -> SpeechState {
    SpeechState::new(cx)
}

/// The composer's Dictate state, made the first time the reader uses it.
pub(crate) struct DictationControl {
    dictate: Entity<Dictate>,
    _subscription: Subscription,
    /// The pointer is on the microphone; the hover menu opens after [`HOVER_DELAY`].
    hover: Option<Task<()>>,
}

pub(crate) type Dictation = Option<DictationControl>;

impl NativeChatView {
    fn dictate(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Dictate> {
        if let Some(control) = &self.dictation {
            return control.dictate.clone();
        }
        let dictate = cx.new(|_| Dictate::new());
        let subscription =
            cx.subscribe_in(&dictate, window, |this, _, event, window, cx| match event {
                DictateEvent::Text(text) => this.insert_dictation(text, window, cx),
                DictateEvent::Failure { title, message } => {
                    // A failed transcription shows in the row above the box with Transcribe
                    // again; only what no row shows becomes a toast.
                    let in_row = this.dictation.as_ref().is_some_and(|control| {
                        matches!(
                            control.dictate.read(cx).phase(),
                            DictatePhase::Failed { .. }
                        )
                    });
                    if !in_row {
                        cx.emit(NativeChatEvent::Host(dictation_toast(title, message)));
                    }
                    cx.notify();
                }
            });
        cx.observe(&dictate, |_, _, cx| cx.notify()).detach();
        self.dictation = Some(DictationControl {
            dictate: dictate.clone(),
            _subscription: subscription,
            hover: None,
        });
        dictate
    }

    fn dictate_phase(&self, cx: &App) -> Option<DictatePhase> {
        self.dictation
            .as_ref()
            .map(|control| control.dictate.read(cx).phase().clone())
    }

    /// Whether the microphone is recording (the Stop glyph).
    pub(super) fn dictating(&self, cx: &App) -> bool {
        self.dictation
            .as_ref()
            .is_some_and(|control| control.dictate.read(cx).recording())
    }

    /// Start recording, or stop and transcribe.
    pub(super) fn toggle_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dictate = self.dictate(window, cx);
        dictate.update(cx, |dictate, cx| dictate.toggle(cx));
        cx.notify();
    }

    /// Escape: cancels a recording or a transcription (the recording is kept). Answers whether
    /// there was one.
    pub(super) fn cancel_dictation(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(control) = &self.dictation else {
            return false;
        };
        let dictate = control.dictate.clone();
        let active = {
            let dictate = dictate.read(cx);
            dictate.recording() || dictate.transcribing()
        };
        if active {
            dictate.update(cx, |dictate, cx| dictate.cancel(cx));
        }
        active
    }

    pub(super) fn transcribe_previous_dictation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dictate = self.dictate(window, cx);
        dictate.update(cx, |dictate, cx| dictate.transcribe_previous(cx));
    }

    /// Settings › Voice agent, scrolled to Dictation history.
    pub(super) fn open_dictation_history(&mut self, cx: &mut Context<Self>) {
        cx.emit(NativeChatEvent::Host(json!({
            "type":"open","modal":"settings","initialTab":"voiceAgent","initialSection":"dictations",
        })));
    }

    /// The microphone's menu: Transcribe previous when a recording has no text yet, and the
    /// history. Opened by resting the pointer on the microphone (only when there is something to
    /// transcribe) or by right-clicking it.
    pub(super) fn show_dictation_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut rows = Vec::new();
        if let Some((_, duration_ms, created_at)) = crate::app::voice_agent::previous_recording() {
            let ago = chrono::Utc::now()
                .timestamp_millis()
                .saturating_sub(created_at)
                / 60_000;
            let when = match ago {
                0 => "just now".to_string(),
                1 => "1 min ago".to_string(),
                minutes if minutes < 60 => format!("{minutes} min ago"),
                minutes => format!("{} h ago", minutes / 60),
            };
            rows.push(json!({
                "label": format!(
                    "Transcribe previous ({}, {when})",
                    crate::app::voice_agent::dictation_clock(Duration::from_millis(duration_ms))
                ),
                "command": {"type":"composerHost","action":"transcribePreviousDictation"},
            }));
        }
        rows.push(json!({
            "label": "Dictation history…",
            "command": {"type":"composerHost","action":"dictationHistory"},
        }));
        self.menu_side = Some(super::option_menu::MenuSide::Right);
        let trigger =
            super::toolbar::button_trigger(self.composer_button_bounds.dictate.get(), position);
        self.show_chat_menu(rows, trigger, 260.0, window, cx);
    }

    /// The pointer entered or left the microphone.
    pub(super) fn hover_dictation(
        &mut self,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !DICTATION {
            return;
        }
        let dictate = self.dictate(window, cx);
        let idle = matches!(
            dictate.read(cx).phase(),
            DictatePhase::Idle | DictatePhase::Failed { .. }
        );
        if !hovered || !idle {
            if let Some(control) = self.dictation.as_mut() {
                control.hover = None;
            }
            return;
        }
        // The hover asks gxserver what is waiting, so a recording the phone or the terminal bar
        // left is offered too.
        let refresh = cx
            .background_executor()
            .spawn(async { crate::app::voice_agent::refresh_previous() });
        let task = cx.spawn_in(window, async move |this, cx| {
            refresh.await;
            cx.background_executor().timer(HOVER_DELAY).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let still_hovered = this
                    .dictation
                    .as_ref()
                    .is_some_and(|control| control.hover.is_some());
                if still_hovered && crate::app::voice_agent::previous_recording().is_some() {
                    let position = window.mouse_position();
                    this.show_dictation_menu(position, window, cx);
                }
                if let Some(control) = this.dictation.as_mut() {
                    control.hover = None;
                }
            });
        });
        if let Some(control) = self.dictation.as_mut() {
            control.hover = Some(task);
        }
    }

    /// The row above the box while Dictate records, transcribes or failed.
    pub(super) fn render_dictation_row(
        &self,
        p: &ChatAppearance,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let phase = self.dictate_phase(cx)?;
        let s = p.scale;
        let (dot, label, detail, failed) = match &phase {
            DictatePhase::Idle => return None,
            DictatePhase::Starting => (false, "Starting the microphone…".to_string(), None, false),
            DictatePhase::Recording => {
                let elapsed = self
                    .dictation
                    .as_ref()
                    .map(|control| control.dictate.read(cx).elapsed())
                    .unwrap_or_default();
                (
                    true,
                    format!(
                        "Recording {}",
                        crate::app::voice_agent::dictation_clock(elapsed)
                    ),
                    Some("Esc cancels".to_string()),
                    false,
                )
            }
            DictatePhase::Transcribing => (false, "Transcribing…".to_string(), None, false),
            DictatePhase::Failed { message, .. } => (
                false,
                "Transcription failed.".to_string(),
                Some(format!("{message} Your recording is kept.")),
                true,
            ),
        };
        let button = |id: &'static str, text: &'static str, cx: &mut Context<Self>| {
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(text)
                .chat_cursor_pointer()
                .flex_shrink_0()
                .px(px(8.0 * s))
                .py(px(3.0 * s))
                .rounded(px(6.0 * s))
                .border_1()
                .chat_filled(p.neutral_button_fills(), true)
                .text_color(p.primary)
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let Some(control) = this.dictation.as_ref() else {
                        return;
                    };
                    let dictate = control.dictate.clone();
                    match id {
                        "dictation-retry" => dictate.update(cx, |dictate, cx| dictate.retry(cx)),
                        _ => dictate.update(cx, |dictate, cx| dictate.cancel(cx)),
                    }
                    cx.notify();
                }))
        };
        let close = div()
            .id("dictation-cancel")
            .role(gpui::Role::Button)
            .aria_label(if failed { "Dismiss" } else { "Cancel" })
            .chat_cursor_pointer()
            .flex_shrink_0()
            .size(px(20.0 * s))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .hover(|style| style.bg(p.border))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(if failed {
                    "Dismiss (the recording is kept)"
                } else {
                    "Cancel (the recording is kept)"
                })
                .build(window, cx)
            })
            .child(
                gpui::svg()
                    .path("titlebar/x.svg")
                    .size(px(13.0 * s))
                    .text_color(p.muted),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(control) = this.dictation.as_ref() {
                    let dictate = control.dictate.clone();
                    dictate.update(cx, |dictate, cx| dictate.cancel(cx));
                }
                cx.notify();
            }));
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(8.0 * s))
                .when(dot, |row| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .size(px(8.0 * s))
                            .rounded_full()
                            .bg(gpui::rgb(0xef4444)),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .gap_x(px(6.0 * s))
                        .child(
                            div()
                                .text_color(if failed {
                                    gpui::rgb(0xef9999).into()
                                } else {
                                    p.primary
                                })
                                .child(label),
                        )
                        .when_some(detail, |row, detail| {
                            row.child(div().text_color(p.muted).child(detail))
                        }),
                )
                .when(failed, |row| {
                    row.child(button("dictation-retry", "Transcribe again", cx))
                })
                .child(close)
                .into_any_element(),
        )
    }

    /// Type a finished dictation into the draft, a space apart from the text before it.
    fn insert_dictation(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() {
            cx.notify();
            return;
        }
        let spaced = if self.draft.is_empty() || self.draft.ends_with(char::is_whitespace) {
            text.to_owned()
        } else {
            format!(" {text}")
        };
        if let Some(input) = self.input.clone() {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.replace_text_in_range(None, &spaced, window, cx);
            });
        }
        cx.notify();
    }
}
