mod accounts;
mod radio;
mod settings;
mod soundboard;
mod video;
mod voice;
mod workspace;

use std::sync::{Arc, LazyLock};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon, Selectable, Sizable, WindowExt,
        avatar::Avatar,
        button::{Button, ButtonVariants},
        input::{Input, InputContentType, InputEvent, InputState},
        label::Label,
    },
    prelude::FluentBuilder,
    *,
};

use crate::{
    session::{self, AccountLinkForm, Command, Sidebar, View},
    shortcuts::{Chord, Shortcuts},
    sounds::{Sound, SoundSetting, SoundSettings},
    voice::{DeviceKind, Status},
};

use settings::Appearance;
pub(super) use settings::ShortcutKeys;
use voice::VoiceControls;

pub(super) struct Spk {
    email: Entity<InputState>,
    password: Entity<InputState>,
    nickname: Entity<InputState>,
    commands: async_channel::Sender<Command>,
    view: View,
    form_error: Option<&'static str>,
    device_picker: Option<(VoiceControls, DeviceKind)>,
    selected_voice_channel: Option<String>,
    sidebar_open: bool,
    video_stage: video::VideoStage,
    workspace_focus: FocusHandle,
    appearance: Appearance,
    appearance_error: bool,
    shortcuts: Shortcuts,
    sound_settings: tokio::sync::watch::Sender<SoundSettings>,
    signing_out: bool,
}

impl Spk {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = Appearance::load();
        appearance.apply(window, cx);
        cx.observe_window_appearance(window, |this, window, cx| {
            if this.appearance == Appearance::System {
                this.appearance.apply(window, cx);
            }
        })
        .detach();
        let email = cx.new(|cx| InputState::new(window, cx));
        let password = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_masked(true, window, cx);
            input
        });
        let nickname = cx.new(|cx| InputState::new(window, cx));
        let workspace_focus = cx.focus_handle().tab_stop(false);
        cx.on_focus_lost(window, |this, window, cx| {
            if matches!(this.view, View::SignedIn(_)) && !window.has_active_dialog(cx) {
                this.workspace_focus.focus(window, cx);
            }
        })
        .detach();
        for input in [&email, &password, &nickname] {
            cx.subscribe_in(input, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit(window, cx);
                }
            })
            .detach();
        }

        let (commands, requests) = async_channel::bounded(8);
        let (updates, views) = async_channel::bounded(16);
        let shortcuts = Shortcuts::new(window);
        let (sound_settings, sounds) = tokio::sync::watch::channel(shortcuts.sound_settings());
        let global_events = shortcuts.watch_events();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok((generation, event)) = global_events.recv().await {
                if this
                    .update_in(cx, |this, _, cx| {
                        if let Some(action) = this.shortcuts.global_action(generation, event) {
                            if let Some(editor) = &this.shortcuts.editor {
                                if editor.recording
                                    && let Some(binding) = this.shortcuts.binding(action)
                                {
                                    this.record_chord(Some(binding.chord.clone()), cx);
                                }
                            } else {
                                this.voice_action(action, cx);
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let app = cx.weak_entity();
        let window_id = window.window_handle().window_id();
        cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle().window_id() == window_id {
                let _ = app.update(cx, |this, cx| this.capture_shortcut(event, window, cx));
            }
        })
        .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.shortcuts.clear_focused();
                if this
                    .shortcuts
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.recording)
                {
                    this.cancel_shortcut_editor(cx);
                }
            }
        })
        .detach();

        let (finished, stopped) = tokio::sync::oneshot::channel();
        let shutdown = commands.clone();
        let app = cx.weak_entity();
        let mut stopped = Some(stopped);
        App::on_app_quit(cx, move |cx| {
            shutdown.close();
            let _ = app.update(cx, |this, _| this.shortcuts.signed_in(false));
            let stopped = stopped.take();
            async move {
                if let Some(stopped) = stopped {
                    let _ = stopped.await;
                }
            }
        })
        .detach();
        std::thread::spawn(move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                runtime.block_on(session::run(requests, updates, sounds));
            }
            let _ = finished.send(());
        });

        cx.spawn_in(window, async move |this, cx| {
            while let Ok(view) = views.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        if this.signing_out && matches!(view, View::SignedIn(_)) {
                            return;
                        }
                        this.signing_out = false;
                        let signed_in = matches!(view, View::SignedIn(_));
                        this.shortcuts.signed_in(signed_in);
                        if signed_in && !matches!(this.view, View::SignedIn(_)) {
                            this.workspace_focus.focus(window, cx);
                        } else if !signed_in {
                            this.selected_voice_channel = None;
                            this.device_picker = None;
                            window.close_all_dialogs(cx);
                        }
                        this.focus_account_input(&view, window, cx);
                        this.set_session_view(view, window);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.shortcuts.signed_in(false);
                window.close_all_dialogs(cx);
                this.set_session_view(
                    View::SignIn {
                        message: Some("The SPK connection stopped. Close and reopen the app."),
                        retry_restore: false,
                    },
                    window,
                );
                cx.notify();
            });
        })
        .detach();

        Self {
            email,
            password,
            nickname,
            commands,
            view: View::Busy("Checking saved session…"),
            form_error: None,
            device_picker: None,
            selected_voice_channel: None,
            sidebar_open: true,
            video_stage: video::VideoStage::default(),
            workspace_focus,
            appearance,
            appearance_error: false,
            shortcuts,
            sound_settings,
            signing_out: false,
        }
    }

    fn set_session_view(&mut self, view: View, window: &mut Window) {
        if let View::SignedIn(previous) = &self.view {
            let next = match &view {
                View::SignedIn(sidebar) => Some(&sidebar.avatars),
                _ => None,
            };
            if !next.is_some_and(|next| std::sync::Arc::ptr_eq(&previous.avatars, next)) {
                for (id, image) in previous.avatars.iter() {
                    if next.and_then(|images| images.get(id)) != Some(image) {
                        let _ = window.drop_image(image.clone());
                    }
                }
            }
        }
        self.view = view;
    }

    fn send(&mut self, command: Command, busy: Option<&'static str>, cx: &mut Context<Self>) {
        self.form_error = None;
        let signing_out = matches!(command, Command::SignOut);
        if self.commands.try_send(command).is_err() {
            self.form_error = Some("Could not reach the SPK connection. Try again.");
        } else {
            if signing_out {
                self.signing_out = true;
                self.shortcuts.signed_in(false);
            }
            if let Some(label) = busy {
                self.view = View::Busy(label);
            }
        }
        cx.notify();
    }

    fn cancel_shortcut_editor(&mut self, cx: &mut Context<Self>) {
        if self.shortcuts.editor.take().is_some() {
            cx.notify();
        }
    }

    fn capture_shortcut(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &self.shortcuts.editor else {
            return;
        };
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.cancel_shortcut_editor(cx);
        } else if !editor.recording {
            return;
        } else if key == "tab" {
            self.cancel_shortcut_editor(cx);
            return;
        } else if !event.keystroke.is_modifier_key() {
            let chord = if event.input_preference == InputPreference::CharacterInput {
                None
            } else {
                Chord::capture(&event.keystroke, window)
            };
            self.record_chord(chord, cx);
        }
        cx.stop_propagation();
        window.prevent_default();
    }

    fn record_chord(&mut self, chord: Option<Chord>, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.shortcuts.editor {
            editor.recording = chord.is_none();
            editor.error = chord.is_none().then_some(
                "Use F1–F24 or add Ctrl, Alt, or your platform modifier to a letter or digit.",
            );
            if chord.is_some() {
                editor.chord = chord;
            }
            cx.notify();
        }
    }

    fn change_sound(
        &mut self,
        sound: Sound,
        change: impl FnOnce(&mut SoundSetting),
        cx: &mut Context<Self>,
    ) {
        let mut sounds = self.shortcuts.sound_settings();
        change(sounds.get_mut(sound));
        self.shortcuts.set_sound_settings(sounds);
        self.sound_settings.send_replace(sounds);
        cx.notify();
    }
}

impl Render for Spk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_video_stage(window);
        let content = match &self.view {
            View::Busy(label) => Label::new(*label).into_any_element(),
            View::SignIn {
                message,
                retry_restore,
            } => self.login(*message, *retry_restore, cx).into_any_element(),
            View::AccountLink { form, message } => {
                self.account_link(*form, *message, cx).into_any_element()
            }
            View::ChooseNickname { message, notice } => self
                .choose_nickname(*message, *notice, cx)
                .into_any_element(),
            View::SignedIn(sidebar) => self.workspace(sidebar, cx).into_any_element(),
            View::RetrySignOut => div()
                .flex()
                .flex_col()
                .items_start()
                .gap_3()
                .child(Label::new(
                    "Could not sign out on SPK or clear the saved session.",
                ))
                .child(
                    Button::new("retry-sign-out")
                        .label("Retry sign-out")
                        .on_click(
                            cx.listener(|this, _, _, cx| this.send(Command::SignOut, None, cx)),
                        ),
                )
                .into_any_element(),
        };
        div()
            .id("spk-window")
            .track_focus(&self.workspace_focus)
            .on_key_down(cx.listener(Self::dismiss_video_fullscreen))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .text_sm()
            .flex()
            .flex_col()
            .when(!matches!(self.view, View::SignedIn(_)), |page| {
                page.justify_center().items_center().p_6()
            })
            .child(content)
    }
}

fn logo(size: f32) -> Img {
    static LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            include_bytes!("../assets/logo.svg").to_vec(),
        ))
    });
    img(Arc::clone(&LOGO)).size(px(size)).rounded_md()
}

fn profile_avatar(
    fallback: Avatar,
    photo: Option<&Arc<RenderImage>>,
    diameter: f32,
    speaking: bool,
    cx: &App,
) -> AnyElement {
    let size = px(diameter);
    let theme = cx.theme();
    let Some(photo) = photo else {
        return fallback
            .size(size)
            .when(speaking, |avatar| avatar.border_color(theme.success))
            .into_any_element();
    };

    div()
        .size(size)
        .flex_shrink_0()
        .rounded_full()
        .overflow_hidden()
        .bg(theme.tokens.secondary)
        .border_1()
        .border_color(if speaking {
            theme.success
        } else {
            theme.border
        })
        .child(
            img(Arc::clone(photo))
                .id(("avatar-photo", photo.id.0))
                .size_full()
                .rounded_full()
                .object_fit(ObjectFit::Cover),
        )
        .into_any_element()
}

fn tool_button(
    id: impl Into<ElementId>,
    icon: impl Into<Icon>,
    label: impl Into<SharedString>,
) -> Button {
    let label = label.into();
    Button::new(id)
        .ghost()
        .small()
        .icon(icon.into())
        .accessibility_label(label.clone())
        .tooltip(label)
        .tab_stop(true)
}

fn themed_icon(icon: IconName, color: Hsla, size: f32) -> Icon {
    Icon::new(icon).size(px(size)).text_color(color)
}
