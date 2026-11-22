use super::*;

impl Spk {
    pub(super) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.email.read(cx).value().trim().to_owned();
        let password = self.password.read(cx).value().to_string();
        let (command, busy) = match self.view {
            View::SignIn { .. } => {
                if email.is_empty() || password.is_empty() {
                    self.form_error = Some("Enter your email and password.");
                    cx.notify();
                    return;
                }
                (Command::SignIn { email, password }, "Signing in…")
            }
            View::AccountLink {
                form: AccountLinkForm::Link,
                ..
            } => (Command::SubmitAccountLink(password), "Checking link…"),
            View::AccountLink {
                form: AccountLinkForm::Invite,
                ..
            } => (
                Command::RedeemAccountLink { email, password },
                "Creating account…",
            ),
            View::AccountLink {
                form: AccountLinkForm::Reset,
                ..
            } => (
                Command::RedeemAccountLink {
                    email: String::new(),
                    password,
                },
                "Setting password…",
            ),
            View::ChooseNickname { .. } => (
                Command::SetInitialNickname(self.nickname.read(cx).value().to_string()),
                "Saving nickname…",
            ),
            _ => return,
        };
        self.clear_password(window, cx);
        self.send(command, Some(busy), cx);
    }

    fn clear_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    pub(super) fn focus_account_input(
        &mut self,
        view: &View,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = match view {
            View::SignIn { .. } => {
                self.nickname
                    .update(cx, |input, cx| input.set_value("", window, cx));
                if self.email.read(cx).value().is_empty() {
                    &self.email
                } else {
                    &self.password
                }
            }
            View::AccountLink {
                form: AccountLinkForm::Invite,
                ..
            } if self.email.read(cx).value().is_empty() => &self.email,
            View::AccountLink { .. } => &self.password,
            View::ChooseNickname { .. } => &self.nickname,
            _ => return,
        };
        window.focus(&input.focus_handle(cx), cx);
    }

    pub(super) fn login(
        &self,
        message: Option<&'static str>,
        retry_restore: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        account_card("Sign in to SPK", cx)
            .child(field(
                "Email",
                InputContentType::EmailAddress,
                &self.email,
                0,
            ))
            .child(field(
                "Password",
                InputContentType::Password,
                &self.password,
                1,
            ))
            .children(self.form_message(message, cx))
            .child(self.submit_button("Sign in", 2, cx).icon(IconName::LogIn))
            .child(
                Label::new("No account? You need an invite link from an admin.")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
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
            .child(
                Button::new("open-account-link")
                    .ghost()
                    .label("Use an invite or reset link")
                    .tab_index(4)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_password(window, cx);
                        this.send(Command::OpenAccountLink, Some("Opening account links…"), cx);
                    })),
            )
    }

    pub(super) fn account_link(
        &self,
        form: AccountLinkForm,
        message: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (title, button, label, content_type) = match form {
            AccountLinkForm::Link => (
                "Use an account link",
                "Check link",
                "Invite or password reset link",
                InputContentType::OneTimeCode,
            ),
            AccountLinkForm::Invite => (
                "Create your SPK account",
                "Create account",
                "Password",
                InputContentType::NewPassword,
            ),
            AccountLinkForm::Reset => (
                "Reset your password",
                "Set password",
                "New password",
                InputContentType::NewPassword,
            ),
        };
        account_card(title, cx)
            .when(matches!(form, AccountLinkForm::Invite), |card| {
                card.child(field(
                    "Email",
                    InputContentType::EmailAddress,
                    &self.email,
                    0,
                ))
            })
            .child(field(label, content_type, &self.password, 1))
            .when(!matches!(form, AccountLinkForm::Link), |card| {
                card.child(Label::new("At least 8 characters").text_sm())
            })
            .children(self.form_message(message, cx))
            .child(self.submit_button(button, 2, cx))
            .when(!matches!(form, AccountLinkForm::Link), |card| {
                card.child(
                    Button::new("change-account-link")
                        .ghost()
                        .label("Use a different link")
                        .tab_index(3)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.clear_password(window, cx);
                            this.send(Command::OpenAccountLink, Some("Opening account links…"), cx);
                        })),
                )
            })
            .child(
                Button::new("back-to-sign-in")
                    .ghost()
                    .label("Back to sign in")
                    .tab_index(4)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_password(window, cx);
                        this.send(Command::BackToSignIn, Some("Returning to sign in…"), cx);
                    })),
            )
    }

    pub(super) fn choose_nickname(
        &self,
        message: Option<&'static str>,
        notice: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        account_card("Welcome to SPK", cx)
            .child(field(
                "Nickname",
                InputContentType::Nickname,
                &self.nickname,
                0,
            ))
            .child(Label::new("2 to 32 characters").text_sm())
            .children(self.form_message(message, cx))
            .children(notice.map(|notice| Label::new(notice).text_sm()))
            .child(self.submit_button("Continue", 1, cx))
            .child(
                Button::new("onboarding-sign-out")
                    .ghost()
                    .label("Sign out")
                    .tab_index(2)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.send(Command::SignOut, Some("Signing out…"), cx);
                    })),
            )
    }

    fn submit_button(
        &self,
        label: &'static str,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new("submit-account-form")
            .primary()
            .label(label)
            .w_full()
            .tab_index(tab_index)
            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
    }

    fn form_message(&self, message: Option<&'static str>, cx: &Context<Self>) -> Option<Label> {
        self.form_error.or(message).map(|message| {
            Label::new(message)
                .text_sm()
                .text_color(if self.form_error.is_some() {
                    cx.theme().danger
                } else {
                    cx.theme().foreground
                })
        })
    }
}

fn account_card(title: &'static str, cx: &Context<Spk>) -> Div {
    div()
        .w_full()
        .max_w(px(384.0))
        .mx_auto()
        .p_6()
        .rounded_lg()
        .bg(cx.theme().popover)
        .border_1()
        .border_color(cx.theme().border)
        .flex()
        .flex_col()
        .gap_3()
        .child(
            Label::new(title)
                .mb_3()
                .text_center()
                .text_base()
                .font_bold(),
        )
}

fn field(
    label: &'static str,
    content_type: InputContentType,
    input: &Entity<InputState>,
    tab_index: isize,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(Label::new(label).id(label).font_medium())
        .child(
            Input::new(input)
                .aria_label(label)
                .content_type(content_type)
                .tab_index(tab_index),
        )
}
