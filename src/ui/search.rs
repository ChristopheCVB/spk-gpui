use std::cell::Cell;

use gpui_kit::component::{
    Disableable,
    menu::{DropdownMenu, PopupMenuItem},
};
use url::Url;

use super::*;
use crate::{
    api::{MessageTarget, SearchHit, SearchScope},
    chat_markdown::{message_markdown, plain_text},
};

static SPK_BASE: LazyLock<Url> =
    LazyLock::new(|| Url::parse(env!("SPK_URL")).expect("validated SPK URL"));

struct SearchPanel {
    spk: Entity<Spk>,
    query: Entity<InputState>,
    scopes: Vec<(SearchScope, &'static str)>,
    selected: usize,
    selected_result: Option<MessageTarget>,
    results_scroll: ScrollHandle,
    _keyboard: Subscription,
}

impl Spk {
    pub(super) fn open_message_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(false, window, cx);
    }

    pub(super) fn open_search(
        &mut self,
        everything: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let scopes = sidebar
            .chat
            .as_ref()
            .filter(|chat| !chat.unavailable())
            .map(|chat| {
                (
                    SearchScope::Conversation(chat.channel_id.clone()),
                    if chat.direct_message_peer.is_some() {
                        "This conversation"
                    } else {
                        "This channel"
                    },
                )
            })
            .into_iter()
            .chain([
                (SearchScope::TextChannels, "All channels"),
                (SearchScope::DirectMessages, "DMs"),
                (SearchScope::Everything, "Everything"),
            ])
            .collect::<Vec<_>>();
        let selected = if everything { scopes.len() - 1 } else { 0 };
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search messages"));
        let spk = cx.entity();
        let panel_query = query.clone();
        let panel = cx.new(|cx: &mut Context<SearchPanel>| {
            cx.observe(&spk, |_, _, cx| cx.notify()).detach();
            cx.subscribe(&panel_query, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.selected_result = None;
                    cx.notify();
                }
            })
            .detach();
            let panel = cx.weak_entity();
            let window_id = window.window_handle().window_id();
            let keyboard = cx.intercept_keystrokes(move |event, window, cx| {
                let key = &event.keystroke;
                if window.window_handle().window_id() == window_id
                    && !key.modifiers.modified()
                    && matches!(key.key.as_str(), "up" | "down" | "enter")
                    && panel
                        .update(cx, |panel, cx| {
                            if !panel.query.focus_handle(cx).is_focused(window) {
                                return false;
                            }
                            if key.key == "enter" {
                                if let Some(target) = panel.selected_result.clone() {
                                    panel.open_result(target, window, cx);
                                } else {
                                    panel.search(cx);
                                }
                                true
                            } else {
                                panel.step_result(key.key == "up", cx)
                            }
                        })
                        .unwrap_or(false)
                {
                    cx.stop_propagation();
                    window.prevent_default();
                }
            });
            SearchPanel {
                spk,
                query: panel_query,
                scopes,
                selected,
                selected_result: None,
                results_scroll: ScrollHandle::new(),
                _keyboard: keyboard,
            }
        });
        self.send(
            Command::SearchMessages {
                query: String::new(),
                scope: SearchScope::Everything,
            },
            None,
            cx,
        );

        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                query.focus_handle(cx).focus(window, cx);
            }
            dialog
                .centered()
                .width(px(500.0))
                .title("Search")
                .child(panel.clone())
        });
    }
}

impl SearchPanel {
    fn search(&mut self, cx: &mut Context<Self>) {
        self.selected_result = None;
        let query = self.query.read(cx).value().to_string();
        let scope = self.scopes[self.selected].0.clone();
        self.spk.update(cx, |spk, cx| {
            spk.send(Command::SearchMessages { query, scope }, None, cx)
        });
    }

    fn step_result(&mut self, backwards: bool, cx: &mut Context<Self>) -> bool {
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return false;
        };
        let results = &sidebar.search.results;
        if results.is_empty() || sidebar.search.loading {
            return false;
        }
        let index = self
            .selected_result
            .as_ref()
            .and_then(|selected| results.iter().position(|hit| hit.target == *selected))
            .map_or(if backwards { results.len() - 1 } else { 0 }, |index| {
                if backwards {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(results.len() - 1)
                }
            });
        self.selected_result = Some(results[index].target.clone());
        self.results_scroll.scroll_to_item(index);
        cx.notify();
        true
    }

    fn open_result(&self, target: MessageTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.spk.update(cx, |spk, cx| {
            spk.send(Command::OpenMessageTarget(target), None, cx)
        });
        window.close_dialog(cx);
    }

    fn result_row(
        &self,
        hit: &SearchHit,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let target = hit.target.clone();
        let channel = sidebar
            .channels
            .iter()
            .flatten()
            .find(|channel| channel.id == target.channel_id());
        let conversation = channel.map_or("Conversation", |channel| {
            if let Some(peer) = &channel.other_user_id {
                sidebar.message_author(peer)
            } else {
                channel.name.as_deref().unwrap_or("Text channel")
            }
        });
        let author = sidebar.message_author(&hit.user_id);
        let context = if hit.attachments > 0 {
            let suffix = if hit.attachments == 1 {
                "attachment"
            } else {
                "attachments"
            };
            format!("{author} · {conversation} · {} {suffix}", hit.attachments)
        } else {
            format!("{author} · {conversation}")
        };
        let copy_text =
            message_markdown(&hit.text, |id| sidebar.message_author(id), |_| false, None)
                .into_owned();
        let snippet = if hit.text.contains("||") {
            // A server snippet can start inside a spoiler.
            copy_text.chars().take(160).collect()
        } else {
            let snippet = hit.snippet.replace(['\u{1}', '\u{2}'], "");
            message_markdown(&snippet, |id| sidebar.message_author(id), |_| false, None)
                .into_owned()
        };
        let copy_target = target.clone();
        div()
            .id(SharedString::from(target.message_id().to_owned()))
            .flex()
            .flex_shrink_0()
            .gap_1()
            .items_center()
            .child(
                Button::new("open-result")
                    .ghost()
                    .flex_1()
                    .min_w_0()
                    .tab_stop(true)
                    .accessibility_label(format!("Jump to message from {author} in {conversation}"))
                    .selected(self.selected_result.as_ref() == Some(&target))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_result(target.clone(), window, cx);
                    }))
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                Label::new(context)
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(Label::new(snippet).truncate()),
                    ),
            )
            .child(
                tool_button(
                    "result-actions",
                    IconName::Ellipsis,
                    "Search result actions",
                )
                .dropdown_menu(move |menu, _, _| {
                    let text = copy_text.clone();
                    let target = copy_target.clone();
                    menu.item(PopupMenuItem::new("Copy text").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(plain_text(&text)));
                    }))
                    .item(PopupMenuItem::new("Copy link").on_click(move |_, _, cx| {
                        copy_message_link(&target, cx);
                    }))
                }),
            )
    }
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return div();
        };
        let search = &sidebar.search;
        let scope = self.scopes[self.selected].1;
        let selected = self.selected;
        let scopes = self
            .scopes
            .iter()
            .map(|(_, label)| *label)
            .collect::<Vec<_>>();
        let panel = cx.weak_entity();
        let status = if search.failed {
            Some("Could not search messages. Try again.")
        } else if search.loading {
            Some("Searching…")
        } else if search.searched && search.results.is_empty() {
            Some("No results.")
        } else if !search.searched {
            Some("Use from:, mentions:, in:, has:, before:, or after: to filter.")
        } else {
            None
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(Input::new(&self.query).aria_label("Search messages")),
                    )
                    .child(
                        Button::new("submit-search")
                            .label("Search")
                            .tab_stop(true)
                            .on_click(cx.listener(|this, _, _, cx| this.search(cx))),
                    ),
            )
            .child(
                Button::new("search-scope")
                    .small()
                    .tab_stop(true)
                    .ghost()
                    .label(scope)
                    .icon(IconName::ChevronDown)
                    .accessibility_label(format!("Search scope, {scope}"))
                    .dropdown_menu(move |mut menu, _, _| {
                        for (index, label) in scopes.iter().enumerate() {
                            let panel = panel.clone();
                            menu = menu.item(
                                PopupMenuItem::new(*label)
                                    .checked(index == selected)
                                    .on_click(move |_, _, cx| {
                                        let _ = panel.update(cx, |panel, cx| {
                                            panel.selected = index;
                                            panel.search(cx);
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .when_some(status, |panel, text| {
                panel.child(Label::new(text).text_color(cx.theme().muted_foreground))
            })
            .child(
                div()
                    .id("search-results")
                    .track_scroll(&self.results_scroll)
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .max_h(window.viewport_size().height * 0.44)
                    .overflow_y_scroll()
                    .children(
                        search
                            .results
                            .iter()
                            .map(|hit| self.result_row(hit, sidebar, cx)),
                    )
                    .when(search.next_cursor.is_some(), |list| {
                        list.child(
                            Button::new("more-results")
                                .small()
                                .tab_stop(true)
                                .label("Load more")
                                .disabled(search.loading)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.spk.update(cx, |spk, cx| {
                                        spk.send(Command::MoreSearchResults, None, cx)
                                    });
                                })),
                        )
                    }),
            )
    }
}

pub(super) fn copy_message_link(target: &MessageTarget, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(
        target.permalink(&SPK_BASE).into(),
    ));
}

pub(super) fn activate_chat_link(app: &WeakEntity<Spk>, link: Url, cx: &mut App) {
    if let Some(target) = MessageTarget::parse(&SPK_BASE, &link) {
        let _ = app.update(cx, |spk, cx| {
            spk.send(Command::OpenMessageTarget(target), None, cx)
        });
    } else {
        cx.open_url(link.as_str());
    }
}
