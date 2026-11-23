use gpui_kit::{
    base::FocusTrapElement,
    component::{
        Disableable,
        menu::{DropdownMenu, PopupMenuItem},
    },
};
use url::Url;

use super::*;
use crate::{
    api::{MessageTarget, SearchHit, SearchScope, conversation_link},
    chat_markdown::{message_markdown, plain_text},
};

static SPK_BASE: LazyLock<Url> =
    LazyLock::new(|| Url::parse(env!("SPK_URL")).expect("validated SPK URL"));

pub(super) struct SearchPanel {
    spk: WeakEntity<Spk>,
    focus: FocusHandle,
    return_focus: FocusHandle,
    query: Entity<InputState>,
    scope: SearchScope,
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
        if self.video_stage.fullscreen {
            return;
        }
        if let Some(panel) = &self.search_panel {
            let command = panel.update(cx, |panel, cx| {
                panel.query.focus_handle(cx).focus(window, cx);
                if everything && panel.set_scope(SearchScope::Everything, cx) {
                    Some(panel.search_command(cx))
                } else {
                    None
                }
            });
            if let Some(command) = command {
                self.send(command, None, cx);
            }
            return;
        }
        let return_focus = window
            .focused(cx)
            .filter(|focus| focus != &self.workspace_focus)
            .unwrap_or_else(|| self.search_button_focus.clone());
        let scope = if everything {
            SearchScope::Everything
        } else {
            conversation_scope(sidebar).map_or(SearchScope::TextChannels, |(scope, _)| scope)
        };
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
                spk: spk.downgrade(),
                focus: cx.focus_handle().tab_stop(false),
                return_focus,
                query: panel_query,
                scope,
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

        query.focus_handle(cx).focus(window, cx);
        self.search_panel = Some(panel);
    }

    pub(super) fn sync_search_conversation(&self, sidebar: &Sidebar, cx: &mut Context<Self>) {
        if let Some(panel) = &self.search_panel
            && matches!(panel.read(cx).scope, SearchScope::Conversation(_))
        {
            let scope =
                conversation_scope(sidebar).map_or(SearchScope::TextChannels, |(scope, _)| scope);
            panel.update(cx, |panel, cx| {
                panel.set_scope(scope, cx);
            });
        }
    }

    pub(super) fn close_message_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.search_panel.take() {
            let panel = panel.read(cx);
            if panel.focus.contains_focused(window, cx) {
                let return_focus = if self.workspace_focus.contains(&panel.return_focus, window) {
                    panel.return_focus.clone()
                } else {
                    self.search_button_focus.clone()
                };
                return_focus.focus(window, cx);
            }
            cx.notify();
        }
    }

    pub(super) fn dismiss_search_on_escape(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_panel.is_some()
            && !self.video_stage.fullscreen
            && event.keystroke.key == "escape"
            && !event.keystroke.modifiers.modified()
        {
            self.close_message_search(window, cx);
            cx.stop_propagation();
        }
    }

    pub(super) fn focus_search_slideover(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = &self.search_panel
            && !self.video_stage.fullscreen
            && !window.has_active_dialog(cx)
        {
            let panel = panel.read(cx);
            if !panel.focus.contains_focused(window, cx) {
                panel.query.focus_handle(cx).focus(window, cx);
            }
        }
    }

    pub(super) fn search_surface(
        &self,
        wide: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut layer = div();
        let Some(panel) = &self.search_panel else {
            return layer;
        };
        let theme = cx.theme();
        let search = div()
            .id("search-surface")
            .role(if wide {
                Role::Complementary
            } else {
                Role::Dialog
            })
            .aria_label("Search")
            .h_full()
            .w((window.viewport_size().width * 0.3).clamp(px(304.0), px(384.0)))
            .flex_shrink_0()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.background)
            .occlude()
            .child(panel.clone());
        if wide {
            layer = layer.h_full().child(search);
        } else {
            layer = layer
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("search-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(black().opacity(0.35))
                        .occlude()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_message_search(window, cx);
                        })),
                )
                .child(
                    search
                        .absolute()
                        .right_0()
                        .top_0()
                        .max_w_full()
                        .focus_trap("search-surface", &cx.focus_handle().tab_stop(false)),
                );
        }
        layer
    }
}

fn conversation_scope(sidebar: &Sidebar) -> Option<(SearchScope, &'static str)> {
    sidebar
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
}

impl SearchPanel {
    fn set_scope(&mut self, scope: SearchScope, cx: &mut Context<Self>) -> bool {
        if self.scope == scope {
            return false;
        }
        self.scope = scope;
        self.selected_result = None;
        self.results_scroll = ScrollHandle::new();
        cx.notify();
        true
    }

    fn search_command(&self, cx: &App) -> Command {
        Command::SearchMessages {
            query: self.query.read(cx).value().to_string(),
            scope: self.scope.clone(),
        }
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        self.selected_result = None;
        let command = self.search_command(cx);
        let _ = self.spk.update(cx, |spk, cx| spk.send(command, None, cx));
    }

    fn step_result(&mut self, backwards: bool, cx: &mut Context<Self>) -> bool {
        let Some(spk) = self.spk.upgrade() else {
            return false;
        };
        let View::SignedIn(sidebar) = &spk.read(cx).view else {
            return false;
        };
        let results = &sidebar.search.results;
        if sidebar.search.scope.as_ref() != Some(&self.scope)
            || results.is_empty()
            || sidebar.search.loading
        {
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
        let spk = self.spk.clone();
        window.defer(cx, move |window, cx| {
            let _ = spk.update(cx, |spk, cx| {
                spk.close_message_search(window, cx);
                spk.send(Command::OpenMessageTarget(target), None, cx);
            });
        });
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(spk) = self.spk.upgrade() else {
            return div();
        };
        let View::SignedIn(sidebar) = &spk.read(cx).view else {
            return div();
        };
        let empty = Default::default();
        let search = if sidebar.search.scope.as_ref() == Some(&self.scope) {
            &sidebar.search
        } else {
            &empty
        };
        let scopes = conversation_scope(sidebar)
            .into_iter()
            .chain([
                (SearchScope::TextChannels, "All channels"),
                (SearchScope::DirectMessages, "DMs"),
                (SearchScope::Everything, "Everything"),
            ])
            .collect::<Vec<_>>();
        let selected = self.scope.clone();
        let scope = scopes
            .iter()
            .find(|(scope, _)| scope == &selected)
            .map_or("All channels", |(_, label)| *label);
        let panel = cx.weak_entity();
        let app = self.spk.clone();
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
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .p_3()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Label::new("Search").font_semibold())
                    .child(
                        tool_button("close-search", IconName::X, "Close search").on_click(
                            move |_, window, cx| {
                                let _ = app.update(cx, |app, cx| {
                                    app.close_message_search(window, cx);
                                });
                            },
                        ),
                    ),
            )
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
                        control_button("submit-search")
                            .label("Search")
                            .tab_stop(true)
                            .on_click(cx.listener(|this, _, _, cx| this.search(cx))),
                    ),
            )
            .child(
                control_button("search-scope")
                    .small()
                    .tab_stop(true)
                    .ghost()
                    .child(
                        control_content()
                            .child(Label::new(scope))
                            .child(Icon::new(IconName::ChevronDown).size(ACTION_ICON_SIZE)),
                    )
                    .accessibility_label(format!("Search scope, {scope}"))
                    .dropdown_menu(move |mut menu, _, _| {
                        for (scope, label) in &scopes {
                            let checked = scope == &selected;
                            let scope = scope.clone();
                            let panel = panel.clone();
                            menu = menu.item(PopupMenuItem::new(*label).checked(checked).on_click(
                                move |_, _, cx| {
                                    let _ = panel.update(cx, |panel, cx| {
                                        panel.set_scope(scope.clone(), cx);
                                        panel.search(cx);
                                    });
                                },
                            ));
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
                    .flex_1()
                    .overflow_y_scroll()
                    .children(
                        search
                            .results
                            .iter()
                            .map(|hit| self.result_row(hit, sidebar, cx)),
                    )
                    .when(search.next_cursor.is_some(), |list| {
                        list.child(
                            control_button("more-results")
                                .small()
                                .tab_stop(true)
                                .label("Load more")
                                .disabled(search.loading)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let _ = this.spk.update(cx, |spk, cx| {
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
    } else if let Some(channel) = conversation_link(&SPK_BASE, &link) {
        let _ = app.update(cx, |spk, cx| {
            spk.send(
                Command::OpenConversationTarget(channel.to_owned()),
                None,
                cx,
            )
        });
    } else {
        cx.open_url(link.as_str());
    }
}
