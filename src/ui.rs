mod accounts;
mod chat;
mod notifications;
mod profile;
mod radio;
mod search;
mod settings;
mod soundboard;
mod switcher;
mod video;
mod voice;
mod workspace;
mod workspace_keys;

use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon, Selectable, Sizable, WindowExt,
        avatar::Avatar,
        button::{Button, ButtonVariants},
        input::{Input, InputContentType, InputEvent, InputState, Textarea, TextareaState},
        label::Label,
    },
    prelude::FluentBuilder,
    *,
};

use crate::{
    session::{self, AccountLinkForm, AvatarPhoto, Command, Sidebar, View},
    shortcuts::{Chord, Shortcuts},
    sounds::{Sound, SoundSetting, SoundSettings, VoiceSounds},
    voice::{DeviceKind, Status},
};

pub(super) use settings::ShortcutKeys;
use settings::{Appearance, Density};
use voice::VoiceControls;

const PRESENCE_IDLE_AFTER: Duration = Duration::from_secs(10 * 60);

pub(super) struct Spk {
    email: Entity<InputState>,
    password: Entity<InputState>,
    nickname: Entity<InputState>,
    chat_input: Entity<TextareaState>,
    gif_input: Entity<InputState>,
    gif_button_focus: FocusHandle,
    chat_pins: Option<chat::PinPanel>,
    chat_pins_focus: FocusHandle,
    latest_pin_focus: FocusHandle,
    all_pins_focus: FocusHandle,
    chat_attachments: chat::AttachmentUi,
    message_action_input: Entity<TextareaState>,
    message_action_form: Option<chat::MessageActionForm>,
    chat_row_focus: std::cell::RefCell<std::collections::HashMap<String, FocusHandle>>,
    chat_scroll: ScrollHandle,
    chat_scroll_anchor: Option<(String, Pixels)>,
    chat_history_position: Rc<Cell<Option<chat::HistoryPosition>>>,
    chat_spoiler_link: String,
    chat_spoilers: std::collections::HashMap<(String, u64), std::collections::HashSet<usize>>,
    commands: async_channel::Sender<Command>,
    view: View,
    form_error: Option<&'static str>,
    device_picker: Option<(VoiceControls, DeviceKind)>,
    selected_voice_channel: Option<String>,
    sidebar_open: bool,
    sidebar_width: Pixels,
    sidebar_drawer_open: bool,
    sidebar_toggle_focus: FocusHandle,
    new_text_channel_focus: FocusHandle,
    new_voice_channel_focus: FocusHandle,
    search_button_focus: FocusHandle,
    search_panel: Option<Entity<search::SearchPanel>>,
    voice_strip_toggle_focus: FocusHandle,
    quick_switcher: Option<WeakEntity<gpui_kit::component::command::CommandState>>,
    video_stage: video::VideoStage,
    workspace_focus: FocusHandle,
    settings_button_focus: FocusHandle,
    appearance: Appearance,
    appearance_error: bool,
    density: Density,
    density_error: bool,
    shortcuts: Shortcuts,
    sound_settings: tokio::sync::watch::Sender<SoundSettings>,
    cues: VoiceSounds,
    notification_preferences: crate::notifications::Preferences,
    notification_error: bool,
    alert_activation: tokio::sync::watch::Sender<Option<crate::api::MessageTarget>>,
    signing_out: bool,
    last_activity: Instant,
    activity: tokio::sync::watch::Sender<bool>,
}

impl Spk {
    pub(super) fn new(
        initial_alert: Option<crate::api::MessageTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        let chat_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .placeholder("Write a message")
                .submit_on_enter(true)
        });
        cx.subscribe_in(
            &chat_input,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::Change => this.on_chat_input(window, cx),
                InputEvent::PressEnter { shift: false, .. } => this.submit_chat(window, cx),
                _ => {}
            },
        )
        .detach();
        let gif_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe(&gif_input, |this, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.send(
                    Command::SearchGifs(input.read(cx).value().to_string()),
                    None,
                    cx,
                );
            }
        })
        .detach();
        let message_action_input =
            cx.new(|cx| TextareaState::new(window, cx).rows(2).submit_on_enter(true));
        cx.subscribe_in(&message_action_input, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                this.submit_message_action(cx);
            }
        })
        .detach();
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
        let (activity, presence_activity) = tokio::sync::watch::channel(window.is_window_active());
        let (sound_settings, sounds) = tokio::sync::watch::channel(shortcuts.sound_settings());
        let (alert_activation, targets) = tokio::sync::watch::channel(None);
        if initial_alert.is_some() {
            alert_activation.send_replace(initial_alert);
        }
        let cues = VoiceSounds::new(sounds);
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
                let _ = app.update(cx, |this, cx| {
                    this.record_activity(window);
                    if this.shortcuts.editor.is_some() {
                        this.capture_shortcut(event, window, cx);
                    } else if this.workspace_key(event, window, cx) {
                        cx.stop_propagation();
                        window.prevent_default();
                    }
                });
            }
        })
        .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.record_activity(window);
                this.read_visible_chat(window);
            } else {
                this.activity
                    .send_if_modified(|active| std::mem::replace(active, false));
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
        cx.observe_window_bounds(window, |this, window, cx| {
            if workspace::wide_workspace(window) {
                this.close_sidebar_drawer(window, cx);
            } else {
                this.focus_search_slideover(window, cx);
            }
        })
        .detach();

        let clock = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut until_idle = PRESENCE_IDLE_AFTER;
            loop {
                clock.timer(until_idle).await;
                let Ok(remaining) = this.update_in(cx, |this, _, _| {
                    let remaining =
                        PRESENCE_IDLE_AFTER.saturating_sub(this.last_activity.elapsed());
                    if remaining.is_zero() {
                        this.activity
                            .send_if_modified(|active| std::mem::replace(active, false));
                        PRESENCE_IDLE_AFTER
                    } else {
                        remaining
                    }
                }) else {
                    break;
                };
                until_idle = remaining;
            }
        })
        .detach();

        Self::track_chat_time(window, cx);

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
        let session_cues = cues.clone();
        std::thread::spawn(move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                runtime.block_on(session::run(
                    requests,
                    updates,
                    session_cues,
                    presence_activity,
                    targets,
                ));
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
                            this.close_sidebar_drawer(window, cx);
                            this.search_panel = None;
                            window.close_all_dialogs(cx);
                        }
                        this.focus_account_input(&view, window, cx);
                        this.set_session_view(view, window, cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.shortcuts.signed_in(false);
                this.close_sidebar_drawer(window, cx);
                this.search_panel = None;
                window.close_all_dialogs(cx);
                this.set_session_view(
                    View::SignIn {
                        message: Some("The SPK connection stopped. Close and reopen the app."),
                        retry_restore: false,
                    },
                    window,
                    cx,
                );
                cx.notify();
            });
        })
        .detach();

        Self {
            email,
            password,
            nickname,
            chat_input,
            gif_input,
            gif_button_focus: cx.focus_handle(),
            chat_pins: None,
            chat_pins_focus: cx.focus_handle().tab_stop(false),
            latest_pin_focus: cx.focus_handle(),
            all_pins_focus: cx.focus_handle(),
            chat_attachments: Default::default(),
            message_action_input,
            message_action_form: None,
            chat_row_focus: Default::default(),
            chat_scroll: ScrollHandle::new(),
            chat_scroll_anchor: None,
            chat_history_position: Rc::default(),
            chat_spoiler_link: format!("spk-spoiler:{}:", uuid::Uuid::new_v4()),
            chat_spoilers: Default::default(),
            commands,
            view: View::Busy("Checking saved session…"),
            form_error: None,
            device_picker: None,
            selected_voice_channel: None,
            sidebar_open: true,
            sidebar_width: workspace::SIDEBAR_WIDTH,
            sidebar_drawer_open: false,
            sidebar_toggle_focus: cx.focus_handle(),
            new_text_channel_focus: cx.focus_handle(),
            new_voice_channel_focus: cx.focus_handle(),
            search_button_focus: cx.focus_handle(),
            search_panel: None,
            voice_strip_toggle_focus: cx.focus_handle(),
            quick_switcher: None,
            video_stage: video::VideoStage::default(),
            workspace_focus,
            settings_button_focus: cx.focus_handle(),
            appearance,
            appearance_error: false,
            density: Density::load(),
            density_error: false,
            shortcuts,
            sound_settings,
            cues,
            notification_preferences: Default::default(),
            notification_error: false,
            alert_activation,
            signing_out: false,
            last_activity: Instant::now(),
            activity,
        }
    }

    fn record_activity(&mut self, window: &Window) {
        if window.is_window_active() {
            self.last_activity = Instant::now();
            self.activity
                .send_if_modified(|active| !std::mem::replace(active, true));
        }
    }

    fn observe_pointer<Event: MouseEvent>(app: &WeakEntity<Self>, window: &mut Window) {
        let app = app.clone();
        window.on_mouse_event(move |_: &Event, phase, window, cx| {
            if phase.capture() {
                let _ = app.update(cx, |app, _| app.record_activity(window));
            }
        });
    }

    fn set_session_view(&mut self, mut view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.update_chat(&view, window, cx);
        if let Some(user_id) = notifications::account_id(&view) {
            if notifications::account_id(&self.view) != Some(user_id) {
                self.notification_preferences = notifications::preferences_path(user_id)
                    .as_deref()
                    .map(crate::notifications::Preferences::load)
                    .unwrap_or_default();
                self.notification_error = false;
                window.clear_notifications(cx);
            }
        } else {
            self.notification_preferences = Default::default();
            self.notification_error = false;
        }
        if let View::SignedIn(previous) = &self.view {
            let next = match &view {
                View::SignedIn(sidebar) => Some(&sidebar.avatars),
                _ => None,
            };
            if !next.is_some_and(|next| std::sync::Arc::ptr_eq(&previous.avatars, next)) {
                for (id, image) in previous.avatars.iter() {
                    if !next
                        .and_then(|images| images.get(id))
                        .is_some_and(|next| Arc::ptr_eq(next, image))
                    {
                        profile::release_avatar(image, window, cx);
                    }
                }
            }
        }
        if let View::SignedIn(sidebar) = &mut view {
            if let (Some(id), Some(channels)) = (&self.selected_voice_channel, &sidebar.channels)
                && !channels
                    .iter()
                    .any(|channel| channel.id == *id && channel.kind == "voice")
            {
                self.selected_voice_channel = None;
            }
            for alert in std::mem::take(&mut sidebar.alerts) {
                self.show_alert(alert, sidebar, window, cx);
            }
            self.sync_search_conversation(sidebar, cx);
        } else if matches!(self.view, View::SignedIn(_)) {
            window.clear_notifications(cx);
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
        self.prepare_video_stage(window, cx);
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
            View::SignedIn(sidebar) => self.workspace(sidebar, window, cx).into_any_element(),
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
        let app = cx.weak_entity();
        div()
            .id("spk-window")
            .track_focus(&self.workspace_focus)
            .on_key_down(cx.listener(Self::dismiss_search_on_escape))
            .on_key_down(cx.listener(Self::dismiss_video_fullscreen))
            .on_key_down(cx.listener(Self::hovered_video_key))
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
            // Window-wide capture includes input over Profile and Settings dialogs.
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        Self::observe_pointer::<MouseDownEvent>(&app, window);
                        Self::observe_pointer::<MouseMoveEvent>(&app, window);
                        Self::observe_pointer::<ScrollEvent>(&app, window);
                    },
                )
                .absolute(),
            )
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
    photo: Option<&Arc<AvatarPhoto>>,
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

    let photo = Arc::clone(photo);
    let photo_id = photo.image.id.0;
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
            img(move |window: &mut Window, cx: &mut App| {
                Some(Ok(profile::avatar_thumbnail(&photo, diameter, window, cx)))
            })
            .id(("avatar-photo", photo_id))
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
