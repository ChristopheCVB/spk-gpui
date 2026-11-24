use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

mod account_admin;
mod audio;
mod camera;
mod changelog;
mod notifications;
mod profile;
mod shortcuts;
mod soundboard_admin;

use gpui_kit::component::{
    Theme, ThemeMode, ThemeRegistry, WindowExt,
    menu::{DropdownMenu, PopupMenuItem},
    radio::RadioGroup,
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
    Dark,
    Light,
    #[default]
    System,
}

impl Appearance {
    pub fn load() -> Self {
        Self::load_from(preference_path("appearance"))
    }

    fn load_from(path: Option<PathBuf>) -> Self {
        match load_preference(path).as_deref() {
            Some("Dark") => Self::Dark,
            Some("Light") => Self::Light,
            _ => Self::System,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::System => "System",
        }
    }

    fn save_to(self, path: Option<PathBuf>) -> io::Result<()> {
        save_preference(path, self.label())
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
        self.appearance_error = appearance.save_to(preference_path("appearance")).is_err();
        appearance.apply(window, cx);
        cx.notify();
    }

    pub(super) fn compact(&self) -> bool {
        self.density == Density::Compact
    }
}

const SETTINGS_WIDE_WIDTH: Pixels = px(768.0);

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
    Soundboard,
}

impl Tab {
    const ALL: [Self; 9] = [
        Self::Profile,
        Self::Appearance,
        Self::Voice,
        Self::Notifications,
        Self::Shortcuts,
        Self::Soundboard,
        Self::Invites,
        Self::Users,
        Self::Changelog,
    ];

    fn navigation(self) -> (&'static str, IconName) {
        match self {
            Self::Profile => ("Profile", IconName::User),
            Self::Appearance => ("Appearance", IconName::Palette),
            Self::Voice => ("Voice & Video", IconName::Mic),
            Self::Notifications => ("Notifications", IconName::Bell),
            Self::Shortcuts => ("Shortcuts", IconName::Settings2),
            Self::Soundboard => ("Soundboard", IconName::Megaphone),
            Self::Invites => ("Invites", IconName::Link),
            Self::Users => ("Users", IconName::Users),
            Self::Changelog => ("Changelog", IconName::FileText),
        }
    }

    fn requires_admin(self) -> bool {
        matches!(self, Self::Invites | Self::Users | Self::Soundboard)
    }
}

impl Spk {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_sidebar_drawer(window, cx);
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
            cx.observe(&spk, |settings, _, cx| {
                settings.sync_account_admin(cx);
                settings.sync_soundboard_form(cx);
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
                nickname,
                invite_note,
                ban_reason,
                ban_target: None,
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
                soundboard: soundboard_admin::SoundboardForm::new(window, cx),
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
            if settings.tab == Tab::Soundboard && matches!(this.view, View::SignedIn(_)) {
                this.send(
                    Command::AdminSoundboard(crate::session::SoundboardAdminAction::Close),
                    None,
                    cx,
                );
            }
            settings.camera.release(window);
        })
        .detach();
        window.open_dialog(cx, move |dialog, window, _| {
            let viewport = window.viewport_size();
            dialog
                .centered()
                .border_0()
                .width(px(1152.0))
                .h(if viewport.width < SETTINGS_WIDE_WIDTH {
                    viewport.height
                } else {
                    viewport.height * 0.85
                })
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
    nickname: Entity<InputState>,
    invite_note: Entity<InputState>,
    ban_reason: Entity<InputState>,
    ban_target: Option<String>,
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
    soundboard: soundboard_admin::SoundboardForm,
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
        let compact = app.compact();
        let wide = window.viewport_size().width >= SETTINGS_WIDE_WIDTH;
        let navigation = self.navigation(sidebar.is_admin(), wide, compact, cx);
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
            Tab::Soundboard => self.soundboard_admin(sidebar, cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_sm()
            .bg(theme.popover)
            .font_family(theme.font_family.clone())
            .text_color(theme.foreground)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_start()
                    .gap_3()
                    .px(if compact { px(16.0) } else { px(24.0) })
                    .py(if compact { px(12.0) } else { px(16.0) })
                    .flex_shrink_0()
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
                    .when(!wide, |layout| layout.flex_col())
                    .flex_1()
                    .min_h_0()
                    .child(navigation)
                    .child(
                        div()
                            .id(("settings-body", self.tab as usize))
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p(if compact { px(16.0) } else { px(24.0) })
                            .child(body),
                    ),
            )
    }
}

impl Settings {
    fn navigation(
        &self,
        admin: bool,
        wide: bool,
        compact: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let tabs = Tab::ALL
            .into_iter()
            .filter(move |tab| admin || !tab.requires_admin());
        let navigation = div()
            .id("settings-navigation")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .border_color(theme.border)
            .p(if compact { px(8.0) } else { px(12.0) });
        if wide {
            navigation
                .w(px(208.0))
                .min_h_0()
                .overflow_y_scroll()
                .border_r_1()
                .children(tabs.map(|tab| {
                    let (label, icon) = tab.navigation();
                    let active = self.tab == tab;
                    let color = if active {
                        theme.primary
                    } else {
                        theme.muted_foreground
                    };
                    control_button(label)
                        .ghost()
                        .small()
                        .tab_stop(true)
                        .selected(active)
                        .accessibility_label(label)
                        .h_8()
                        .flex_shrink_0()
                        .when(active, |button| button.bg(theme.primary.opacity(0.10)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .w_full()
                                .gap(CONTROL_PADDING)
                                .child(Icon::new(icon).size(ACTION_ICON_SIZE).text_color(color))
                                .child(Label::new(label).text_color(color).truncate()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.select_tab(tab, cx)))
                }))
        } else {
            let active = self.tab;
            let (label, icon) = active.navigation();
            let settings = cx.weak_entity();
            navigation.border_b_1().child(
                control_button("settings-sections")
                    .small()
                    .outline()
                    .tab_stop(true)
                    .accessibility_label(format!("Settings sections, {label}"))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(CONTROL_PADDING)
                            .child(
                                Icon::new(icon)
                                    .size(ACTION_ICON_SIZE)
                                    .text_color(theme.foreground),
                            )
                            .child(Label::new(label).flex_1())
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .size(ACTION_ICON_SIZE)
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .dropdown_menu(move |mut menu, window, _| {
                        menu = menu.max_h(window.viewport_size().height - px(136.0));
                        for tab in tabs.clone() {
                            let (label, icon) = tab.navigation();
                            let settings = settings.clone();
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .icon(icon)
                                    .checked(active == tab)
                                    .on_click(move |_, _, cx| {
                                        let _ = settings.update(cx, |settings, cx| {
                                            settings.select_tab(tab, cx);
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
        }
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if tab.requires_admin()
            && !matches!(&self.spk.read(cx).view, View::SignedIn(sidebar) if sidebar.is_admin())
        {
            return;
        }
        if self.tab == Tab::Shortcuts && tab != Tab::Shortcuts {
            self.spk
                .update(cx, |app, cx| app.cancel_shortcut_editor(cx));
        }
        let old_admin_page = self.tab.admin_page();
        let admin_page = tab.admin_page();
        if old_admin_page != admin_page {
            self.ban_target = None;
            self.account_action(AdminAccountAction::Show(admin_page), cx);
        }
        if self.tab != tab {
            use crate::session::SoundboardAdminAction;
            if self.tab == Tab::Soundboard {
                self.soundboard_action(SoundboardAdminAction::Close, cx);
            } else if tab == Tab::Soundboard {
                self.soundboard_action(SoundboardAdminAction::Show, cx);
            }
        }
        self.tab = tab;
        self.sync_mic_test(cx);
        self.sync_camera_preview(cx);
        if tab == Tab::Voice {
            self.refresh_cameras(cx);
            self.spk
                .update(cx, |app, cx| app.send(Command::RefreshDevices, None, cx));
        }
        cx.notify();
    }
}

fn card(title: &'static str, body: impl IntoElement, cx: &App) -> impl IntoElement + Styled {
    div()
        .id(title)
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(cx.theme().background)
        .child(
            div()
                .px_6()
                .py_4()
                .child(Label::new(title).text_base().font_semibold()),
        )
        .child(div().p_6().child(body))
}

impl Settings {
    fn appearance(&self, app: &Spk, cx: &App) -> impl IntoElement {
        const THEMES: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];
        const DENSITIES: [Density; 2] = [Density::Comfortable, Density::Compact];
        let theme_app = self.spk.downgrade();
        let density_app = theme_app.clone();
        div()
            .flex()
            .flex_col()
            .gap(if app.compact() { px(8.0) } else { px(16.0) })
            .child(card(
                "Theme",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        RadioGroup::vertical("theme-choices")
                            .children(THEMES.map(Appearance::label))
                            .selected_index(
                                THEMES.iter().position(|choice| *choice == app.appearance),
                            )
                            .on_change(move |&index, window, cx| {
                                let _ = theme_app.update(cx, |app, cx| {
                                    app.set_appearance(THEMES[index], window, cx);
                                });
                            }),
                    )
                    .when(app.appearance_error, |body| {
                        body.child(
                            Label::new("Could not save your theme preference.")
                                .text_color(cx.theme().danger),
                        )
                    }),
                cx,
            ))
            .child(card(
                "Density",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        RadioGroup::vertical("density-choices")
                            .children(DENSITIES.map(Density::label))
                            .selected_index(
                                DENSITIES.iter().position(|choice| *choice == app.density),
                            )
                            .on_change(move |&index, _, cx| {
                                let _ = density_app.update(cx, |app, cx| {
                                    app.density = DENSITIES[index];
                                    app.density_error =
                                        app.density.save_to(preference_path("density")).is_err();
                                    cx.notify();
                                });
                            }),
                    )
                    .when(app.density_error, |body| {
                        body.child(
                            Label::new("Could not save your density preference.")
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
    fn appearance_defaults_to_system_and_restores_saved_modes() {
        assert!(Appearance::default() == Appearance::System);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance");
        assert!(Appearance::load_from(Some(path.clone())) == Appearance::System);
        for choice in [Appearance::Dark, Appearance::Light, Appearance::System] {
            Appearance::save_to(choice, Some(path.clone())).unwrap();
            assert!(Appearance::load_from(Some(path.clone())) == choice);
        }
        std::fs::write(&path, "unknown").unwrap();
        assert!(Appearance::load_from(Some(path)) == Appearance::System);
    }
}
