mod api;
mod session;
mod ui;
mod voice;

use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, ThemeRegistry, WindowExt,
        button::{Button, ButtonVariants},
        input::{Input, InputContentType, InputEvent, InputState},
        label::Label,
    },
    prelude::FluentBuilder,
    *,
};

use session::{Command, View};
use ui::{Appearance, VoiceControls, logo};
use voice::DeviceKind;

struct Spk {
    email: Entity<InputState>,
    password: Entity<InputState>,
    commands: async_channel::Sender<Command>,
    view: View,
    form_error: Option<&'static str>,
    device_picker: Option<(VoiceControls, DeviceKind)>,
    selected_room: Option<String>,
    sidebar_open: bool,
    appearance: Appearance,
    appearance_error: bool,
}

impl Spk {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        for input in [&email, &password] {
            cx.subscribe_in(input, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit(window, cx);
                }
            })
            .detach();
        }

        let (commands, requests) = async_channel::bounded(8);
        let (updates, views) = async_channel::bounded(16);
        let (finished, stopped) = tokio::sync::oneshot::channel();
        let shutdown = commands.clone();
        let mut stopped = Some(stopped);
        App::on_app_quit(cx, move |_| {
            shutdown.close();
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
                runtime.block_on(session::run(requests, updates));
            }
            let _ = finished.send(());
        });

        cx.spawn_in(window, async move |this, cx| {
            while let Ok(view) = views.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        if !matches!(view, View::SignedIn(_)) {
                            this.selected_room = None;
                            this.device_picker = None;
                            window.close_all_dialogs(cx);
                        }
                        if matches!(view, View::SignIn { .. }) {
                            let input = if this.email.read(cx).value().is_empty() {
                                &this.email
                            } else {
                                &this.password
                            };
                            let handle = input.focus_handle(cx);
                            window.focus(&handle, cx);
                        }
                        this.view = view;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                window.close_all_dialogs(cx);
                this.view = View::SignIn {
                    message: Some("The SPK connection stopped. Close and reopen the app."),
                    retry_restore: false,
                };
                cx.notify();
            });
        })
        .detach();

        Self {
            email,
            password,
            commands,
            view: View::Busy("Checking saved session…"),
            form_error: None,
            device_picker: None,
            selected_room: None,
            sidebar_open: true,
            appearance,
            appearance_error: false,
        }
    }

    fn send(&mut self, command: Command, busy: Option<&'static str>, cx: &mut Context<Self>) {
        self.form_error = None;
        if self.commands.try_send(command).is_err() {
            self.form_error = Some("Could not reach the SPK connection. Try again.");
        } else if let Some(label) = busy {
            self.view = View::Busy(label);
        }
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.view, View::SignIn { .. }) {
            return;
        }
        let email = self.email.read(cx).value().trim().to_owned();
        let password = self.password.read(cx).value().to_string();
        if email.is_empty() || password.is_empty() {
            self.form_error = Some("Enter your email and password.");
            cx.notify();
            return;
        }
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.send(Command::SignIn { email, password }, Some("Signing in…"), cx);
    }

    fn login(
        &self,
        message: Option<&'static str>,
        retry_restore: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .w_full()
            .max_w(px(360.0))
            .mx_auto()
            .p_6()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(logo(32.0))
                    .child(Label::new("Sign in to SPK").text_xl().font_semibold()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new("Email"))
                    .child(
                        Input::new(&self.email)
                            .aria_label("Email")
                            .content_type(InputContentType::EmailAddress)
                            .tab_index(0),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new("Password"))
                    .child(
                        Input::new(&self.password)
                            .aria_label("Password")
                            .content_type(InputContentType::Password)
                            .tab_index(1),
                    ),
            )
            .children(
                self.form_error
                    .or(message)
                    .map(|message| Label::new(message).text_sm().text_color(cx.theme().danger)),
            )
            .child(
                Button::new("sign-in")
                    .primary()
                    .label("Sign in")
                    .tab_index(2)
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            )
            .when(retry_restore, |card| {
                card.child(
                    Button::new("restore")
                        .label("Retry saved session")
                        .tab_index(3)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.send(Command::Restore, Some("Checking saved session…"), cx);
                        })),
                )
            })
    }
}

impl Render for Spk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.view {
            View::Busy(label) => Label::new(*label).into_any_element(),
            View::SignIn {
                message,
                retry_restore,
            } => self.login(*message, *retry_restore, cx).into_any_element(),
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

fn main() {
    application().with_assets(assets::AllAssets).run(|cx| {
        init(cx);
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(include_str!("../assets/themes.json"))
            .expect("SPK themes should be valid");

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1200.0), px(760.0)), cx)),
            window_min_size: Some(size(px(560.0), px(440.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("SPK".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        open_window(options, cx, |window, cx| cx.new(|cx| Spk::new(window, cx)))
            .expect("Failed to open the SPK window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
