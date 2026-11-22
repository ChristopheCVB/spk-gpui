use super::*;

use gpui_kit::component::popover::Popover;

use crate::{api::GifItem, session::GifPickerView};

fn gif_button(id: &'static str, label: &'static str) -> Button {
    Button::new(id).ghost().small().tab_stop(true).label(label)
}

impl Spk {
    pub(in crate::ui) fn on_chat_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_input.read(cx).value().as_ref() == "/gif "
            && chat(&self.view).is_some_and(|chat| chat.can_send() && chat.outgoing.is_none())
        {
            self.chat_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.send(Command::ShowGifs(true), None, cx);
        }
        self.update_chat_draft(cx);
    }

    pub(super) fn update_gif_input(
        &self,
        previous: Option<&ChatView>,
        next: Option<&ChatView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = previous.and_then(|chat| chat.gifs.as_ref());
        let Some(next) = next.and_then(|chat| chat.gifs.as_ref()) else {
            return;
        };
        if previous.is_none_or(|view| view.showing_favorites != next.showing_favorites) {
            self.gif_input.update(cx, |input, cx| {
                let placeholder = if next.showing_favorites {
                    "Search favorites"
                } else {
                    "Search KLIPY"
                };
                input.set_placeholder(placeholder, window, cx);
                if previous.is_none() {
                    input.set_value("", window, cx);
                } else {
                    input.focus(window, cx);
                }
            });
        }
    }

    pub(super) fn gif_toggle(&self, chat: &ChatView, cx: &Context<Self>) -> impl IntoElement {
        let opened = chat.gifs.is_some();
        let disabled = !chat.can_send() || chat.outgoing.is_some();
        let trigger = tool_button("toggle-gif-picker", IconName::ImagePlay, "GIFs")
            .track_focus(&self.gif_button_focus)
            .toggled(opened)
            .accessibility_label(if opened {
                "Close GIF menu"
            } else {
                "Open GIF menu"
            })
            .disabled(disabled)
            .when(!disabled, |button| {
                // AccessKit dispatches Click as a pointer press, which the popover handles.
                button.on_click(cx.listener(move |app, event, _, cx| {
                    if !matches!(event, ClickEvent::Mouse(_)) {
                        app.send(Command::ShowGifs(!opened), None, cx);
                    }
                }))
            });
        let app = cx.weak_entity();
        Popover::new("gif-picker")
            .anchor(Anchor::BottomLeft)
            .w(px(320.))
            .p_2()
            .open(opened)
            .overlay_closable(true)
            .track_focus(&self.gif_input.focus_handle(cx))
            .trigger(trigger)
            .on_open_change(move |open, _, cx| {
                if !*open || !disabled {
                    let _ = app.update(cx, |app, cx| {
                        app.send(Command::ShowGifs(*open), None, cx);
                    });
                }
            })
            .children(chat.gifs.as_ref().map(|gifs| self.gif_picker(gifs, cx)))
    }

    fn gif_picker(&self, gifs: &GifPickerView, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let favorites = gifs.showing_favorites;
        let loading = if favorites {
            gifs.favorites_loading
        } else {
            gifs.browse_loading
        };
        let empty = if loading {
            Some("Loading GIFs…")
        } else if gifs.visible().next().is_some() {
            None
        } else if favorites && gifs.favorites_ready {
            Some(if gifs.favorites.is_empty() {
                "No favorites yet. Star a GIF to save it."
            } else {
                "No favorites match your search."
            })
        } else if !favorites && gifs.browse_error.is_none() {
            Some("No GIFs found")
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
                    .gap_1()
                    .p_1()
                    .rounded_md()
                    .bg(theme.accent)
                    .children(
                        [("Browse", false), ("Favorites", true)].map(|(label, show)| {
                            gif_button(label, label)
                                .flex_1()
                                .toggled(favorites == show)
                                .when(favorites == show, |button| button.primary())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(Command::ShowGifFavorites(show), None, cx);
                                }))
                        }),
                    ),
            )
            .child(Input::new(&self.gif_input).aria_label("Search GIFs"))
            .when_some(gifs.favorites_error, |panel, error| {
                panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Label::new(error))
                        .child(gif_button("retry-favorites", "Retry favorites").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.send(Command::RetryGifFavorites, None, cx)
                            }),
                        )),
                )
            })
            .when_some(gifs.mutation_error, |panel, error| {
                panel.child(Label::new(error))
            })
            .when_some(gifs.browse_error.filter(|_| !favorites), |panel, error| {
                panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Label::new(error))
                        .child(gif_button("retry-gifs", "Retry GIFs").on_click(
                            cx.listener(|this, _, _, cx| this.send(Command::RetryGifs, None, cx)),
                        )),
                )
            })
            .when_some(empty, |panel, status| {
                panel.child(Label::new(status).text_color(theme.muted_foreground))
            })
            .child(
                div()
                    .id(if favorites {
                        "favorite-gifs"
                    } else {
                        "browse-gifs"
                    })
                    .max_h(px(205.0))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(gifs.visible().map(|gif| self.gif_card(gif, gifs, cx))),
                    )
                    .when(
                        !favorites && gifs.has_next && gifs.browse_error.is_none(),
                        |list| {
                            list.child(
                                gif_button("more-gifs", "Load more")
                                    .disabled(gifs.browse_loading)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(Command::MoreGifs, None, cx)
                                    })),
                            )
                        },
                    ),
            )
            .child(
                div().flex().justify_center().child(
                    Label::new("Powered by KLIPY")
                        .text_xs()
                        .text_color(theme.muted_foreground),
                ),
            )
    }

    fn gif_card(
        &self,
        gif: &GifItem,
        gifs: &GifPickerView,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let title = gif_title(gif).to_owned();
        let select = gif.id.clone();
        let star = gif.id.clone();
        let favorite = gifs.is_favorite(&gif.id);
        let favorite_label = if favorite {
            format!("Remove {title} from favorites")
        } else {
            format!("Add {title} to favorites")
        };
        div()
            .id(SharedString::from(gif.id.clone()))
            .w(px(146.0))
            .relative()
            .child(
                Button::new("select-gif")
                    .ghost()
                    .w_full()
                    .h(px(106.0))
                    .small()
                    .tab_stop(true)
                    .accessibility_label(format!("Select {title}"))
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(div().w_full().h(px(80.0)).when_some(
                                gifs.image(&gif.preview),
                                |thumbnail, image| {
                                    thumbnail.child(
                                        img(image).size_full().object_fit(ObjectFit::Contain),
                                    )
                                },
                            ))
                            .child(Label::new(title).truncate()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.send(Command::PickGif(select.clone()), None, cx);
                        this.chat_input
                            .update(cx, |input, cx| input.focus(window, cx));
                    })),
            )
            .child(
                Button::new("star-gif")
                    .icon(IconName::Star)
                    .small()
                    .tab_stop(true)
                    .toggled(favorite)
                    .when(favorite, |button| button.bg(cx.theme().accent))
                    .absolute()
                    .top_0()
                    .right_0()
                    .accessibility_label(favorite_label)
                    .disabled(!gifs.favorites_ready || gifs.saving)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send(Command::ToggleGifFavorite(star.clone()), None, cx);
                    })),
            )
    }

    pub(super) fn staged_gif(&self, chat: &ChatView, cx: &Context<Self>) -> impl IntoElement {
        let channel = chat.channel_id.clone();
        div().when_some(chat.draft.gif.as_ref(), |row, gif| {
            row.flex()
                .items_center()
                .gap_2()
                .pb_2()
                .when_some(gif.image.clone(), |row, image| {
                    row.child(
                        img(image)
                            .w(px(80.0))
                            .h(px(50.0))
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .child(Label::new(format!("GIF · {}", gif_title(&gif.item))).truncate())
                .child(
                    gif_button("remove-staged-gif", "Remove GIF")
                        .disabled(chat.outgoing.is_some() || !chat.can_send())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(Command::RemoveDraftGif(channel.clone()), None, cx);
                        })),
                )
        })
    }

    pub(super) fn message_images(
        &self,
        chat: &ChatView,
        message_id: &str,
        rendered: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let links = image_preview_links(rendered);
        let multiple = links.len() > 1;
        div().children(links.into_iter().enumerate().map(|(index, target)| {
            let preview = chat
                .message_image
                .as_ref()
                .filter(|image| image.message_id == message_id && image.url == target.as_str());
            let loading = preview.is_some_and(|result| result.loading);
            let image = preview.and_then(|result| result.image.clone());
            let label = if loading {
                "Loading image…".to_owned()
            } else if multiple {
                let filename = target
                    .path_segments()
                    .and_then(|mut segments| segments.next_back())
                    .unwrap_or("image");
                format!("Preview {filename}")
            } else {
                "Preview image".to_owned()
            };
            let message_id = message_id.to_owned();
            let url = target.to_string();
            let copy_url = url.clone();
            div()
                .id(("message-image", index))
                .flex()
                .flex_col()
                .gap_1()
                .when_some(image.clone(), |row, image| {
                    row.child(
                        img(image)
                            .max_w_full()
                            .w(px(360.0))
                            .h(px(225.0))
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .when(image.is_none(), |row| {
                            row.child(
                                Button::new("preview-message-image")
                                    .ghost()
                                    .small()
                                    .tab_stop(true)
                                    .label(label)
                                    .disabled(loading || !chat.history_ready())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send(
                                            Command::PreviewMessageImage {
                                                message_id: message_id.clone(),
                                                url: url.clone(),
                                            },
                                            None,
                                            cx,
                                        );
                                    })),
                            )
                        })
                        .when(preview.is_some(), |row| {
                            row.child(
                                gif_button("open-image-link", "Open image link")
                                    .on_click(move |_, _, cx| cx.open_url(target.as_str())),
                            )
                            .child(
                                gif_button("copy-image-link", "Copy image link").on_click(
                                    move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_url.clone(),
                                        ));
                                    },
                                ),
                            )
                        }),
                )
                .when(preview.is_some() && !loading && image.is_none(), |row| {
                    row.child(
                        Label::new("Image preview unavailable")
                            .text_color(cx.theme().muted_foreground),
                    )
                })
        }))
    }
}

fn gif_title(gif: &GifItem) -> &str {
    if gif.title.is_empty() {
        "GIF"
    } else {
        &gif.title
    }
}
