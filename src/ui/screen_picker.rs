#[cfg(windows)]
use gpui_kit::component::Disableable;
use gpui_kit::component::{
    dialog::Dialog,
    tab::{Tab, TabBar},
};

use super::*;
use crate::voice::screen::{Choice, Report, SourceKind, State};

#[cfg(windows)]
pub(super) struct ScreenPreviews {
    attempt: u64,
    images: std::collections::HashMap<String, Arc<RenderImage>>,
    _receive: Task<()>,
}

impl Spk {
    fn screen_picker_report(&self) -> Option<&Report> {
        let View::SignedIn(sidebar) = &self.view else {
            return None;
        };
        sidebar
            .voice
            .as_ref()
            .filter(|call| call.is_participating())?
            .screen
            .as_ref()
    }

    pub(super) fn update_screen_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(windows)]
        self.update_screen_previews(window, cx);
        let state = self.screen_picker_report().map(|report| &report.state);
        if self.screen_picker_open {
            if !matches!(
                state,
                Some(
                    State::LoadingSources
                        | State::Picking
                        | State::Choices(_)
                        | State::Confirm { .. }
                        | State::Ended(_)
                )
            ) {
                self.screen_picker_open = false;
                #[cfg(windows)]
                {
                    self.windows_screen_picker = None;
                }
                if window.has_active_dialog(cx) {
                    window.close_dialog(cx);
                }
            }
        } else if matches!(
            state,
            Some(State::LoadingSources | State::Choices(_) | State::Confirm { .. })
        ) {
            self.screen_picker_open = true;
            let app = cx.weak_entity();
            let cancel_focus = cx.focus_handle();
            let primary_focus = cx.focus_handle();
            let focused_state = Cell::new(None);
            window.open_dialog(cx, move |dialog, window, cx| {
                let Some(app) = app.upgrade() else {
                    return dialog;
                };
                let app_state = app.read(cx);
                let Some(report) = app_state
                    .screen_picker_report()
                    .filter(|_| app_state.screen_picker_open)
                    .cloned()
                else {
                    return dialog;
                };
                let focus_state = (report.attempt, std::mem::discriminant(&report.state));
                if focused_state.replace(Some(focus_state)) != Some(focus_state) {
                    match report.state {
                        State::Confirm { .. } | State::Ended(_) => primary_focus.focus(window, cx),
                        _ => cancel_focus.focus(window, cx),
                    }
                }
                Self::screen_picker_dialog(
                    dialog,
                    &report,
                    &app,
                    &cancel_focus,
                    &primary_focus,
                    window,
                    cx,
                )
            });
        }
    }

    #[cfg(windows)]
    fn update_screen_previews(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = self
            .screen_picker_report()
            .and_then(|report| match &report.state {
                State::Choices(choices)
                    if self.windows_screen_picker != Some(report.attempt)
                        && !choices.is_empty() =>
                {
                    Some((report.attempt, choices))
                }
                _ => None,
            });
        if request.map(|(attempt, _)| attempt)
            == self
                .screen_previews
                .as_ref()
                .map(|previews| previews.attempt)
        {
            return;
        }
        let request = request.map(|(attempt, choices)| (attempt, choices.clone()));
        if let Some(previews) = self.screen_previews.take() {
            for image in previews.images.into_values() {
                let _ = window.drop_image(image);
            }
        }
        let Some((attempt, choices)) = request else {
            return;
        };
        let (send, receive) = async_channel::bounded(1);
        cx.background_spawn(crate::voice::screen::preview_sources(choices, send))
            .detach();
        let task = cx.spawn(async move |app, cx| {
            while let Ok((id, image)) = receive.recv().await {
                let updated = app.update(cx, |app, cx| {
                    let Some(previews) = app
                        .screen_previews
                        .as_mut()
                        .filter(|previews| previews.attempt == attempt)
                    else {
                        return false;
                    };
                    previews.images.insert(id, image);
                    cx.notify();
                    true
                });
                if !matches!(updated, Ok(true)) {
                    break;
                }
            }
        });
        self.screen_previews = Some(ScreenPreviews {
            attempt,
            images: Default::default(),
            _receive: task,
        });
    }

    fn screen_picker_dialog(
        dialog: Dialog,
        report: &Report,
        app: &Entity<Spk>,
        cancel_focus: &FocusHandle,
        primary_focus: &FocusHandle,
        window: &mut Window,
        cx: &mut App,
    ) -> Dialog {
        let attempt = report.attempt;
        let cancel = app.downgrade();
        let dialog = dialog
            .centered()
            .title("Share Screen")
            .width(px(520.).min(window.viewport_size().width - px(32.)))
            .overlay_closable(false)
            .on_ok(|_, _, cx| {
                // Let the focused tab or button handle Enter.
                cx.propagate();
                false
            })
            .on_cancel(move |_, _, cx| {
                let _ = cancel.update(cx, |app, cx| {
                    app.send(Command::StopScreenShare(attempt), None, cx);
                });
                false
            });
        let actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .justify_end();
        #[cfg(windows)]
        let native_pending = app.read(cx).windows_screen_picker == Some(attempt);
        #[cfg(windows)]
        let actions = if matches!(report.state, State::Choices(_)) {
            let app = app.downgrade();
            actions
                .child(
                    control_button("windows-screen-picker")
                        .label("Windows picker · Video only")
                        .tab_stop(true)
                        .disabled(native_pending)
                        .on_click(move |_, window, cx| {
                            let _ = app.update(cx, |app, cx| {
                                app.pick_windows_screen(attempt, window, cx);
                            });
                        }),
                )
                .child(div().flex_1())
        } else {
            actions
        };
        let mut actions = actions.child(
            picker_button("cancel-screen-picker", "Cancel", app, move || {
                Command::StopScreenShare(attempt)
            })
            .track_focus(cancel_focus),
        );
        let content = match &report.state {
            State::LoadingSources => Label::new("Loading Screen Share sources…").into_any_element(),
            State::Picking => Label::new("Choosing a Screen Share source…").into_any_element(),
            State::Choices(choices) => {
                #[cfg(windows)]
                if native_pending {
                    return dialog
                        .child(Label::new("Choose a source in the Windows picker…"))
                        .footer(actions);
                }
                screen_sources(choices, attempt, app, window, cx).into_any_element()
            }
            State::Confirm { label, audio } => {
                let View::SignedIn(sidebar) = &app.read(cx).view else {
                    return dialog;
                };
                let audio_enabled = sidebar.voice_intent.preferred.screen_audio;
                let audio_label = match (audio, audio_enabled) {
                    (Some(scope), true) => scope.label(),
                    (None, true) => "Video only. Audio is unavailable for this source.",
                    _ => "Video only",
                };
                let audio = audio_enabled && audio.is_some();
                actions = actions.child(
                    picker_button("start-screen-share", "Start Screen Share", app, move || {
                        Command::StartScreenShare(attempt, audio)
                    })
                    .primary()
                    .track_focus(primary_focus),
                );
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(label.clone()).font_semibold())
                    .child(Label::new(audio_label).text_color(cx.theme().muted_foreground))
                    .into_any_element()
            }
            State::Ended(message) => {
                actions = actions.child(
                    picker_button("retry-screen-picker", "Try again", app, || {
                        Command::OpenScreenShare
                    })
                    .primary()
                    .track_focus(primary_focus),
                );
                Label::new(*message).into_any_element()
            }
            _ => return dialog,
        };
        dialog.child(content).footer(actions)
    }

    #[cfg(windows)]
    fn pick_windows_screen(&mut self, attempt: u64, window: &mut Window, cx: &mut Context<Self>) {
        use crate::voice::screen::{CaptureError, pick_system_source};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        if self.windows_screen_picker == Some(attempt) {
            return;
        }
        self.windows_screen_picker = Some(attempt);
        self.update_screen_previews(window, cx);
        cx.notify();
        let hwnd = HasWindowHandle::window_handle(window)
            .ok()
            .and_then(|window| match window.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                _ => None,
            });
        cx.spawn(async move |app, cx| {
            let result = match hwnd {
                Some(hwnd) => pick_system_source(hwnd).await,
                None => Err(CaptureError::Unavailable),
            };
            let _ = app.update(cx, |app, cx| {
                if app.windows_screen_picker == Some(attempt) {
                    app.windows_screen_picker = None;
                    app.send(
                        Command::ChooseWindowsScreenSource(attempt, result),
                        None,
                        cx,
                    );
                }
            });
        })
        .detach();
    }
}

fn picker_button(
    id: &'static str,
    label: &'static str,
    app: &Entity<Spk>,
    command: impl Fn() -> Command + 'static,
) -> Button {
    let app = app.downgrade();
    control_button(id)
        .label(label)
        .tab_stop(true)
        .on_click(move |_, _, cx| {
            let _ = app.update(cx, |app, cx| app.send(command(), None, cx));
        })
}

fn screen_sources(
    choices: &[Choice],
    attempt: u64,
    app: &Entity<Spk>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let tab = window.use_keyed_state(("screen-source-tab", attempt as usize), cx, |_, _| 0);
    let selected = *tab.read(cx);
    let (kind, icon, empty) = if selected == 0 {
        (
            SourceKind::Display,
            IconName::Monitor,
            "No screens available.",
        )
    } else {
        (
            SourceKind::Window,
            IconName::AppWindow,
            "No windows available.",
        )
    };
    let sources: Vec<_> = choices
        .iter()
        .filter(|choice| choice.kind == kind)
        .collect();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            TabBar::new("screen-source-tabs")
                .underline()
                .selected_index(selected)
                .child(Tab::new().label("Screens"))
                .child(Tab::new().label("Windows"))
                .on_click(move |index, _, cx| {
                    tab.update(cx, |tab, cx| {
                        *tab = *index;
                        cx.notify();
                    });
                }),
        )
        .when(sources.is_empty(), |panel| {
            panel.child(Label::new(empty).text_color(cx.theme().muted_foreground))
        })
        .child(
            div()
                .id(("screen-sources", selected))
                .max_h(window.viewport_size().height * 0.5)
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .children(sources.into_iter().map(|choice| {
                    let weak_app = app.downgrade();
                    let id = choice.id.clone();
                    let button = Button::new(SharedString::from(format!("screen-source-{id}")))
                        .secondary()
                        .w_full()
                        .justify_start()
                        .tab_stop(true)
                        .tooltip(choice.label.clone())
                        .on_click(move |_, _, cx| {
                            let _ = weak_app.update(cx, |app, cx| {
                                app.send(
                                    Command::ChooseScreenSource(attempt, id.clone()),
                                    None,
                                    cx,
                                );
                            });
                        });
                    #[cfg(windows)]
                    {
                        let image = app
                            .read(cx)
                            .screen_previews
                            .as_ref()
                            .filter(|previews| previews.attempt == attempt)
                            .and_then(|previews| previews.images.get(&choice.id))
                            .cloned();
                        let preview = div()
                            .w(px(112.))
                            .h(px(63.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .overflow_hidden()
                            .bg(cx.theme().background);
                        let preview = match image {
                            Some(image) => {
                                preview.child(img(image).size_full().object_fit(ObjectFit::Contain))
                            }
                            None => preview
                                .child(Icon::new(icon).text_color(cx.theme().muted_foreground)),
                        };
                        button
                            .h(px(76.))
                            .accessibility_label(choice.label.clone())
                            .child(
                                div()
                                    .flex()
                                    .w_full()
                                    .items_center()
                                    .gap_3()
                                    .min_w_0()
                                    .child(preview)
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .overflow_hidden()
                                            .child(choice.label.clone()),
                                    ),
                            )
                    }
                    #[cfg(not(windows))]
                    button.icon(icon).label(choice.label.clone())
                })),
        )
}
