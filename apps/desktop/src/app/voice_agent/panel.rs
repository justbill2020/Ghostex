//! The voice panel: a small native child window, first under the work area header's top right,
//! that shows the open conversation's state, live captions and tool lines, the voice (click it to
//! pick another) and the speaking speed, with Mute, Stop talking and Hang up, and says why a
//! conversation stopped.
//!
//! CDXC:VoiceMode 2026-10-20 WHY: the conversation keeps going while the user moves between projects and chats, so the panel belongs to the main window, not to a chat or a view: it is an owned pop-up window attached as an AppKit child of the main window (the app toast window's pattern), which moves with the window, never takes focus, and overlaps nothing interactive of its own. A size change of the main window places it again.
//! CDXC:VoiceMode 2026-10-20 DECISION: User: "the call screen should be movable and scrollable during voice mode." The panel's header is its drag handle: dragging it moves the panel's own window (`move_child_window`), kept inside the main window and remembered for the rest of the app's run, and the transcript scrolls with the wheel and trackpad. A window too small for the panel shrinks it, down to a floor.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, CursorStyle, DispatchPhase, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions, canvas, div, point, px, rgba, size,
};
use gpui_component::{h_flex, v_flex};

use super::conversation::{
    VOICE_AGENT_SPEEDS, VoicePhase, VoiceProblem, VoiceRole, VoiceRow, provider_name, voice_name,
};
use super::memory::{VoiceMemory, VoiceMemoryState};
use crate::app::helpers::*;
#[cfg(not(target_os = "windows"))]
use crate::app::native_chat::child_window::move_child_window;
use crate::app::render::workarea_header::workarea_header_bottom_y;
use crate::app::window::settings_modal::catalog::settings_catalog;
use crate::app::window::*;
use crate::*;

const PANEL_WIDTH: f32 = 340.0;
const PANEL_HEIGHT: f32 = 360.0;
/// The smallest the panel shrinks to in a small window.
const PANEL_MIN_WIDTH: f32 = 260.0;
const PANEL_MIN_HEIGHT: f32 = 220.0;
/// From the window's trailing edge and below the header.
const PANEL_MARGIN: f32 = 12.0;
/// The closest a dragged panel comes to the window's edges.
const PANEL_EDGE: f32 = 6.0;
/// A press on the header that moves less than this is not a drag.
const DRAG_THRESHOLD: f32 = 3.0;
/// Within this distance of the transcript's end, new lines keep it scrolled to the end.
const FOLLOW_SLACK: f32 = 24.0;
const SPEED_LABELS: [&str; 6] = ["1×", "1.2×", "1.4×", "1.6×", "1.8×", "2×"];
const PANEL_RADIUS: f32 = 12.0;
/// How long the main window's size must hold before the panel is placed again.
const REPOSITION_DELAY: std::time::Duration = std::time::Duration::from_millis(160);

const ICON_MIC: &str = "titlebar/microphone.svg";
const ICON_MIC_OFF: &str = "titlebar/microphone-off.svg";
const ICON_HANG_UP: &str = "titlebar/phone-off.svg";
const ICON_STOP: &str = "titlebar/player-stop.svg";
const ICON_TOOL: &str = "titlebar/tool.svg";
const ICON_CHECK: &str = "titlebar/check.svg";
const ICON_ALERT: &str = "titlebar/alert-triangle.svg";
const ICON_CLOSE: &str = "titlebar/x.svg";
const ICON_SETTINGS: &str = "titlebar/settings.svg";
const ICON_HEADSET: &str = "titlebar/headset.svg";
const ICON_CHEVRON: &str = "titlebar/chevron-down.svg";
const ICON_HISTORY: &str = "titlebar/history.svg";
const ICON_TRASH: &str = "titlebar/trash.svg";
const ICON_FRESH: &str = "titlebar/refresh.svg";
/// The remembered conversation's accent (the speaking violet).
const MEMORY_ACCENT: u32 = 0xa78bfaff;

/// The panel's colours for the chrome's appearance.
struct PanelColors {
    background: Hsla,
    border: Hsla,
    ink: Hsla,
    text: Hsla,
    muted: Hsla,
}

impl PanelColors {
    fn resolve() -> Self {
        let light = chrome_uses_light_appearance();
        let ink: Hsla = if light { gpui::black() } else { gpui::white() };
        Self {
            background: if light {
                rgba(0xfafafaf7).into()
            } else {
                rgba(0x1e1e21f7).into()
            },
            border: ink.opacity(if light { 0.10 } else { 0.12 }),
            ink,
            text: ink.opacity(0.90),
            muted: ink.opacity(0.55),
        }
    }
}

/// The state's colour dot: green while it listens, blue while you talk, amber while it works,
/// violet while it speaks.
fn state_color(state: &str) -> Hsla {
    match state {
        "listening" => rgba(0x4ade80ff),
        "userSpeaking" => rgba(0x38bdf8ff),
        "thinking" | "toolRunning" => rgba(0xfbbf24ff),
        "speaking" => rgba(0xa78bfaff),
        _ => rgba(0x9ca3afff),
    }
    .into()
}

/// What the panel draws, copied out of the app and pushed into the window.
///
/// CDXC:VoiceMode 2026-10-20 WHY: the panel must never read the app entity while rendering: `open_window` renders the new window at once, inside the app's own update (the Voice button's click), and reading the app there panics with "already being updated" and aborts the app.
struct PanelSnapshot {
    phase: VoicePhase,
    state: String,
    tool_label: Option<String>,
    rows: Vec<VoiceRow>,
    muted: bool,
    problem: Option<VoiceProblem>,
    provider: Option<String>,
    voice: Option<String>,
    next_voice: Option<String>,
    /// The provider's voices as (id, label), for the voice list.
    voice_options: Vec<(String, String)>,
    voice_picker_open: bool,
    speed: f32,
    light: bool,
    memory: Option<VoiceMemory>,
    memory_state: VoiceMemoryState,
    memory_details_open: bool,
}

/// A drag of the panel by its header: where the mouse went down, in screen points.
struct PanelDrag {
    mouse_start: Point<Pixels>,
    moved: bool,
}

pub(crate) struct VoicePanelWindow {
    app: WeakEntity<GhostexGpuiApp>,
    snapshot: PanelSnapshot,
    scroll: ScrollHandle,
    drag: Option<PanelDrag>,
}

/// The panel's frame in the main window's content coordinates, for a window of `main` size, at
/// the place the user dragged it to (its gap to the trailing edge, its top) or under the header's
/// top right.
fn panel_frame(main: gpui::Size<Pixels>, place: Option<Point<Pixels>>) -> Bounds<Pixels> {
    let (main_width, main_height) = (main.width.as_f32(), main.height.as_f32());
    let width = PANEL_WIDTH
        .min(main_width - PANEL_EDGE * 2.0)
        .max(PANEL_MIN_WIDTH);
    let height = PANEL_HEIGHT
        .min(main_height - workarea_header_bottom_y() - PANEL_MARGIN)
        .max(PANEL_MIN_HEIGHT);
    let (trailing, top) = match place {
        Some(place) => (place.x.as_f32(), place.y.as_f32()),
        None => (
            PANEL_MARGIN,
            workarea_header_bottom_y() + PANEL_MARGIN / 2.0,
        ),
    };
    let x = (main_width - trailing - width)
        .min(main_width - width - PANEL_EDGE)
        .max(PANEL_EDGE);
    let y = top.min(main_height - height - PANEL_EDGE).max(PANEL_EDGE);
    Bounds {
        origin: point(px(x), px(y)),
        size: size(px(width), px(height)),
    }
}

/// The provider's voices with their Settings labels ("Eve"), in the order gxserver listed them.
fn voice_choices(provider: &str, voices: &[String]) -> Vec<(String, String)> {
    let key = match provider {
        "openai" => "voiceAgentOpenaiVoice",
        _ => "voiceAgentXaiVoice",
    };
    let options = settings_catalog()
        .extra_tab("voiceAgent")
        .and_then(|tab| {
            tab.sections
                .iter()
                .flat_map(|section| section.settings.iter())
                .find(|row| row.key == key)
        })
        .map(|row| row.options.clone())
        .unwrap_or_default();
    voices
        .iter()
        .map(|voice| {
            let label = options
                .iter()
                .find(|option| option.value == *voice)
                .map(|option| option.label.clone())
                .unwrap_or_else(|| voice_name(voice));
            (voice.clone(), label)
        })
        .collect()
}

impl GhostexGpuiApp {
    /// Opens, refreshes or closes the panel to match the conversation, and redraws the Voice
    /// button.
    pub(crate) fn sync_voice_agent_panel(&mut self, cx: &mut gpui::Context<Self>) {
        cx.notify();
        if self.voice_agent.phase == VoicePhase::Idle {
            self.voice_agent.panel_reposition = None;
            self.voice_agent.panel_placed_for = None;
            if let Some(panel) = self.voice_agent.panel.take() {
                let _ = panel.update(cx, |_, window, _| window.remove_window());
            }
            return;
        }
        if let Some(panel) = self.voice_agent.panel {
            let snapshot = self.voice_panel_snapshot();
            let updated = panel.update(cx, |panel, _, cx| {
                panel.receive(snapshot);
                cx.notify();
            });
            if updated.is_ok() {
                return;
            }
            self.voice_agent.panel = None;
        }
        self.open_voice_agent_panel_window(cx);
    }

    fn open_voice_agent_panel_window(&mut self, cx: &mut gpui::Context<Self>) {
        let main = self.main_window_bounds;
        let frame = panel_frame(main.size, self.voice_agent.panel_place);
        let bounds = Bounds {
            origin: main.origin + frame.origin,
            size: frame.size,
        };
        let app = cx.entity().downgrade();
        let snapshot = self.voice_panel_snapshot();
        let main_window_native_view = self.parent_ns_view;
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            display_id: self.main_window_popup_owner().display_for(bounds, cx),
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            titlebar: None,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };
        self.voice_agent.panel = cx
            .open_window(options, move |panel_window, cx| {
                panel_window.set_background_corner_radius(px(PANEL_RADIUS));
                remove_gpui_app_toast_popup_window_chrome(panel_window, main_window_native_view);
                cx.new(|_| {
                    let scroll = ScrollHandle::new();
                    scroll.scroll_to_bottom();
                    VoicePanelWindow {
                        app,
                        snapshot,
                        scroll,
                        drag: None,
                    }
                })
            })
            .ok();
        self.voice_agent.panel_placed_for = Some(main.size);
    }

    /// Places the panel again after the main window changed size (it follows moves by itself, as
    /// an AppKit child window). Called from the main window's render.
    pub(crate) fn voice_agent_follow_main_window(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(placed_for) = self.voice_agent.panel_placed_for else {
            return;
        };
        if self.voice_agent.panel.is_none()
            || placed_for == self.main_window_bounds.size
            || self.voice_agent.panel_reposition.is_some()
        {
            return;
        }
        self.voice_agent.panel_reposition = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REPOSITION_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                this.voice_agent.panel_reposition = None;
                if this.voice_agent.panel_placed_for == Some(this.main_window_bounds.size) {
                    return;
                }
                this.place_voice_agent_panel(cx);
            });
        }));
    }

    /// Moves the open panel to its place for the main window's current size.
    fn place_voice_agent_panel(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(panel) = self.voice_agent.panel else {
            return;
        };
        let size = self.main_window_bounds.size;
        let frame = panel_frame(size, self.voice_agent.panel_place);
        if move_voice_panel_window(
            panel.into(),
            self.parent_ns_view,
            self.main_window_bounds,
            frame,
            cx,
        ) {
            self.voice_agent.panel_placed_for = Some(size);
        } else {
            // This platform cannot move a child window: open it again in place.
            self.voice_agent.panel = None;
            let _ = panel.update(cx, |_, window, _| window.remove_window());
            if self.voice_agent.phase != VoicePhase::Idle {
                self.open_voice_agent_panel_window(cx);
            }
        }
    }

    /// A drag of the panel's header, `delta` from where it started: moves the panel by it, kept
    /// inside the main window, and remembers the place.
    fn drag_voice_agent_panel(
        &mut self,
        delta: Point<Pixels>,
        first: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        let main = self.main_window_bounds.size;
        let frame = panel_frame(main, self.voice_agent.panel_place);
        if first || self.voice_agent.panel_drag_from.is_none() {
            self.voice_agent.panel_drag_from = Some(frame.origin);
        }
        let Some(from) = self.voice_agent.panel_drag_from else {
            return;
        };
        let wanted = from + delta;
        let placed = panel_frame(
            main,
            Some(point(main.width - wanted.x - frame.size.width, wanted.y)),
        );
        self.voice_agent.panel_place = Some(point(
            main.width - placed.origin.x - placed.size.width,
            placed.origin.y,
        ));
        self.place_voice_agent_panel(cx);
    }

    fn voice_panel_snapshot(&self) -> PanelSnapshot {
        let agent = &self.voice_agent;
        PanelSnapshot {
            phase: agent.phase,
            state: agent.state.clone(),
            tool_label: agent.tool_label.clone(),
            rows: agent.rows.clone(),
            muted: agent.muted,
            problem: agent.problem.clone(),
            provider: agent.provider.clone(),
            voice: agent.voice.clone(),
            next_voice: agent.next_voice.clone(),
            voice_options: voice_choices(
                agent.provider.as_deref().unwrap_or_default(),
                &agent.voice_options,
            ),
            voice_picker_open: agent.voice_picker_open,
            speed: agent.speed,
            light: chrome_uses_light_appearance(),
            memory: agent.memory.clone(),
            memory_state: agent.memory_state,
            memory_details_open: agent.memory_details_open,
        }
    }
}

/// Moves the open panel to `frame`, in the main window's content coordinates (`main` is the main
/// window's bounds). False when this platform cannot move it.
///
/// CDXC:VoiceMode 2026-10-20 WHY: `move_child_window` cannot move a window on Windows, and its callers open the window again instead, which would end a drag of the panel at its first step. On Windows the panel is moved with `SetWindowPos` from a task, the Docs drawer's way (`native_docs/drawer.rs`); moving the main window already carries the panel with it (`workspace_windows/owned_windows.rs`).
#[cfg(target_os = "windows")]
fn move_voice_panel_window(
    handle: gpui::AnyWindowHandle,
    _: *mut std::ffi::c_void,
    main: Bounds<Pixels>,
    frame: Bounds<Pixels>,
    cx: &mut gpui::App,
) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos,
    };
    let Ok(Some((hwnd, scale))) = handle.update(cx, |_, window, _| {
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some((handle.hwnd.get(), window.scale_factor())),
            _ => None,
        }
    }) else {
        return false;
    };
    let screen = Bounds::new(main.origin + frame.origin, frame.size);
    // `SetWindowPos` delivers `WM_SIZE` at once, and GPUI's resize callback needs the app free.
    cx.spawn(async move |_| {
        let device = |value: Pixels| (f32::from(value) * scale).round() as i32;
        unsafe {
            SetWindowPos(
                hwnd as *mut std::ffi::c_void,
                std::ptr::null_mut(),
                device(screen.origin.x),
                device(screen.origin.y),
                device(screen.size.width),
                device(screen.size.height),
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
            );
        }
    })
    .detach();
    true
}

#[cfg(not(target_os = "windows"))]
fn move_voice_panel_window(
    handle: gpui::AnyWindowHandle,
    parent: *mut std::ffi::c_void,
    _: Bounds<Pixels>,
    frame: Bounds<Pixels>,
    cx: &mut gpui::App,
) -> bool {
    move_child_window(handle, parent, frame, cx)
}

/// How much a row says, so a growing caption counts as new content.
fn row_text_len(row: &VoiceRow) -> usize {
    match row {
        VoiceRow::Caption { text, .. } | VoiceRow::Announcement { text } => text.len(),
        VoiceRow::Tool { label, ok, .. } => label.len() + usize::from(ok.is_some()),
    }
}

/// What a panel control asks the app to do.
#[derive(Clone)]
enum PanelAction {
    Mute,
    StopTalking,
    HangUp,
    Close,
    StartAgain,
    OpenSettings,
    ToggleVoicePicker,
    PickVoice(String),
    Speed(f32),
    DragBy { delta: Point<Pixels>, first: bool },
    ContinueMemory,
    StartFresh,
    ForgetMemory,
    ToggleMemoryDetails,
}

impl VoicePanelWindow {
    /// Takes the app's new snapshot, keeping the transcript at its end when it was there.
    fn receive(&mut self, snapshot: PanelSnapshot) {
        let grew = snapshot.rows.len() != self.snapshot.rows.len()
            || snapshot.rows.last().map(row_text_len)
                != self.snapshot.rows.last().map(row_text_len);
        let following = -self.scroll.offset().y >= self.scroll.max_offset().y - px(FOLLOW_SLACK);
        if (grew && following) || (self.snapshot.voice_picker_open && !snapshot.voice_picker_open) {
            self.scroll.scroll_to_bottom();
        }
        self.snapshot = snapshot;
    }

    fn run(&mut self, action: PanelAction, cx: &mut Context<Self>) {
        let app = self.app.clone();
        // Deferred: the app's answer updates (or removes) this very window.
        cx.defer(move |cx| {
            let _ = app.update(cx, |app, cx| match action {
                PanelAction::ToggleVoicePicker => app.toggle_voice_agent_voice_picker(cx),
                PanelAction::PickVoice(voice) => app.pick_voice_agent_voice(voice, cx),
                PanelAction::Speed(speed) => app.set_voice_agent_speed(speed, cx),
                PanelAction::DragBy { delta, first } => {
                    app.drag_voice_agent_panel(delta, first, cx)
                }
                PanelAction::ContinueMemory => app.continue_voice_agent_memory(cx),
                PanelAction::StartFresh => app.start_voice_agent_fresh(cx),
                PanelAction::ForgetMemory => app.forget_voice_agent_memory(cx),
                PanelAction::ToggleMemoryDetails => app.toggle_voice_agent_memory_details(cx),
                PanelAction::Mute => app.toggle_voice_agent_mute(cx),
                PanelAction::StopTalking => app.interrupt_voice_agent(cx),
                PanelAction::HangUp => app.hang_up_voice_agent(cx),
                PanelAction::Close => app.close_voice_agent_panel(cx),
                PanelAction::StartAgain => {
                    app.close_voice_agent_panel(cx);
                    app.start_voice_agent_conversation(cx);
                }
                PanelAction::OpenSettings => {
                    app.close_voice_agent_panel(cx);
                    app.open_gpui_settings_voice_agent_page(cx);
                }
            });
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn button(
        &self,
        id: &'static str,
        icon: &'static str,
        label: Option<&'static str>,
        action: PanelAction,
        colors: &PanelColors,
        danger: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (fill, hover, foreground): (Hsla, Hsla, Hsla) = if danger {
            (
                rgba(0xdc2626ff).into(),
                rgba(0xef4444ff).into(),
                gpui::white(),
            )
        } else {
            (
                colors.ink.opacity(0.07),
                colors.ink.opacity(0.13),
                colors.text,
            )
        };
        h_flex()
            .id(id)
            .role(gpui::Role::Button)
            .aria_label(SharedString::new_static(label.unwrap_or(id)))
            .h(px(28.0))
            .px(px(if label.is_some() { 10.0 } else { 7.0 }))
            .gap(px(6.0))
            .items_center()
            .rounded(px(8.0))
            .bg(fill)
            .text_color(foreground)
            .text_size(px(12.5))
            .cursor_default()
            .hover(move |this| this.bg(hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener({
                    let action = action.clone();
                    move |this, _: &MouseUpEvent, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                        this.run(action.clone(), cx);
                    }
                }),
            )
            .on_a11y_action(gpui::AccessibleAction::Click, {
                let panel = cx.entity().downgrade();
                move |_, _, cx| {
                    let _ = panel.update(cx, |this, cx| this.run(action.clone(), cx));
                }
            })
            .child(titlebar_svg_icon(icon, 14.0, foreground))
            .when_some(label, |this, label| this.child(label))
            .into_any_element()
    }

    fn header(
        &self,
        snapshot: &PanelSnapshot,
        colors: &PanelColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (label, busy): (String, bool) = match snapshot.phase {
            VoicePhase::Starting => ("Connecting…".to_string(), true),
            VoicePhase::Ending => ("Hanging up…".to_string(), true),
            VoicePhase::Ended => ("Voice agent stopped".to_string(), false),
            VoicePhase::Idle | VoicePhase::Live => match snapshot.state.as_str() {
                "listening" if snapshot.muted => ("Muted".to_string(), false),
                "listening" => ("Listening".to_string(), false),
                "userSpeaking" => ("Hearing you".to_string(), false),
                "thinking" => ("Thinking…".to_string(), true),
                "toolRunning" => (
                    snapshot
                        .tool_label
                        .clone()
                        .unwrap_or_else(|| "Working…".to_string()),
                    true,
                ),
                "speaking" => ("Speaking".to_string(), false),
                _ => ("Connecting…".to_string(), true),
            },
        };
        let indicator: AnyElement = if snapshot.phase == VoicePhase::Ended {
            titlebar_svg_icon(ICON_ALERT, 14.0, rgba(0xfbbf24ff).into()).into_any_element()
        } else if busy {
            canvas(
                |_, _, _| {},
                |bounds, _, window, _| {
                    window.request_animation_frame();
                    paint_agent_gui_loading_spinner(bounds, window);
                },
            )
            .size(px(12.0))
            .into_any_element()
        } else {
            let color = if snapshot.muted {
                colors.muted
            } else {
                state_color(&snapshot.state)
            };
            div()
                .size(px(9.0))
                .rounded_full()
                .bg(color)
                .into_any_element()
        };
        h_flex()
            .id("voice-panel-header")
            .h(px(36.0))
            .flex_shrink_0()
            .px(px(12.0))
            .gap(px(8.0))
            .items_center()
            .border_b_1()
            .border_color(colors.border)
            .cursor(if self.drag.as_ref().is_some_and(|drag| drag.moved) {
                CursorStyle::ClosedHand
            } else {
                CursorStyle::OpenHand
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    this.drag = Some(PanelDrag {
                        mouse_start: window.bounds().origin + event.position,
                        moved: false,
                    });
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .size(px(14.0))
                    .items_center()
                    .justify_center()
                    .child(indicator),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.text)
                    .child(label),
            )
            .child(self.voice_button(snapshot, colors, cx))
            .into_any_element()
    }

    /// "Grok · Eve ⌄": opens the voice list once the provider named its voices.
    fn voice_button(
        &self,
        snapshot: &PanelSnapshot,
        colors: &PanelColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = match (&snapshot.provider, &snapshot.voice) {
            (Some(provider), Some(voice)) => {
                format!("{} · {}", provider_name(provider), voice_name(voice))
            }
            _ => "Voice agent".to_string(),
        };
        let pickable =
            !snapshot.voice_options.is_empty() && matches!(snapshot.phase, VoicePhase::Live);
        let open = snapshot.voice_picker_open;
        let muted = colors.muted;
        let hover = colors.ink.opacity(0.08);
        h_flex()
            .id("voice-panel-voice")
            .flex_shrink_0()
            .h(px(24.0))
            .px(px(6.0))
            .gap(px(5.0))
            .items_center()
            .rounded(px(6.0))
            .text_size(px(11.5))
            .text_color(if open { colors.text } else { muted })
            .when(open, |this| this.bg(hover))
            .child(titlebar_svg_icon(ICON_HEADSET, 12.0, muted))
            .child(label)
            .when(pickable, |this| {
                this.role(gpui::Role::Button)
                    .aria_label(SharedString::new_static("Change the voice"))
                    .cursor_default()
                    .hover(move |this| this.bg(hover))
                    .child(titlebar_svg_icon(ICON_CHEVRON, 11.0, muted))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.run(PanelAction::ToggleVoicePicker, cx);
                        }),
                    )
                    .on_a11y_action(gpui::AccessibleAction::Click, {
                        let panel = cx.entity().downgrade();
                        move |_, _, cx| {
                            let _ = panel.update(cx, |this, cx| {
                                this.run(PanelAction::ToggleVoicePicker, cx)
                            });
                        }
                    })
            })
            .into_any_element()
    }

    /// The voice list that replaces the transcript while it is open.
    fn voice_picker(
        &self,
        snapshot: &PanelSnapshot,
        colors: &PanelColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = snapshot.voice.clone().unwrap_or_default();
        let hover = colors.ink.opacity(0.08);
        let rows = snapshot.voice_options.iter().map(|(voice, label)| {
            let selected = *voice == current;
            let next = snapshot.next_voice.as_deref() == Some(voice.as_str());
            let pick = PanelAction::PickVoice(voice.clone());
            h_flex()
                .id(SharedString::from(format!("voice-panel-voice-{voice}")))
                .role(gpui::Role::MenuItemRadio)
                .aria_label(SharedString::from(label.clone()))
                .aria_toggled(if selected {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .h(px(28.0))
                .flex_shrink_0()
                .px(px(10.0))
                .gap(px(8.0))
                .items_center()
                .rounded(px(6.0))
                .text_size(px(12.5))
                .text_color(colors.text)
                .cursor_default()
                .hover(move |this| this.bg(hover))
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                .when(next, |this| {
                    this.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(colors.muted)
                            .child("Next conversation"),
                    )
                })
                .when(selected, |this| {
                    this.child(titlebar_svg_icon(ICON_CHECK, 12.0, colors.text))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseDownEvent, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener({
                        let pick = pick.clone();
                        move |this, _: &MouseUpEvent, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.run(pick.clone(), cx);
                        }
                    }),
                )
                .on_a11y_action(gpui::AccessibleAction::Click, {
                    let panel = cx.entity().downgrade();
                    move |_, _, cx| {
                        let _ = panel.update(cx, |this, cx| this.run(pick.clone(), cx));
                    }
                })
        });
        v_flex()
            .id("voice-panel-voices")
            .role(gpui::Role::Menu)
            .aria_label(SharedString::new_static("Voices"))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(6.0))
            .gap(px(1.0))
            .children(rows)
            .into_any_element()
    }

    /// The speed buttons, 1x to 2x (`voiceAgentSpeed`).
    fn speed_row(&self, snapshot: &PanelSnapshot, cx: &mut Context<Self>) -> AnyElement {
        let palette = ModalPalette::resolve(snapshot.light, None);
        let items: Vec<ModalSegmentedItem> = SPEED_LABELS
            .iter()
            .map(|label| ModalSegmentedItem { icon: None, label })
            .collect();
        let selected = VOICE_AGENT_SPEEDS
            .iter()
            .position(|speed| (speed - snapshot.speed).abs() < 0.01)
            .unwrap_or(0);
        div()
            .flex_shrink_0()
            .px(px(10.0))
            .pb(px(8.0))
            .child(modal_segmented_control(
                &palette,
                "voice-panel-speed",
                &items,
                selected,
                |this: &mut Self, index, _, cx| {
                    if let Some(speed) = VOICE_AGENT_SPEEDS.get(index) {
                        this.run(PanelAction::Speed(*speed), cx);
                    }
                },
                cx,
            ))
            .into_any_element()
    }

    /// Follows a header drag with the panel's window, through moves outside the panel too (the
    /// press that started it keeps the mouse).
    fn drag_listener(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.on_mouse_event({
                    let panel = panel.clone();
                    move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let mouse = window.bounds().origin + event.position;
                        let pressed = event.pressed_button == Some(MouseButton::Left);
                        let _ = panel.update(cx, |this, cx| this.drag_moved(mouse, pressed, cx));
                    }
                });
                window.on_mouse_event({
                    let panel = panel.clone();
                    move |_: &MouseUpEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let _ = panel.update(cx, |this, cx| {
                            if this.drag.take().is_some() {
                                cx.notify();
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    fn drag_moved(&mut self, mouse: Point<Pixels>, pressed: bool, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        if !pressed {
            self.drag = None;
            cx.notify();
            return;
        }
        let delta = mouse - drag.mouse_start;
        if !drag.moved && delta.x.abs().max(delta.y.abs()) < px(DRAG_THRESHOLD) {
            return;
        }
        let first = !drag.moved;
        if first {
            drag.moved = true;
            cx.notify();
        }
        self.run(PanelAction::DragBy { delta, first }, cx);
    }

    fn row(row: &VoiceRow, colors: &PanelColors) -> AnyElement {
        match row {
            VoiceRow::Caption {
                role, text, cut, ..
            } => {
                let you = *role == VoiceRole::User;
                v_flex()
                    .gap(px(1.0))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.muted)
                            .child(if you { "You" } else { "Agent" }),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .line_height(px(17.0))
                            .text_color(if you {
                                colors.text.opacity(0.78)
                            } else {
                                colors.text
                            })
                            .child(if *cut {
                                format!("{text}…")
                            } else {
                                text.clone()
                            }),
                    )
                    .into_any_element()
            }
            VoiceRow::Tool { label, ok, .. } => h_flex()
                .gap(px(6.0))
                .items_center()
                .text_size(px(11.5))
                .text_color(colors.muted)
                .child(titlebar_svg_icon(ICON_TOOL, 12.0, colors.muted))
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                .when(*ok == Some(true), |this| {
                    this.child(titlebar_svg_icon(ICON_CHECK, 12.0, rgba(0x4ade80ff).into()))
                })
                .when(*ok == Some(false), |this| {
                    this.child(titlebar_svg_icon(ICON_CLOSE, 12.0, rgba(0xf87171ff).into()))
                })
                .into_any_element(),
            VoiceRow::Announcement { text } => div()
                .text_size(px(11.5))
                .italic()
                .text_color(colors.muted)
                .child(text.clone())
                .into_any_element(),
        }
    }

    fn body(
        &self,
        snapshot: &PanelSnapshot,
        colors: &PanelColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if snapshot.voice_picker_open {
            return self.voice_picker(snapshot, colors, cx);
        }
        let rows = v_flex()
            .id("voice-panel-rows")
            .role(gpui::Role::Log)
            .aria_label(SharedString::new_static("Conversation"))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(10.0))
            .children(snapshot.rows.iter().map(|row| Self::row(row, colors)));
        let empty = snapshot.rows.is_empty() && snapshot.problem.is_none();
        let memory = self.memory_block(snapshot, colors, cx);
        v_flex()
            .flex_1()
            .min_h_0()
            .when_some(memory, |this, memory| this.child(memory))
            .when(empty, |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap(px(4.0))
                        .px(px(24.0))
                        .text_center()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(colors.text)
                                .child(match snapshot.phase {
                                    VoicePhase::Live => "Go ahead, I'm listening.",
                                    _ => "Starting the voice agent…",
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .line_height(px(16.0))
                                .text_color(colors.muted)
                                .child("Ask what your agents are doing, send one a prompt, answer a question, or start a new agent."),
                        ),
                )
            })
            .when(!empty, |this| this.child(rows))
            .when_some(snapshot.problem.clone(), |this, problem| {
                this.child(
                    h_flex()
                        .mx(px(12.0))
                        .mb(px(8.0))
                        .p(px(10.0))
                        .gap(px(8.0))
                        .items_start()
                        .rounded(px(8.0))
                        .bg(rgba(0xfbbf2418))
                        .border_1()
                        .border_color(rgba(0xfbbf2440))
                        .child(
                            div()
                                .pt(px(1.0))
                                .child(titlebar_svg_icon(ICON_ALERT, 14.0, rgba(0xfbbf24ff).into())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.0))
                                .line_height(px(17.0))
                                .text_color(colors.text)
                                .child(problem.message),
                        ),
                )
            })
            .into_any_element()
    }

    /// The remembered last conversation, above the transcript while the call is live: the
    /// Continue / Start fresh card, the "Remembering" line (click it for the whole summary), or
    /// "Forgot the last conversation."
    fn memory_block(
        &self,
        snapshot: &PanelSnapshot,
        colors: &PanelColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if snapshot.phase != VoicePhase::Live || snapshot.voice_picker_open {
            return None;
        }
        let accent: Hsla = rgba(MEMORY_ACCENT).into();
        match (snapshot.memory_state, &snapshot.memory) {
            (VoiceMemoryState::Offered, Some(memory)) => {
                let ago = memory.ago();
                let gist = if memory.gist.is_empty() {
                    memory.summary.clone()
                } else {
                    memory.gist.clone()
                };
                Some(
                    v_flex()
                        .id("voice-panel-memory-card")
                        .role(gpui::Role::Group)
                        .aria_label(SharedString::new_static("Last conversation"))
                        .flex_shrink_0()
                        .mx(px(12.0))
                        .mt(px(10.0))
                        .p(px(10.0))
                        .gap(px(7.0))
                        .rounded(px(10.0))
                        .bg(colors.ink.opacity(0.05))
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            h_flex()
                                .gap(px(6.0))
                                .items_center()
                                .text_size(px(11.5))
                                .text_color(colors.muted)
                                .child(titlebar_svg_icon(ICON_HISTORY, 12.0, accent))
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(colors.text)
                                        .child("Last conversation"),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .when(!ago.is_empty(), |this| {
                                            this.child(format!("· {ago}"))
                                        }),
                                )
                                .child(self.button(
                                    "voice-panel-memory-forget",
                                    ICON_TRASH,
                                    None,
                                    PanelAction::ForgetMemory,
                                    colors,
                                    false,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .line_height(px(16.5))
                                .text_color(colors.text.opacity(0.85))
                                .child(gist),
                        )
                        .child(
                            h_flex()
                                .gap(px(6.0))
                                .child(self.button(
                                    "voice-panel-memory-continue",
                                    ICON_HISTORY,
                                    Some("Continue"),
                                    PanelAction::ContinueMemory,
                                    colors,
                                    false,
                                    cx,
                                ))
                                .child(self.button(
                                    "voice-panel-memory-fresh",
                                    ICON_FRESH,
                                    Some("Start fresh"),
                                    PanelAction::StartFresh,
                                    colors,
                                    false,
                                    cx,
                                )),
                        )
                        .into_any_element(),
                )
            }
            (VoiceMemoryState::Continued, Some(memory)) => {
                let ago = memory.ago();
                let open = snapshot.memory_details_open;
                let hover = colors.ink.opacity(0.06);
                Some(
                    v_flex()
                        .flex_shrink_0()
                        .mx(px(12.0))
                        .mt(px(10.0))
                        .p(px(6.0))
                        .gap(px(6.0))
                        .rounded(px(8.0))
                        .bg(accent.opacity(0.08))
                        .border_1()
                        .border_color(accent.opacity(0.22))
                        .child(
                            h_flex()
                                .gap(px(6.0))
                                .items_center()
                                .child(
                                    h_flex()
                                        .id("voice-panel-memory-remembering")
                                        .role(gpui::Role::Button)
                                        .aria_label(SharedString::new_static(
                                            "Show what the agent remembers",
                                        ))
                                        .flex_1()
                                        .min_w_0()
                                        .h(px(24.0))
                                        .px(px(4.0))
                                        .gap(px(6.0))
                                        .items_center()
                                        .rounded(px(6.0))
                                        .text_size(px(11.5))
                                        .text_color(colors.muted)
                                        .cursor_default()
                                        .hover(move |this| this.bg(hover))
                                        .child(titlebar_svg_icon(ICON_HISTORY, 12.0, accent))
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(colors.text)
                                                .child("Remembering"),
                                        )
                                        .child(div().flex_1().min_w_0().truncate().child(
                                            if ago.is_empty() {
                                                "last conversation".to_string()
                                            } else {
                                                format!("last conversation ({ago})")
                                            },
                                        ))
                                        .child(titlebar_svg_icon(ICON_CHEVRON, 11.0, colors.muted))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|_, _: &MouseDownEvent, window, cx| {
                                                window.prevent_default();
                                                cx.stop_propagation();
                                            }),
                                        )
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseUpEvent, window, cx| {
                                                window.prevent_default();
                                                cx.stop_propagation();
                                                this.run(PanelAction::ToggleMemoryDetails, cx);
                                            }),
                                        )
                                        .on_a11y_action(gpui::AccessibleAction::Click, {
                                            let panel = cx.entity().downgrade();
                                            move |_, _, cx| {
                                                let _ = panel.update(cx, |this, cx| {
                                                    this.run(PanelAction::ToggleMemoryDetails, cx)
                                                });
                                            }
                                        }),
                                )
                                .child(self.button(
                                    "voice-panel-memory-forget",
                                    ICON_TRASH,
                                    Some("Forget"),
                                    PanelAction::ForgetMemory,
                                    colors,
                                    false,
                                    cx,
                                )),
                        )
                        .when(open, |this| {
                            this.child(
                                div()
                                    .px(px(4.0))
                                    .pb(px(2.0))
                                    .text_size(px(12.0))
                                    .line_height(px(16.5))
                                    .text_color(colors.text.opacity(0.85))
                                    .child(memory.summary.clone()),
                            )
                        })
                        .into_any_element(),
                )
            }
            (VoiceMemoryState::Forgotten, _) => Some(
                h_flex()
                    .flex_shrink_0()
                    .mx(px(12.0))
                    .mt(px(10.0))
                    .gap(px(6.0))
                    .items_center()
                    .text_size(px(11.5))
                    .text_color(colors.muted)
                    .child(titlebar_svg_icon(ICON_TRASH, 12.0, colors.muted))
                    .child("Forgot the last conversation.")
                    .into_any_element(),
            ),
            _ => None,
        }
    }

    fn footer(
        &self,
        snapshot: &PanelSnapshot,
        colors: &PanelColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut left: Vec<AnyElement> = Vec::new();
        let mut right: Vec<AnyElement> = Vec::new();
        match snapshot.phase {
            VoicePhase::Starting | VoicePhase::Live => {
                left.push(self.button(
                    "voice-panel-mute",
                    if snapshot.muted {
                        ICON_MIC_OFF
                    } else {
                        ICON_MIC
                    },
                    Some(if snapshot.muted { "Unmute" } else { "Mute" }),
                    PanelAction::Mute,
                    colors,
                    false,
                    cx,
                ));
                if snapshot.state == "speaking" {
                    left.push(self.button(
                        "voice-panel-stop-talking",
                        ICON_STOP,
                        Some("Stop talking"),
                        PanelAction::StopTalking,
                        colors,
                        false,
                        cx,
                    ));
                }
                right.push(self.button(
                    "voice-panel-hang-up",
                    ICON_HANG_UP,
                    Some("Hang up"),
                    PanelAction::HangUp,
                    colors,
                    true,
                    cx,
                ));
            }
            VoicePhase::Ending => {}
            VoicePhase::Ended | VoicePhase::Idle => {
                if snapshot
                    .problem
                    .as_ref()
                    .is_some_and(VoiceProblem::wants_settings)
                {
                    left.push(self.button(
                        "voice-panel-open-settings",
                        ICON_SETTINGS,
                        Some("Open Settings"),
                        PanelAction::OpenSettings,
                        colors,
                        false,
                        cx,
                    ));
                } else {
                    left.push(self.button(
                        "voice-panel-start-again",
                        ICON_MIC,
                        Some("Start again"),
                        PanelAction::StartAgain,
                        colors,
                        false,
                        cx,
                    ));
                }
                right.push(self.button(
                    "voice-panel-close",
                    ICON_CLOSE,
                    Some("Close"),
                    PanelAction::Close,
                    colors,
                    false,
                    cx,
                ));
            }
        }
        h_flex()
            .h(px(44.0))
            .flex_shrink_0()
            .px(px(10.0))
            .gap(px(6.0))
            .items_center()
            .border_t_1()
            .border_color(colors.border)
            .children(left)
            .child(div().flex_1())
            .children(right)
            .into_any_element()
    }
}

impl Render for VoicePanelWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = &self.snapshot;
        let colors = PanelColors::resolve();
        v_flex()
            .id("voice-panel")
            .role(gpui::Role::Group)
            .aria_label(SharedString::new_static("Voice agent"))
            .size_full()
            .overflow_hidden()
            .rounded(px(PANEL_RADIUS))
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .font_family(crate::ui_fonts::UI_FONT)
            .child(self.header(snapshot, &colors, cx))
            .child(self.body(snapshot, &colors, cx))
            .when(
                matches!(snapshot.phase, VoicePhase::Starting | VoicePhase::Live),
                |this| this.child(self.speed_row(snapshot, cx)),
            )
            .child(self.footer(snapshot, &colors, cx))
            .when(self.drag.is_some(), |this| {
                this.child(self.drag_listener(cx))
            })
            .into_any_element()
    }
}
