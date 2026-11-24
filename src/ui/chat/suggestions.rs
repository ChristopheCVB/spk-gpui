use std::ops::Range;

use gpui_kit::component::ThemeStyled;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Trigger {
    Slash,
    Mention,
    Emoji,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SuggestionMatch {
    trigger: Trigger,
    range: Range<usize>,
    query: String,
}

fn match_prefix(source: &str, prefix: Range<usize>, allow_slash: bool) -> Option<SuggestionMatch> {
    let text = source.get(prefix.clone())?;
    [
        (Trigger::Slash, '/'),
        (Trigger::Mention, '@'),
        (Trigger::Emoji, ':'),
    ]
    .into_iter()
    .filter_map(|(trigger, symbol)| {
        let start = text.rfind(symbol)?;
        let query = &text[start + 1..];
        if query
            .chars()
            .any(|ch| ch.is_whitespace() || ch == '\u{feff}')
        {
            return None;
        }
        let allowed = match trigger {
            Trigger::Slash => allow_slash && start == 0,
            Trigger::Mention | Trigger::Emoji => start == 0 || text[..start].ends_with(' '),
        };
        allowed.then(|| SuggestionMatch {
            trigger,
            range: prefix.start + start..prefix.end,
            query: query.to_owned(),
        })
    })
    .max_by_key(|matched| matched.range.start)
}

fn matching(input: &TextareaState, allow_slash: bool) -> Option<SuggestionMatch> {
    let caret = input.selected_range();
    if !caret.is_empty() {
        return None;
    }
    let value = input.value();
    match_prefix(
        &value,
        editor::suggestion_prefix(input, &value, caret.end)?,
        allow_slash,
    )
}

pub(super) fn filter<T>(
    query: &str,
    items: impl IntoIterator<Item = T>,
    fields: impl Fn(&T) -> Vec<&str>,
) -> Vec<T> {
    const LIMIT: usize = 42;
    if query.is_empty() {
        return items.into_iter().take(LIMIT).collect();
    }
    let normalize = |value: &str| {
        if value.is_ascii() {
            return value.to_ascii_lowercase();
        }
        use icu_properties::{
            CodePointMapData,
            props::{GeneralCategory, GeneralCategoryGroup},
        };
        let categories = CodePointMapData::<GeneralCategory>::new();
        icu_normalizer::DecomposingNormalizer::new_nfd()
            .normalize_iter(value.chars().flat_map(char::to_lowercase))
            .filter(|ch| !GeneralCategoryGroup::Mark.contains(categories.get(*ch)))
            .collect::<String>()
    };
    let query = normalize(query);
    let score = |value: &str| {
        if value == query {
            Some(0)
        } else if value.starts_with(&query) {
            Some(1)
        } else {
            value.contains(&query).then_some(2)
        }
    };
    let mut scored: Vec<_> = items
        .into_iter()
        .filter_map(|item| {
            let rank = fields(&item)
                .into_iter()
                .filter_map(|field| {
                    let field = normalize(field);
                    let rank = score(&field);
                    if rank == Some(0) || !field.contains([' ', '_', '-']) {
                        return rank;
                    }
                    let compact: String = field
                        .chars()
                        .filter(|ch| !matches!(ch, ' ' | '_' | '-'))
                        .collect();
                    rank.into_iter().chain(score(&compact)).min()
                })
                .min()?;
            Some((rank, item))
        })
        .collect();
    scored.sort_by_key(|(rank, _)| *rank);
    scored
        .into_iter()
        .take(LIMIT)
        .map(|(_, item)| item)
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
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

    fn description(self) -> &'static str {
        match self {
            Self::Gif => "Search for a GIF",
            Self::Poll => "Create a poll",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Gif => IconName::ImagePlay,
            Self::Poll => IconName::ChartBar,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Item {
    Emoji(&'static reaction_picker::Emoji),
    Command(SlashCommand),
    Mention { id: String, nickname: String },
}

impl Item {
    fn label(&self) -> String {
        match self {
            Self::Emoji(emoji) => emoji.annotation.clone(),
            Self::Command(command) => format!("/{}", command.name()),
            Self::Mention { nickname, .. } => nickname.clone(),
        }
    }

    fn description(&self) -> Option<&str> {
        match self {
            Self::Emoji(_) => None,
            Self::Command(command) => Some(command.description()),
            Self::Mention { .. } => None,
        }
    }

    fn leading(&self, app: &Spk, cx: &App) -> AnyElement {
        match self {
            Self::Emoji(emoji) => reaction_glyph(&emoji.emoji, px(16.)),
            Self::Command(command) => Icon::new(command.icon())
                .size(px(16.))
                .text_color(cx.theme().muted_foreground)
                .into_any_element(),
            Self::Mention { id, nickname } => {
                let photo = match &app.view {
                    View::SignedIn(sidebar) => sidebar.avatar(Some(id)),
                    _ => None,
                };
                profile_avatar(Avatar::new().name(nickname.clone()), photo, 20., false, cx)
            }
        }
    }
}

fn emoji_items(query: &str) -> Vec<Item> {
    filter(query, reaction_picker::EMOJI.iter(), |emoji| {
        std::iter::once(emoji.annotation.as_str())
            .chain(emoji.shortcodes.iter().map(String::as_str))
            .chain(emoji.tags.iter().map(String::as_str))
            .collect()
    })
    .into_iter()
    .map(Item::Emoji)
    .collect()
}

#[derive(Default)]
pub(in crate::ui) struct MenuState {
    matched: Option<SuggestionMatch>,
    selected: usize,
    dismissed: bool,
    scroll: ScrollHandle,
}

impl MenuState {
    fn sync(&mut self, matched: Option<SuggestionMatch>) {
        if self.matched != matched {
            if !self
                .matched
                .as_ref()
                .zip(matched.as_ref())
                .is_some_and(|(previous, next)| {
                    previous.trigger == next.trigger && previous.range.start == next.range.start
                })
            {
                self.selected = 0;
                self.scroll.scroll_to_item(0);
            }
            self.matched = matched;
            self.dismissed = false;
        }
    }

    pub(in crate::ui) fn dismiss(&mut self) {
        self.dismissed = true;
        self.selected = 0;
        self.scroll.scroll_to_item(0);
    }
}

impl Spk {
    fn suggestion_input(&self, editing: bool) -> &Entity<TextareaState> {
        if editing {
            &self.message_action_input
        } else {
            &self.chat_input
        }
    }

    fn suggestion_state(&self, editing: bool) -> &MenuState {
        &self.suggestions[usize::from(editing)]
    }

    fn suggestion_state_mut(&mut self, editing: bool) -> &mut MenuState {
        &mut self.suggestions[usize::from(editing)]
    }

    pub(in crate::ui) fn sync_chat_suggestions(&mut self, editing: bool, cx: &mut Context<Self>) {
        let matched = matching(self.suggestion_input(editing).read(cx), !editing);
        if self.suggestion_state(editing).matched != matched {
            let count = matched
                .as_ref()
                .map_or(0, |matched| self.suggestion_items(matched).len());
            let state = self.suggestion_state_mut(editing);
            state.sync(matched);
            state.selected = state.selected.min(count.saturating_sub(1));
            state.scroll.scroll_to_item(state.selected);
        }
        cx.notify();
    }

    fn suggestion_items(&self, matched: &SuggestionMatch) -> Vec<Item> {
        match matched.trigger {
            Trigger::Emoji => emoji_items(&matched.query),
            Trigger::Slash => filter(&matched.query, SlashCommand::ALL, |command| {
                vec![command.name()]
            })
            .into_iter()
            .map(Item::Command)
            .collect(),
            Trigger::Mention => match &self.view {
                View::SignedIn(sidebar) => filter(
                    &matched.query,
                    sidebar.mention_directory(),
                    |(_, nickname)| vec![*nickname],
                )
                .into_iter()
                .map(|(id, nickname)| Item::Mention {
                    id: id.to_owned(),
                    nickname: nickname.to_owned(),
                })
                .collect(),
                _ => Vec::new(),
            },
        }
    }

    fn suggestions_at_caret(
        &self,
        editing: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<(SuggestionMatch, Vec<Item>)> {
        let chat = chat(&self.view)?;
        let input = self.suggestion_input(editing).read(cx);
        if !window.is_window_active()
            || !input.focus_handle(cx).is_focused(window)
            || (if editing {
                chat.action_pending
            } else {
                !chat.can_send() || chat.gifs.is_some()
            })
        {
            return None;
        }
        let matched = matching(input, !editing)?;
        let state = self.suggestion_state(editing);
        if state.matched.as_ref() == Some(&matched) && state.dismissed {
            return None;
        }
        let items = self.suggestion_items(&matched);
        (!items.is_empty()).then_some((matched, items))
    }

    pub(in crate::ui) fn on_chat_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.chat_input.read(cx);
        let value = input.value();
        if (value.contains("/poll ") || value.contains("/gif "))
            && input.focus_handle(cx).is_focused(window)
            && !window.has_active_dialog(cx)
            && chat(&self.view).is_some_and(ChatView::can_send)
        {
            let text = editor::plain_text(&value);
            if let Some(command) = text
                .strip_prefix('/')
                .and_then(|text| text.strip_suffix(' '))
                .and_then(|name| {
                    SlashCommand::ALL
                        .into_iter()
                        .find(|command| command.name() == name)
                })
            {
                self.apply_suggestion(false, Item::Command(command), 0..value.len(), window, cx);
            }
        }
        self.update_chat_draft(cx);
    }

    fn apply_suggestion(
        &mut self,
        editing: bool,
        item: Item,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.suggestion_state_mut(editing).dismiss();
        match item {
            Item::Emoji(emoji) => self.suggestion_input(editing).update(cx, |input, cx| {
                input.set_selected_range(range, cx);
                input.replace(&emoji.emoji, window, cx);
                input.focus(window, cx);
            }),
            Item::Command(command) => {
                self.chat_input.update(cx, |input, cx| {
                    let range = editor::empty_suggestion_range(&input.value(), range);
                    input.set_selected_range(range, cx);
                    input.replace("", window, cx);
                    input.focus(window, cx);
                });
                match command {
                    SlashCommand::Gif => self.send(Command::ShowGifs(true), None, cx),
                    SlashCommand::Poll => self.open_poll_composer(window, cx),
                }
            }
            Item::Mention { id, .. } => {
                self.suggestion_input(editing).update(cx, |input, cx| {
                    editor::replace_with_mention(input, range, &id, window, cx);
                });
                self.refresh_chat_editor(editing, cx);
            }
        }
    }

    pub(in crate::ui) fn chat_suggestion_key(
        &mut self,
        key: &str,
        editing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(key, "up" | "down" | "escape" | "enter" | "tab")
            || self.suggestion_input(editing).update(cx, |input, cx| {
                input.marked_text_range(window, cx).is_some()
            })
        {
            return false;
        }
        let Some((matched, items)) = self.suggestions_at_caret(editing, window, cx) else {
            return false;
        };
        let state = self.suggestion_state_mut(editing);
        state.sync(Some(matched.clone()));
        let selected = state.selected.min(items.len() - 1);
        match key {
            "up" | "down" => {
                let step = if key == "up" { items.len() - 1 } else { 1 };
                state.selected = (selected + step) % items.len();
                state.scroll.scroll_to_item(state.selected);
            }
            "escape" => state.dismiss(),
            _ => self.apply_suggestion(editing, items[selected].clone(), matched.range, window, cx),
        }
        cx.notify();
        true
    }

    pub(super) fn suggestion_menu(
        &self,
        editing: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        let (matched, items) = self.suggestions_at_caret(editing, window, cx)?;
        let state = self.suggestion_state(editing);
        let selected = state.selected.min(items.len() - 1);
        let theme = cx.theme();
        Some(
            div()
                .id("message-suggestions")
                .role(Role::ListBox)
                .aria_label(match matched.trigger {
                    Trigger::Slash => "Slash commands",
                    Trigger::Mention => "Mention suggestions",
                    Trigger::Emoji => "Emoji suggestions",
                })
                .popover_style(cx)
                .flex()
                .flex_col()
                .flex_shrink_0()
                .self_start()
                .max_w_full()
                .max_h(px(218.))
                .overflow_y_scroll()
                .track_scroll(&state.scroll)
                .p_1()
                .children(items.into_iter().enumerate().map(|(index, item)| {
                    let label = item.label();
                    let description = item.description().map(str::to_owned);
                    let matched = matched.clone();
                    let for_hover = matched.clone();
                    let for_click = matched.clone();
                    let activate = item.clone();
                    div()
                        .id(index)
                        .role(Role::ListBoxOption)
                        .aria_label(label.clone())
                        .aria_selected(index == selected)
                        .when_some(description.clone(), |row, description| {
                            row.aria_description(description)
                        })
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .h(px(30.))
                        .flex_shrink_0()
                        .rounded_md()
                        .when(index == selected, |row| row.bg(theme.accent))
                        .child(item.leading(self, cx))
                        .child(Label::new(label).font_semibold().truncate())
                        .when_some(description, |row, description| {
                            row.child(
                                Label::new(description)
                                    .text_color(theme.muted_foreground)
                                    .truncate(),
                            )
                        })
                        .on_hover(cx.listener(move |app, hovered: &bool, _, cx| {
                            let state = app.suggestion_state_mut(editing);
                            if *hovered && state.matched.as_ref() == Some(&for_hover) {
                                state.selected = index;
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |app, _, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                app.pick_suggestion(editing, &matched, &item, window, cx);
                            }),
                        )
                        .on_click(cx.listener(move |app, event, window, cx| {
                            if !matches!(event, ClickEvent::Mouse(_)) {
                                app.pick_suggestion(editing, &for_click, &activate, window, cx);
                            }
                        }))
                })),
        )
    }

    fn pick_suggestion(
        &mut self,
        editing: bool,
        displayed: &SuggestionMatch,
        item: &Item,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((matched, items)) = self.suggestions_at_caret(editing, window, cx)
            && &matched == displayed
            && items.contains(item)
        {
            self.apply_suggestion(editing, item.clone(), matched.range, window, cx);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SuggestionMatch, Trigger, filter, match_prefix};

    #[test]
    fn emoji_menu_searches_the_web_catalog_and_matches_at_the_caret() {
        use super::emoji_items;

        assert_eq!(emoji_items("").len(), 42);
        assert_eq!(emoji_items("")[0].label(), "grinning face");
        assert_eq!(emoji_items("THUMBSUP")[0].label(), "thumbs up");
        assert_eq!(emoji_items("pepperoni")[0].label(), "pizza");

        let source = "👍 Choose :redheart, please";
        let caret = source.find(',').unwrap();
        let matched = match_prefix(source, 0..caret, false).unwrap();
        assert_eq!(matched.trigger, Trigger::Emoji);
        assert_eq!(&source[matched.range], ":redheart");
        assert_eq!(emoji_items(&matched.query)[0].label(), "red heart");
        for source in [
            "https://example.com",
            "word:joy",
            "::joy",
            ":joy:",
            "x\t:joy",
        ] {
            assert!(
                match_prefix(source, 0..source.len(), false).is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn suggestions_rank_names_and_keep_a_bounded_stable_menu() {
        let names = ["A Noël party", "Noëlle", "NOËL", "N-O_E L", "Christmas"];
        assert_eq!(
            filter("noel", names, |name| vec![*name]),
            ["NOËL", "N-O_E L", "Noëlle", "A Noël party"],
        );
        assert_eq!(filter("", 0..50, |_| vec![]), (0..42).collect::<Vec<_>>(),);
    }

    #[test]
    fn slash_matches_the_text_segment_before_the_caret() {
        assert_eq!(
            match_prefix("/pOll tail", 0..3, true),
            Some(SuggestionMatch {
                trigger: Trigger::Slash,
                range: 0..3,
                query: "pO".into(),
            }),
        );
        for source in [" /poll", "x/poll", "/po/ll", "/poll "] {
            assert!(
                match_prefix(source, 0..source.len(), true).is_none(),
                "{source}"
            );
        }
        let styled = "before **/poll**";
        assert_eq!(match_prefix(styled, 9..14, true).unwrap().range, 9..14);
        let hard_break = "before\n/poll";
        assert_eq!(match_prefix(hard_break, 7..12, true).unwrap().range, 7..12);
        assert!(match_prefix("/poll", 0..5, false).is_none());
    }
}
