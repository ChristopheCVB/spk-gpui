use super::*;
use crate::chat_markdown::image_preview_links;
use url::Url;

pub(super) type LinkImageKey = (MessageTarget, Url);

pub(super) enum LinkImage {
    Loading(Uuid),
    Ready(Arc<RenderImage>),
    Unavailable,
}

fn message_body<'a>(chat: &'a ChatView, target: &MessageTarget) -> Option<&'a str> {
    chat.messages
        .iter()
        .find(|message| message.id == target.message_id())
        .map(|message| message.body.as_str())
}

impl AttachmentUi {
    pub(super) fn sync_link_images(
        &mut self,
        previous: Option<&ChatView>,
        next: Option<&ChatView>,
        switched: bool,
        window: &mut Window,
    ) {
        self.link_images.retain(|(target, _), state| {
            let keep = !switched
                && previous.zip(next).is_some_and(|(previous, next)| {
                    message_body(previous, target)
                        .zip(message_body(next, target))
                        .is_some_and(|(old, new)| old == new)
                });
            if !keep && let LinkImage::Ready(image) = state {
                let _ = window.drop_image(image.clone());
            }
            keep
        });
    }

    fn start_link_image(&mut self, key: &LinkImageKey) -> Option<Uuid> {
        if self.inline_loads >= MAX_INLINE_LOADS || self.link_images.contains_key(key) {
            return None;
        }
        let operation = Uuid::new_v4();
        self.inline_loads += 1;
        self.link_images
            .insert(key.clone(), LinkImage::Loading(operation));
        Some(operation)
    }

    fn finish_link_image(
        &mut self,
        key: LinkImageKey,
        operation: Uuid,
        image: Option<Arc<RenderImage>>,
    ) -> bool {
        self.inline_loads -= 1;
        if let Some(state @ LinkImage::Loading(_)) = self.link_images.get_mut(&key)
            && matches!(state, LinkImage::Loading(id) if *id == operation)
        {
            *state = image.map_or(LinkImage::Unavailable, LinkImage::Ready);
            true
        } else {
            false
        }
    }
}

impl Spk {
    pub(in crate::ui::chat) fn message_images(
        &self,
        chat: &ChatView,
        target: Option<MessageTarget>,
        body: &str,
        rendered: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        div().children(
            target
                .into_iter()
                .flat_map(|target| {
                    image_preview_links(rendered)
                        .into_iter()
                        .map(move |url| (target.clone(), url))
                })
                .enumerate()
                .filter_map(|(index, (target, url))| {
                    let key = (target, url.clone());
                    match self.chat_attachments.link_images.get(&key) {
                        Some(LinkImage::Unavailable) => None,
                        Some(LinkImage::Ready(image)) => {
                            let image = image.clone();
                            let copy_url = url.to_string();
                            Some(
                                div()
                                    .id(("message-image", index))
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .gap_1()
                                    .child(
                                        Button::new("view-image")
                                            .ghost()
                                            .tab_stop(true)
                                            .accessibility_label("Enlarge image")
                                            .max_w_full()
                                            .h_auto()
                                            .p_0()
                                            .child(
                                                img(image.clone())
                                                    .max_w_full()
                                                    .w(px(360.0))
                                                    .h(px(225.0))
                                                    .object_fit(ObjectFit::Contain),
                                            )
                                            .on_click(cx.listener(move |_, _, window, cx| {
                                                let url = url.clone();
                                                open_chat_image(
                                                    "Image".into(),
                                                    image.clone(),
                                                    move |_, cx| {
                                                        cx.open_url(url.as_str());
                                                    },
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                    .child(
                                        Button::new("copy-image-link")
                                            .ghost()
                                            .small()
                                            .tab_stop(true)
                                            .label("Copy image link")
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_url.clone(),
                                                ));
                                            }),
                                    ),
                            )
                        }
                        state if chat.history_ready() => {
                            let row = div().child(
                                div()
                                    .id("image-status")
                                    .role(Role::Status)
                                    .child(Label::new("Loading image…").text_xs()),
                            );
                            let row = if state.is_none() {
                                let app = cx.weak_entity();
                                let body = body.to_owned();
                                when_visible(row, move |window, cx| {
                                    let _ = app.update(cx, |app, cx| {
                                        app.load_message_image(&key, &body, window, cx);
                                    });
                                })
                            } else {
                                row
                            };
                            Some(row.id(("message-image", index)))
                        }
                        _ => None,
                    }
                }),
        )
    }

    fn load_message_image(
        &mut self,
        key: &LinkImageKey,
        body: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (target, url) = key;
        if !chat(&self.view).is_some_and(|chat| {
            chat.channel_id == target.channel_id()
                && chat.history_ready()
                && message_body(chat, target) == Some(body)
        }) {
            return;
        }
        let Some(operation) = self.chat_attachments.start_link_image(key) else {
            return;
        };
        let (reply, result) = oneshot::channel();
        let commands = self.commands.clone();
        let command = Command::ReadMessageImage {
            target: target.clone(),
            url: url.clone(),
            body: body.to_owned(),
            reply,
        };
        let key = key.clone();
        cx.spawn_in(window, async move |app, cx| {
            let image = if commands.send(command).await.is_ok() {
                result.await.ok().flatten()
            } else {
                None
            };
            let _ = app.update_in(cx, |app, _, cx| {
                if app
                    .chat_attachments
                    .finish_link_image(key, operation, image)
                {
                    app.preserve_chat_position();
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
