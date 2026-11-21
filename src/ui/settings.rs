use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

mod account_admin;
mod audio;
mod camera;
mod changelog;
mod channels;
mod notifications;
mod profile;
mod shortcuts;

use gpui_kit::component::{
    Theme, ThemeMode, ThemeRegistry, WindowExt,
    slider::{SliderEvent, SliderState, SliderValue},
};

use super::*;
use crate::{
    session::{AdminAccountAction, View, VoiceProcessingSetting},
    sounds::Sound,
};

pub use shortcuts::ShortcutKeys;

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
    System,
}

impl Appearance {
    pub fn load() -> Self {
        load_preference(preference_path("appearance"))
            .as_deref()
            .map_or_else(Self::default, Self::from_preference)
    }

    fn from_preference(value: &str) -> Self {
        match value {
            "Light" => Self::Light,
            "System" => Self::System,
            _ => Self::Dark,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::System => "System",
        }
    }

    pub(super) fn save(self) -> io::Result<()> {
        save_preference(preference_path("appearance"), self.label())
    }

    pub fn apply(self, window: &Window, cx: &mut App) {
        let mode = match self {
            Self::Dark => ThemeMode::Dark,
            Self::Light => ThemeMode::Light,
            Self::System => window.appearance().into(),
        };
        let name = if mode.is_dark() {
            "SPK Dark"
        } else {
            "SPK Light"
        };
        let theme = ThemeRegistry::global(cx).themes()[name].clone();
        Theme::update(cx, |current| current.apply_config(&theme));
    }
}

fn preference_path(name: &str) -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("spk-gpui").join(name))
}

fn load_preference(path: Option<PathBuf>) -> Option<String> {
    fs::read_to_string(path?).ok()
}

fn save_preference(path: Option<PathBuf>, value: &str) -> io::Result<()> {
    let path = path.ok_or_else(|| io::Error::other("Missing config directory"))?;
    let directory = path.parent().expect("preference file has a parent");
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(value.as_bytes())?;
    file.persist(path)?;
    Ok(())
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(super) enum Density {
    #[default]
    Comfortable,
    Compact,
}

impl Density {
    pub(super) fn load() -> Self {
        Self::load_from(preference_path("density"))
    }

    fn load_from(path: Option<PathBuf>) -> Self {
        match load_preference(path).as_deref() {
            Some("Compact") => Self::Compact,
            _ => Self::Comfortable,
        }
    }

    fn save_to(self, path: Option<PathBuf>) -> io::Result<()> {
        save_preference(path, self.label())
    }

    fn label(self) -> &'static str {
        match self {
            Self::Comfortable => "Comfortable",
            Self::Compact => "Compact",
        }
    }
}

impl Spk {
    pub(super) fn set_appearance(
        &mut self,
        appearance: Appearance,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.appearance = appearance;
        self.appearance_error = appearance.save().is_err();
        appearance.apply(window, cx);
        cx.notify();
    }

    pub(super) fn compact(&self) -> bool {
        self.density == Density::Compact
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Tab {
    Profile,
    Appearance,
    Voice,
    Notifications,
    Shortcuts,
    Changelog,
    Invites,
    Users,
    Channels,
}

impl Tab {
    fn requires_admin(self) -> bool {
        matches!(self, Self::Invites | Self::Users | Self::Channels)
    }
}

impl Spk {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.device_picker = None;
        let spk = cx.entity();
        let sounds = self.shortcuts.sound_settings();
        let preferences = match &self.view {
            View::SignedIn(sidebar) => sidebar.voice_intent.preferred.clone(),
            _ => Default::default(),
        };
        let initial_nickname = match &self.view {
            View::SignedIn(sidebar) => sidebar.profile.display_name.clone().unwrap_or_default(),
            _ => String::new(),
        };
        let content = cx.new(|cx: &mut Context<Settings>| {
            let nickname = cx.new(|cx| InputState::new(window, cx).default_value(initial_nickname));
            cx.subscribe_in(&nickname, window, |settings, _, event, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    settings.save_nickname(cx);
                }
            })
            .detach();
            let invite_note = cx.new(|cx| InputState::new(window, cx));
            let ban_reason = cx.new(|cx| InputState::new(window, cx));
            cx.subscribe_in(&invite_note, window, |settings, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    settings.create_invite(window, cx);
                }
            })
            .detach();
            let channel_name = cx.new(|cx| InputState::new(window, cx).placeholder("Channel name"));
            cx.subscribe_in(&channel_name, window, |settings, _, event, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    settings.submit_channel_form(false, cx);
                }
            })
            .detach();
            cx.observe(&spk, |settings, _, cx| {
                settings.sync_account_admin(cx);
                settings.sync_channel_form(cx);
                settings.sync_mic_test(cx);
                settings.sync_camera_preview(cx);
                cx.notify();
            })
            .detach();
            let output_volume = cx.new(|_| {
                SliderState::new()
                    .max(200.0)
                    .default_value(f32::from(preferences.output_volume.min(200)))
            });
            cx.subscribe(&output_volume, |settings, _, event, cx| {
                if let SliderEvent::Release(SliderValue::Single(value)) = event {
                    settings.spk.update(cx, |app, cx| {
                        app.send(Command::SetOutputVolume(*value as u8), None, cx)
                    });
                }
                cx.notify();
            })
            .detach();
            let sensitivity = cx.new(|_| {
                SliderState::new()
                    .default_value(f32::from(preferences.processing.sensitivity.min(100)))
            });
            cx.subscribe(&sensitivity, |settings, _, event, cx| {
                if let SliderEvent::Release(SliderValue::Single(value)) = event {
                    settings.spk.update(cx, |app, cx| {
                        app.send(
                            Command::SetVoiceProcessing(VoiceProcessingSetting::Sensitivity(
                                *value as u8,
                            )),
                            None,
                            cx,
                        );
                    });
                }
                cx.notify();
            })
            .detach();
            let volumes = Sound::ALL.map(|sound| {
                let volume = cx
                    .new(|_| SliderState::new().default_value(f32::from(sounds.get(sound).volume)));
                cx.subscribe(&volume, move |settings, _, event, cx| {
                    if let SliderEvent::Release(SliderValue::Single(value)) = event {
                        settings.spk.update(cx, |app, cx| {
                            app.change_sound(sound, |setting| setting.volume = *value as u8, cx)
                        });
                    }
                    cx.notify();
                })
                .detach();
                volume
            });
            let message_volume = cx.new(|_| {
                SliderState::new()
                    .max(200.0)
                    .default_value(f32::from(sounds.get(Sound::Message).volume.min(200)))
            });
            cx.subscribe(&message_volume, |settings, _, event, cx| {
                if let SliderEvent::Release(SliderValue::Single(value)) = event {
                    settings.spk.update(cx, |app, cx| {
                        app.change_sound(
                            Sound::Message,
                            |setting| setting.volume = *value as u8,
                            cx,
                        )
                    });
                }
                cx.notify();
            })
            .detach();
            Settings {
                spk,
                tab: Tab::Profile,
                changelog: Default::default(),
                nickname,
                invite_note,
                ban_reason,
                ban_target: None,
                channel_name,
                channel_form: None,
                channel_submitted_at: None,
                avatar_picker_open: false,
                avatar_picker_error: None,
                volumes,
                message_volume,
                recorder_focus: cx.focus_handle(),
                output_volume,
                sensitivity,
                mic_test: None,
                mic_level: 0,
                playback_pending: false,
                audio_test_error: None,
                camera: camera::Preview::default(),
            }
        });
        cx.observe_release_in(&content, window, |this, settings, window, cx| {
            this.cancel_shortcut_editor(cx);
            if settings.tab.admin_page().is_some() && matches!(this.view, View::SignedIn(_)) {
                this.send(
                    Command::AdminAccounts(AdminAccountAction::Show(None)),
                    None,
                    cx,
                );
            }
            settings.camera.release(window);
        })
        .detach();
        window.open_dialog(cx, move |dialog, window, _| {
            dialog
                .centered()
                .width(px(1152.0))
                .h(window.viewport_size().height * 0.85)
                .p_0()
                .close_button(false)
                .content({
                    let content = content.clone();
                    move |body, _, _| body.min_h_0().child(content.clone())
                })
        });
        cx.notify();
    }
}

struct Settings {
    spk: Entity<Spk>,
    tab: Tab,
    changelog: changelog::History,
    nickname: Entity<InputState>,
    invite_note: Entity<InputState>,
    ban_reason: Entity<InputState>,
    ban_target: Option<String>,
    channel_name: Entity<InputState>,
    channel_form: Option<channels::ChannelForm>,
    channel_submitted_at: Option<u64>,
    avatar_picker_open: bool,
    avatar_picker_error: Option<&'static str>,
    volumes: [Entity<SliderState>; Sound::ALL.len()],
    message_volume: Entity<SliderState>,
    recorder_focus: FocusHandle,
    output_volume: Entity<SliderState>,
    sensitivity: Entity<SliderState>,
    mic_test: Option<(Option<crate::voice::Device>, Task<()>)>,
    mic_level: u8,
    playback_pending: bool,
    audio_test_error: Option<&'static str>,
    camera: camera::Preview,
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.camera.release_unused_image(window);
        let theme = cx.theme();
        let app = self.spk.read(cx);
        let View::SignedIn(sidebar) = &app.view else {
            return div();
        };
        if self.tab.requires_admin() && !sidebar.is_admin() {
            self.tab = Tab::Profile;
        }
        let body = match self.tab {
            Tab::Profile => self.profile(sidebar, cx).into_any_element(),
            Tab::Appearance => self.appearance(app, cx).into_any_element(),
            Tab::Voice => self.audio(sidebar, cx).into_any_element(),
            Tab::Notifications => self.notifications(cx).into_any_element(),
            Tab::Shortcuts => self.shortcuts(cx).into_any_element(),
            Tab::Changelog => self.changelog(cx).into_any_element(),
            Tab::Invites | Tab::Users => self
                .manage_accounts(
                    self.tab.admin_page().expect("admin account tab"),
                    sidebar,
                    cx,
                )
                .into_any_element(),
            Tab::Channels => self.channels(sidebar, cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_sm()
            .font_family(theme.font_family.clone())
            .text_color(theme.foreground)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_start()
                    .gap_3()
                    .px_6()
                    .py_4()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(Label::new("Settings").text_base().font_semibold())
                            .child(
                                Label::new("Your profile and preferences")
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .child(
                        tool_button("close-settings", IconName::X, "Close settings").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.spk
                                    .update(cx, |app, cx| app.cancel_shortcut_editor(cx));
                                window.close_dialog(cx);
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .w(px(208.0))
                            .max_w(relative(0.36))
                            .flex_shrink_0()
                            .p_3()
                            .border_r_1()
                            .border_color(theme.border)
                            .children(
                                [
                                    (Tab::Profile, "Profile", IconName::User),
                                    (Tab::Appearance, "Appearance", IconName::Palette),
                                    (Tab::Voice, "Voice & Video", IconName::Mic),
                                    (Tab::Notifications, "Notifications", IconName::Bell),
                                    (Tab::Shortcuts, "Shortcuts", IconName::Settings2),
                                    (Tab::Changelog, "Changelog", IconName::FileText),
                                    (Tab::Invites, "Invites", IconName::Link),
                                    (Tab::Users, "Users", IconName::Users),
                                    (Tab::Channels, "Channels", IconName::Hash),
                                ]
                                .into_iter()
                                .filter(|(tab, _, _)| !tab.requires_admin() || sidebar.is_admin())
                                .map(|(tab, label, icon)| {
                                    let active = self.tab == tab;
                                    let color = if active {
                                        theme.primary
                                    } else {
                                        theme.muted_foreground
                                    };
                                    Button::new(label)
                                        .ghost()
                                        .small()
                                        .tab_stop(true)
                                        .selected(active)
                                        .accessibility_label(label)
                                        .h_8()
                                        .when(active, |button| {
                                            button.bg(theme.primary.opacity(0.10))
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .w_full()
                                                .gap_2()
                                                .child(themed_icon(icon, color, 16.0))
                                                .child(
                                                    Label::new(label).text_color(color).truncate(),
                                                ),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if this.tab == Tab::Shortcuts && tab != Tab::Shortcuts {
                                                this.spk.update(cx, |app, cx| {
                                                    app.cancel_shortcut_editor(cx)
                                                });
                                            }
                                            let old_admin_page = this.tab.admin_page();
                                            let admin_page = tab.admin_page();
                                            if old_admin_page != admin_page {
                                                this.ban_target = None;
                                                this.account_action(
                                                    AdminAccountAction::Show(admin_page),
                                                    cx,
                                                );
                                            }
                                            this.tab = tab;
                                            if tab == Tab::Changelog && this.changelog.initial() {
                                                this.load_changelog(cx);
                                            }
                                            this.sync_mic_test(cx);
                                            this.sync_camera_preview(cx);
                                            if tab == Tab::Voice {
                                                this.refresh_cameras(cx);
                                                this.spk.update(cx, |app, cx| {
                                                    app.send(Command::RefreshDevices, None, cx)
                                                });
                                            }
                                            cx.notify();
                                        }))
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .children(
                                self.channel_form
                                    .as_ref()
                                    .filter(|_| self.tab == Tab::Channels)
                                    .map(|form| {
                                        div()
                                            .px_6()
                                            .pt_6()
                                            .gap_2()
                                            .flex()
                                            .flex_col()
                                            .children(self.channel_status(sidebar, cx))
                                            .child(self.channel_editor(
                                                form,
                                                sidebar.channel_admin.pending,
                                                cx,
                                            ))
                                    }),
                            )
                            .child(
                                div()
                                    .id("settings-body")
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .p_6()
                                    .child(body),
                            ),
                    ),
            )
    }
}

fn card(title: &'static str, body: impl IntoElement, cx: &App) -> impl IntoElement + Styled {
    let border = cx.theme().border;
    div()
        .id(title)
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .child(
            div()
                .px_6()
                .py_4()
                .border_b_1()
                .border_color(border)
                .child(Label::new(title).text_base().font_semibold()),
        )
        .child(div().p_6().child(body))
}

impl Settings {
    fn appearance(&self, app: &Spk, cx: &App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(card(
                "Density",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div().flex().flex_wrap().gap_2().children(
                            [Density::Comfortable, Density::Compact]
                                .into_iter()
                                .map(|density| {
                                    let spk = self.spk.downgrade();
                                    let active = app.density == density;
                                    Button::new(density.label())
                                        .small()
                                        .label(density.label())
                                        .tab_stop(true)
                                        .selected(active)
                                        .when(active, |button| button.icon(IconName::Check))
                                        .on_click(move |_, _, cx| {
                                            let _ = spk.update(cx, |app, cx| {
                                                app.density = density;
                                                app.density_error = density
                                                    .save_to(preference_path("density"))
                                                    .is_err();
                                                cx.notify();
                                            });
                                        })
                                }),
                        ),
                    )
                    .when(app.density_error, |body| {
                        body.child(
                            Label::new("Could not save your density preference.")
                                .text_color(cx.theme().danger),
                        )
                    }),
                cx,
            ))
            .child(card(
                "Theme",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div().flex().flex_wrap().gap_2().children(
                            [Appearance::Dark, Appearance::Light, Appearance::System]
                                .into_iter()
                                .map(|appearance| {
                                    let spk = self.spk.downgrade();
                                    let active = app.appearance == appearance;
                                    Button::new(appearance.label())
                                        .small()
                                        .w(px(88.0))
                                        .label(appearance.label())
                                        .tab_stop(true)
                                        .selected(active)
                                        .when(active, |button| button.icon(IconName::Check))
                                        .on_click(move |_, window, cx| {
                                            let _ = spk.update(cx, |app, cx| {
                                                app.set_appearance(appearance, window, cx);
                                            });
                                        })
                                }),
                        ),
                    )
                    .when(app.appearance_error, |body| {
                        body.child(
                            Label::new("Could not save your theme preference.")
                                .text_color(cx.theme().danger),
                        )
                    }),
                cx,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::{Appearance, Density};

    #[test]
    fn density_defaults_to_comfortable_and_restores_saved_choices() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("density");
        assert!(Density::load_from(Some(path.clone())) == Density::Comfortable);
        for choice in [Density::Compact, Density::Comfortable] {
            Density::save_to(choice, Some(path.clone())).unwrap();
            assert!(Density::load_from(Some(path.clone())) == choice);
        }
        std::fs::write(&path, "unknown").unwrap();
        assert!(Density::load_from(Some(path)) == Density::Comfortable);
    }

    #[test]
    fn appearance_defaults_to_dark_and_restores_saved_modes() {
        assert!(Appearance::default() == Appearance::Dark);
        assert!(Appearance::from_preference("unknown") == Appearance::Dark);
        for choice in [Appearance::Dark, Appearance::Light, Appearance::System] {
            assert!(Appearance::from_preference(choice.label()) == choice);
        }
    }
}
