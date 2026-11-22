use super::chat::QUOTE_PREFIX;
use super::*;
use crate::{
    session::VoiceProcessingSetting,
    shortcuts::Action,
    voice::{NoiseSuppression, screen::State as ScreenState},
};

impl Spk {
    pub(super) fn workspace_key(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let View::SignedIn(sidebar) = &self.view else {
            return false;
        };
        if !window.is_window_active() {
            return false;
        }
        let key = &event.keystroke;
        if Chord::capture(key, window).is_some_and(|chord| self.shortcuts.owns_chord(&chord)) {
            return false;
        }
        let mut modifiers = key.modifiers;
        modifiers.shift |= window.modifiers().shift;
        let primary = modifiers.secondary(cx);
        let count = modifiers.number_of_modifiers();
        let key_name = key.key.as_str();
        if event.input_preference == InputPreference::CharacterInput {
            return false;
        }
        if primary && count == 1 && key_name == "k" {
            if window.has_active_dialog(cx) {
                if self
                    .quick_switcher
                    .as_ref()
                    .and_then(WeakEntity::upgrade)
                    .is_none()
                {
                    return false;
                }
                self.quick_switcher = None;
                window.close_dialog(cx);
            } else {
                self.open_quick_switcher(window, cx);
            }
            return true;
        }
        if window.has_active_dialog(cx) {
            return false;
        }
        let typing = window.has_focused_input(cx);
        if primary && modifiers.alt && modifiers.shift && count == 3 {
            let processing = sidebar.voice_intent.preferred.processing;
            let command = match key_name {
                "e" if sidebar
                    .voice
                    .as_ref()
                    .is_some_and(session::VoiceView::is_participating) =>
                {
                    Command::SetCameraEnabled(!sidebar.camera_publishing())
                }
                "s" if sidebar
                    .voice
                    .as_ref()
                    .is_some_and(session::VoiceView::is_participating) =>
                {
                    sidebar
                        .voice
                        .as_ref()
                        .and_then(|call| call.screen.as_ref())
                        .filter(|report| !matches!(report.state, ScreenState::Ended(_)))
                        .map_or(Command::OpenScreenShare, |report| {
                            Command::StopScreenShare(report.attempt)
                        })
                }
                "n" => Command::SetVoiceProcessing(VoiceProcessingSetting::NoiseSuppression(
                    match processing.noise_suppression {
                        NoiseSuppression::Off => NoiseSuppression::Standard,
                        NoiseSuppression::Standard => NoiseSuppression::Enhanced,
                        NoiseSuppression::Enhanced => NoiseSuppression::Off,
                    },
                )),
                "o" => Command::SetVoiceProcessing(VoiceProcessingSetting::EchoCancellation(
                    !processing.echo_cancellation,
                )),
                "g" => Command::SetVoiceProcessing(VoiceProcessingSetting::AutoGain(
                    !processing.auto_gain,
                )),
                _ => return false,
            };
            self.send(command, None, cx);
            return true;
        }
        let visible_chat = sidebar.chat.is_some() && !self.video_stage.fullscreen;
        let composer = visible_chat && self.chat_input.focus_handle(cx).is_focused(window);
        let editor = visible_chat
            && self
                .message_action_input
                .focus_handle(cx)
                .is_focused(window);
        if composer || editor {
            let formatting = match (primary, modifiers.alt, modifiers.shift, count, key_name) {
                (true, false, false, 1, "b") => Some(("**", "**")),
                (true, false, false, 1, "i") => Some(("_", "_")),
                (true, false, false, 1, "e") => Some(("`", "`")),
                (true, false, true, 2, "x") => Some(("~~", "~~")),
                (true, true, false, 2, "c") | (true, false, true, 2, "c") => {
                    Some(("\n```\n", "\n```\n"))
                }
                (true, false, true, 2, "." | ">") => Some((QUOTE_PREFIX, "")),
                _ => None,
            };
            if let Some((before, after)) = formatting {
                self.format_chat_input(before, after, editor, window, cx);
                return true;
            }
        }
        match (primary, modifiers.alt, modifiers.shift, count, key_name) {
            (false, true, shift, 1 | 2, "up" | "down") => {
                self.navigate_channel(key_name == "up", shift, window, cx);
            }
            (true, false, false, 1, "b") if !typing => {
                self.toggle_sidebar(cx);
            }
            (true, false, true, 2, "f") => {
                self.open_search(true, window, cx);
            }
            (false, false, false, 0, "/") if !typing => {
                self.open_message_search(window, cx);
            }
            (false, false, _, 0 | 1, "?") | (false, false, true, 1, "/") if !typing => {
                self.open_shortcut_reference(window, cx)
            }
            (false, false, false, 0, "up") if composer && self.edit_last_message(window, cx) => {}
            (false, false, false, 0, "escape") if !self.video_stage.fullscreen => {
                return self.escape_chat(window, cx);
            }
            _ => return false,
        }
        true
    }

    pub(super) fn open_shortcut_reference(&self, window: &mut Window, cx: &mut Context<Self>) {
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(app) = app.upgrade() else {
                return dialog;
            };
            let app = app.read(cx);
            dialog
                .centered()
                .width(px(520.0))
                .title("Keyboard shortcuts")
                .child(
                    div()
                        .id("shortcut-reference")
                        .max_h(window.viewport_size().height * 0.65)
                        .overflow_y_scroll()
                        .child(shortcut_reference(&app.shortcuts, true, cx)),
                )
        });
    }
}

pub(super) fn shortcut_reference(
    shortcuts: &Shortcuts,
    include_voice_bindings: bool,
    cx: &App,
) -> impl IntoElement {
    let primary = if cx.platform_info().is_mac() {
        "⌘"
    } else {
        "Ctrl"
    };
    let alt = if cx.platform_info().is_mac() {
        "Option"
    } else {
        "Alt"
    };
    let media = |key| format!("{primary}+{alt}+Shift+{key}");
    let groups = [
        (
            "Navigation",
            vec![
                ("Quick Switcher", format!("{primary}+K")),
                ("Previous or next channel", format!("{alt}+↑ / ↓")),
                (
                    "Previous or next unread channel",
                    format!("{alt}+Shift+↑ / ↓"),
                ),
                ("Show or hide sidebar", format!("{primary}+B")),
                ("Show keyboard shortcuts", "?".into()),
            ],
        ),
        (
            "Messages",
            vec![
                ("Edit your last message (empty composer)", "↑".into()),
                ("Reply to focused message", "R".into()),
                ("Cancel edit or reply, or mark as read", "Esc".into()),
                ("Send message", "Enter".into()),
                ("New line", "Shift+Enter".into()),
            ],
        ),
        (
            "Search",
            vec![
                ("Open message search", "/".into()),
                ("Search everything", format!("{primary}+Shift+F")),
                ("Select a search result", "↑ / ↓".into()),
                ("Search or open selected result", "Enter".into()),
                ("Close search", "Esc".into()),
            ],
        ),
        (
            "Formatting",
            vec![
                ("Bold", format!("{primary}+B")),
                ("Italic", format!("{primary}+I")),
                ("Strikethrough", format!("{primary}+Shift+X")),
                ("Inline code", format!("{primary}+E")),
                ("Code block", format!("{primary}+{alt}+C")),
                ("Quote", format!("{primary}+Shift+.")),
            ],
        ),
        (
            "Voice and video",
            Action::ALL
                .into_iter()
                .filter(|_| include_voice_bindings)
                .map(|action| {
                    let label = shortcuts.binding(action).map_or_else(
                        || "Unassigned".to_owned(),
                        |binding| format!("{} · {}", binding.chord, binding.scope.label()),
                    );
                    (action.label(), label)
                })
                .chain([
                    ("Camera on or off", media("E")),
                    ("Start or stop Screen Share", media("S")),
                    ("Cycle noise suppression", media("N")),
                    ("Echo cancellation on or off", media("O")),
                    ("Auto gain on or off", media("G")),
                    ("Theater mode (focused or hovered tile)", "T".into()),
                    ("Fullscreen (focused or hovered tile)", "F".into()),
                    (
                        "Watch or stop watching (focused or hovered tile)",
                        "W".into(),
                    ),
                ])
                .collect(),
        ),
    ];
    div()
        .flex()
        .flex_col()
        .gap_4()
        .children(groups.into_iter().map(|(title, rows)| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Label::new(title).font_semibold())
                .children(rows.into_iter().map(|(label, keys)| {
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap_3()
                        .child(Label::new(label).min_w_0())
                        .child(
                            Label::new(keys)
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground),
                        )
                }))
        }))
}
