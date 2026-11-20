use gpui_kit::{
    base::{Root, RootPlugin},
    component::{
        Disableable,
        slider::{Slider, SliderValue},
        switch::Switch,
    },
};

use super::*;
use crate::shortcuts::{Action, Binding, Chord, Scope, Shortcuts};

pub struct ShortcutKeys;

impl Render for ShortcutKeys {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl RootPlugin for ShortcutKeys {
    fn decorate(
        &self,
        surface: AnyElement,
        root: &Root,
        _: &mut Window,
        _: &mut App,
    ) -> impl IntoElement {
        let app = root
            .view()
            .clone()
            .downcast::<Spk>()
            .ok()
            .map(|app| app.downgrade());
        let release = app.clone();
        div()
            .size_full()
            .on_key_down(move |event, window, cx| {
                if event.prefer_character_input || !window.is_window_active() {
                    return;
                }
                let Some(app) = &app else { return };
                let Some(chord) = Chord::capture(&event.keystroke, window) else {
                    return;
                };
                let _ = app.update(cx, |app, cx| {
                    if let Some(action) = app.shortcuts.focused_action(&chord) {
                        if app.shortcuts.focused_press(action, event.is_held) {
                            app.voice_action(action, cx);
                        }
                        cx.stop_propagation();
                        window.prevent_default();
                    }
                });
            })
            .capture_key_up(move |event, window, cx| {
                if let Some(app) = &release {
                    let _ = app.update(cx, |app, _| {
                        app.shortcuts.focused_release(&event.keystroke, window)
                    });
                }
            })
            .child(surface)
    }
}

impl Settings {
    pub(super) fn shortcuts(&self, cx: &Context<Self>) -> impl IntoElement {
        let shortcuts = &self.spk.read(cx).shortcuts;
        card(
            "Voice shortcuts",
            div()
                .id("shortcut-recorder")
                .track_focus(&self.recorder_focus)
                .flex()
                .flex_col()
                .gap_5()
                .children(
                    shortcuts
                        .platform_error()
                        .map(|message| Label::new(message).text_color(cx.theme().muted_foreground)),
                )
                .children(Action::ALL.map(|action| self.shortcut_row(action, shortcuts, cx)))
                .children(
                    shortcuts
                        .settings_error
                        .map(|message| Label::new(message).text_color(cx.theme().danger)),
                ),
            cx,
        )
    }

    fn shortcut_row(
        &self,
        action: Action,
        shortcuts: &Shortcuts,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let binding = shortcuts.binding(action);
        let description = binding.map_or_else(
            || "Unassigned".to_owned(),
            |binding| format!("{} · {}", binding.chord, binding.scope.label()),
        );
        let editor = shortcuts
            .editor
            .as_ref()
            .filter(|editor| editor.action == action);
        let error = shortcuts.error(action);
        div()
            .id(action.label())
            .flex()
            .flex_col()
            .gap_2()
            .child(Label::new(action.label()).font_semibold())
            .child(Label::new(description).text_color(cx.theme().muted_foreground))
            .when(editor.is_none(), |row| {
                row.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            control(
                                "record",
                                if binding.is_some() {
                                    "Change"
                                } else {
                                    "Record shortcut"
                                },
                            )
                            .accessibility_label(format!(
                                "Record shortcut for {}",
                                action.label().to_lowercase()
                            ))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.spk.update(cx, |app, cx| {
                                        let scope = app
                                            .shortcuts
                                            .binding(action)
                                            .map_or(Scope::InSpk, |binding| binding.scope);
                                        app.shortcuts.editor = Some(crate::shortcuts::Editor {
                                            action,
                                            scope,
                                            chord: None,
                                            recording: true,
                                            error: None,
                                            allow_focused: false,
                                        });
                                        cx.notify();
                                    });
                                    this.recorder_focus.focus(window, cx);
                                },
                            )),
                        )
                        .when(binding.is_some(), |controls| {
                            controls.child(
                                control("clear", "Clear")
                                    .accessibility_label(format!(
                                        "Clear shortcut for {}",
                                        action.label().to_lowercase()
                                    ))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.spk.update(cx, |app, cx| {
                                            if let Err(error) =
                                                app.shortcuts.set_binding(action, None)
                                            {
                                                app.shortcuts.settings_error =
                                                    Some(error.message());
                                            }
                                            cx.notify();
                                        });
                                    })),
                            )
                        })
                        .when(error.is_some(), |controls| {
                            controls
                                .child(control("retry", "Retry").on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.spk.update(cx, |app, cx| {
                                            app.shortcuts.retry(action);
                                            cx.notify();
                                        });
                                    },
                                )))
                                .child(control("fallback", "Use in SPK").on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.spk.update(cx, |app, cx| {
                                            if let Some(mut binding) =
                                                app.shortcuts.binding(action).cloned()
                                            {
                                                binding.scope = Scope::InSpk;
                                                if let Err(error) =
                                                    app.shortcuts.set_binding(action, Some(binding))
                                                {
                                                    app.shortcuts.settings_error =
                                                        Some(error.message());
                                                }
                                                cx.notify();
                                            }
                                        });
                                    },
                                )))
                        }),
                )
            })
            .children(
                error
                    .filter(|_| editor.is_none())
                    .map(|error| Label::new(error.message()).text_color(cx.theme().danger)),
            )
            .children(editor.map(|editor| {
                let text = editor.chord.as_ref().map_or_else(
                    || "Press a key combination. Esc cancels.".to_owned(),
                    |chord| chord.to_string(),
                );
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .child(Label::new(text))
                    .when(!editor.recording, |draft| {
                        draft.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .children([Scope::InSpk, Scope::Global].map(|scope| {
                                    control(scope.label(), scope.label())
                                        .selected(editor.scope == scope)
                                        .disabled(
                                            scope == Scope::Global
                                                && shortcuts.platform_error().is_some(),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.spk.update(cx, |app, cx| {
                                                if let Some(editor) = &mut app.shortcuts.editor {
                                                    editor.scope = scope;
                                                    editor.error = None;
                                                    editor.allow_focused = false;
                                                    cx.notify();
                                                }
                                            });
                                        }))
                                }))
                                .child(control("save", "Save shortcut").primary().on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.save_shortcut(None, cx);
                                    }),
                                ))
                                .when(editor.allow_focused, |controls| {
                                    controls.child(control("save-focused", "Use in SPK").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.save_shortcut(Some(Scope::InSpk), cx);
                                        }),
                                    ))
                                }),
                        )
                    })
                    .child(
                        control("cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.spk
                                .update(cx, |app, cx| app.cancel_shortcut_editor(cx));
                        })),
                    )
                    .children(
                        editor
                            .error
                            .map(|error| Label::new(error).text_color(cx.theme().danger)),
                    )
            }))
    }

    fn save_shortcut(&self, scope: Option<Scope>, cx: &mut Context<Self>) {
        self.spk.update(cx, |app, cx| {
            let Some(editor) = &app.shortcuts.editor else {
                return;
            };
            let Some(chord) = editor.chord.clone() else {
                return;
            };
            let binding = Binding {
                chord,
                scope: scope.unwrap_or(editor.scope),
            };
            match app.shortcuts.set_binding(editor.action, Some(binding)) {
                Ok(()) => {
                    app.shortcuts.editor = None;
                }
                Err(error) => {
                    let editor = app.shortcuts.editor.as_mut().expect("editing a shortcut");
                    editor.error = Some(error.message());
                    editor.allow_focused = error.can_use_focused();
                }
            }
            cx.notify();
        });
    }

    pub(super) fn voice_sounds(&self, cx: &App) -> impl IntoElement {
        let shortcuts = &self.spk.read(cx).shortcuts;
        let sounds = shortcuts.sound_settings();
        card(
            "Voice sounds",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .children(
                    Sound::ALL
                        .into_iter()
                        .zip(&self.volumes)
                        .map(|(sound, slider)| {
                            let spk = self.spk.downgrade();
                            let setting = sounds.get(sound);
                            let SliderValue::Single(volume) = slider.read(cx).value() else {
                                unreachable!("voice sound uses a single volume");
                            };
                            div()
                                .id(sound.label())
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    Switch::new("enabled")
                                        .label(sound.label())
                                        .accessibility_label(sound.label())
                                        .checked(setting.enabled)
                                        .tab_stop(true)
                                        .on_change(move |enabled, _, cx| {
                                            let _ = spk.update(cx, |app, cx| {
                                                app.change_sound(
                                                    sound,
                                                    |setting| setting.enabled = *enabled,
                                                    cx,
                                                );
                                            });
                                        }),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            Slider::new(slider)
                                                .w(px(128.0))
                                                .accessibility_label(format!(
                                                    "{} volume",
                                                    sound.label()
                                                ))
                                                .tab_stop(true),
                                        )
                                        .child(Label::new(format!("{volume:.0}%")).w(px(36.0))),
                                )
                        }),
                )
                .children(
                    shortcuts
                        .settings_error
                        .map(|message| Label::new(message).text_color(cx.theme().danger)),
                ),
            cx,
        )
    }
}

fn control(id: &'static str, label: &'static str) -> Button {
    Button::new(id).small().tab_stop(true).label(label)
}
