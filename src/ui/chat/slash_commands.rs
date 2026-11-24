use std::ops::Range;

use super::*;

#[derive(Clone, Copy)]
enum SlashCommand {
    Gif,
    Poll,
}

impl SlashCommand {
    const ALL: [Self; 2] = [Self::Gif, Self::Poll];

    fn name(self) -> &'static str {
        match self {
            Self::Gif => "gif",
            Self::Poll => "poll",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Gif => "Send a GIF",
            Self::Poll => "Create a poll",
        }
    }
}

impl Spk {
    pub(in crate::ui) fn on_chat_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.chat_input.read(cx).value();
        let command = value
            .strip_prefix('/')
            .and_then(|text| text.strip_suffix(' '))
            .and_then(|name| {
                SlashCommand::ALL
                    .into_iter()
                    .find(|command| command.name() == name)
            });
        self.slash_choice = Some(0);
        if let Some(command) = command
            && !window.has_active_dialog(cx)
            && self.chat_input.focus_handle(cx).is_focused(window)
            && chat(&self.view).is_some_and(|chat| chat.can_send())
        {
            self.apply_slash_command(command, 0..value.len(), window, cx);
        }
        self.update_chat_draft(cx);
    }

    fn slash_matches(&self, cx: &Context<Self>) -> Option<(Range<usize>, Vec<SlashCommand>)> {
        self.slash_choice?;
        if !chat(&self.view).is_some_and(|chat| chat.can_send()) {
            return None;
        }
        let input = self.chat_input.read(cx);
        let caret = input.selected_range();
        if !caret.is_empty() {
            return None;
        }
        let value = input.value();
        let prefix = value.get(..caret.end)?;
        let start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
        let query = prefix[start..].strip_prefix('/')?;
        let matches: Vec<_> = SlashCommand::ALL
            .into_iter()
            .filter(|command| command.name().starts_with(query))
            .collect();
        (!matches.is_empty()).then_some((start..caret.end, matches))
    }

    fn apply_slash_command(
        &mut self,
        command: SlashCommand,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.slash_choice = None;
        self.chat_input.update(cx, |input, cx| {
            input.set_selected_range(range, cx);
            input.replace("", window, cx);
            input.focus(window, cx);
        });
        match command {
            SlashCommand::Gif => self.send(Command::ShowGifs(true), None, cx),
            SlashCommand::Poll => self.open_poll_composer(window, cx),
        }
    }

    pub(in crate::ui) fn chat_slash_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((range, matches)) = self.slash_matches(cx) else {
            return false;
        };
        let selected = self.slash_choice.unwrap_or_default().min(matches.len() - 1);
        match key {
            "up" => self.slash_choice = Some((selected + matches.len() - 1) % matches.len()),
            "down" => self.slash_choice = Some((selected + 1) % matches.len()),
            "escape" => self.slash_choice = None,
            "enter" | "tab" => self.apply_slash_command(matches[selected], range, window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn slash_suggestions(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let (_, matches) = self.slash_matches(cx)?;
        let selected = self.slash_choice.unwrap_or_default().min(matches.len() - 1);
        let theme = cx.theme();
        Some(
            div()
                .id("chat-slash-suggestions")
                .flex()
                .flex_col()
                .items_start()
                .p_1()
                .children(matches.into_iter().enumerate().map(|(index, command)| {
                    control_button(command.name())
                        .ghost()
                        .small()
                        .tab_stop(true)
                        .accessibility_label(command.label())
                        .when(index == selected, |button| button.bg(theme.accent))
                        .child(Label::new(format!(
                            "/{}  {}",
                            command.name(),
                            command.label()
                        )))
                        .on_click(cx.listener(move |app, _, window, cx| {
                            if let Some((range, _)) = app.slash_matches(cx) {
                                app.apply_slash_command(command, range, window, cx);
                            }
                        }))
                })),
        )
    }
}
