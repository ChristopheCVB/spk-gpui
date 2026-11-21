use std::{
    future::Future,
    hash::{Hash, Hasher},
};

use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};

use super::{
    chat::{mark_read_item, unread_badges, unread_label},
    *,
};

impl Spk {
    pub(super) fn person_row(
        &self,
        sidebar: &Sidebar,
        id: &str,
        name: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let profile_id = id.to_owned();
        let message_id = id.to_owned();
        let menu_id = id.to_owned();
        let conversation = sidebar.direct_message(id).map(|channel| channel.id.clone());
        let counts = conversation
            .as_deref()
            .map_or_else(Default::default, |id| sidebar.unread(id));
        let read_id = conversation.clone();
        let active = sidebar
            .chat
            .as_ref()
            .is_some_and(|chat| chat.direct_message_peer.as_deref() == Some(id));
        let app = cx.weak_entity();
        let theme = cx.theme();
        div()
            .id(SharedString::from(format!("person-{id}")))
            .flex()
            .items_center()
            .h_8()
            .gap_1()
            .child(
                Button::new("open-direct-message")
                    .ghost()
                    .small()
                    .tab_stop(true)
                    .selected(active)
                    .accessibility_label(unread_label(&format!("Direct message {name}"), counts))
                    .h_8()
                    .flex_1()
                    .min_w_0()
                    .when(active || counts.mentions > 0, |button| {
                        button.bg(theme.primary.opacity(0.10))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .child(profile_avatar(
                                Avatar::new().name(name.to_owned()).small(),
                                sidebar.avatar(Some(id)),
                                20.0,
                                false,
                                cx,
                            ))
                            .child(
                                Label::new(name.to_owned())
                                    .flex_1()
                                    .truncate()
                                    .when(counts.unread > 0, |label| label.font_bold()),
                            )
                            .child(unread_badges(counts, cx)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let command = match &conversation {
                            Some(id) => Command::OpenDirectMessage(id.clone()),
                            None => Command::StartDirectMessage(message_id.clone()),
                        };
                        this.open_direct_message(command, cx);
                    })),
            )
            .child(
                tool_button(
                    "inspect-person",
                    IconName::User,
                    format!("Profile for {name}"),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_person_profile(profile_id.clone(), window, cx);
                })),
            )
            .context_menu(move |menu, _, _| {
                let app = app.clone();
                let id = menu_id.clone();
                let message_app = app.clone();
                let message_id = id.clone();
                let copy_app = app.clone();
                let copy_id = id.clone();
                let read_app = app.clone();
                let mut menu = menu
                    .item(
                        PopupMenuItem::new("Profile").on_click(move |_, window, cx| {
                            let _ = app.update(cx, |app, cx| {
                                app.open_person_profile(id.clone(), window, cx)
                            });
                        }),
                    )
                    .item(PopupMenuItem::new("Send DM").on_click(move |_, _, cx| {
                        let _ = message_app.update(cx, |app, cx| {
                            app.open_direct_message(
                                Command::StartDirectMessage(message_id.clone()),
                                cx,
                            );
                        });
                    }))
                    .item(PopupMenuItem::new("Copy name").on_click(move |_, _, cx| {
                        if let Some(name) = copy_app.upgrade().and_then(|app| {
                            let View::SignedIn(sidebar) = &app.read(cx).view else {
                                return None;
                            };
                            sidebar.person(&copy_id)?.name().map(str::to_owned)
                        }) {
                            cx.write_to_clipboard(ClipboardItem::new_string(name));
                        }
                    }));
                if let Some(channel_id) = &read_id {
                    menu = menu.item(mark_read_item(channel_id.clone(), read_app));
                }
                menu
            })
    }

    fn open_direct_message(&mut self, command: Command, cx: &mut Context<Self>) {
        self.device_picker = None;
        self.send(command, None, cx);
    }

    fn open_person_profile(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        if sidebar.person(&id).is_none() {
            return;
        }
        let email = sidebar.is_admin().then(|| {
            let (reply, result) = tokio::sync::oneshot::channel();
            self.send(Command::InspectProfileEmail(id.clone(), reply), None, cx);
            result
        });
        let app = cx.entity();
        let profile = cx.new(|cx: &mut Context<PersonProfile>| {
            cx.observe(&app, |profile, _, cx| {
                if !matches!(&profile.app.read(cx).view, View::SignedIn(sidebar) if sidebar.is_admin()) {
                    profile.email = None;
                }
                cx.notify();
            })
            .detach();
            if let Some(email) = email {
                cx.spawn(async move |profile, cx| {
                    let Ok(Some(email)) = email.await else {
                        return;
                    };
                    let _ = profile.update(cx, |profile, cx| {
                        if matches!(&profile.app.read(cx).view, View::SignedIn(sidebar) if sidebar.is_admin()) {
                            profile.email = Some(email);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            PersonProfile { app, id, email: None }
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .centered()
                .width(px(380.0))
                .title("Profile")
                .child(profile.clone())
        });
    }
}

struct PersonProfile {
    app: Entity<Spk>,
    id: String,
    email: Option<String>,
}

impl Render for PersonProfile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let View::SignedIn(sidebar) = &self.app.read(cx).view else {
            return div();
        };
        let Some(person) = sidebar.person(&self.id) else {
            return div().child(Label::new("Profile unavailable."));
        };
        let name = person.name().unwrap_or("Unknown user");
        let role = if person.role == "admin" {
            "Admin"
        } else {
            "User"
        };
        let app = self.app.downgrade();
        let message_id = self.id.clone();
        div()
            .flex()
            .items_center()
            .gap_4()
            .child(profile_avatar(
                Avatar::new().name(name.to_owned()),
                sidebar.avatar(Some(&self.id)),
                64.0,
                false,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .gap_1()
                    .child(Label::new(name.to_owned()).text_lg().font_bold())
                    .child(Label::new(role).text_color(cx.theme().muted_foreground))
                    .children(
                        self.email
                            .as_ref()
                            .filter(|_| sidebar.is_admin())
                            .map(|email| {
                                Label::new(email.clone()).text_color(cx.theme().muted_foreground)
                            }),
                    )
                    .when(
                        sidebar.profile.id.as_deref() != Some(self.id.as_str()),
                        |profile| {
                            profile.child(
                                Button::new("send-profile-dm")
                                    .small()
                                    .tab_stop(true)
                                    .label("Send DM")
                                    .on_click(move |_, window, cx| {
                                        let _ = app.update(cx, |app, cx| {
                                            app.open_direct_message(
                                                Command::StartDirectMessage(message_id.clone()),
                                                cx,
                                            );
                                        });
                                        window.close_dialog(cx);
                                    }),
                            )
                        },
                    ),
            )
    }
}

#[derive(Clone)]
struct ThumbnailRequest {
    image: Arc<RenderImage>,
    size: u32,
}

impl Hash for ThumbnailRequest {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.image.id.hash(state);
        self.size.hash(state);
    }
}

struct AvatarThumbnail;

impl Asset for AvatarThumbnail {
    type Source = ThumbnailRequest;
    type Output = Arc<RenderImage>;

    fn load(
        request: Self::Source,
        _: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move { Arc::new(resize_avatar(&request.image, request.size)) }
    }
}

pub(super) fn avatar_thumbnail(
    photo: &AvatarPhoto,
    diameter: f32,
    window: &mut Window,
    cx: &mut App,
) -> Arc<RenderImage> {
    let pixels = ((diameter - 2.0) * window.scale_factor()).round().max(1.0) as u32;
    let image = photo.image.clone();
    let bounds = image.size(0);
    if pixels >= u32::from(bounds.width.min(bounds.height)) {
        return image;
    }
    photo.thumbnail_sizes.lock().unwrap().insert(pixels);
    let request = ThumbnailRequest {
        image,
        size: pixels,
    };
    window
        .use_asset::<AvatarThumbnail>(&request, cx)
        .unwrap_or(request.image)
}

pub(super) fn release_avatar(photo: &AvatarPhoto, window: &mut Window, cx: &mut App) {
    let _ = window.drop_image(photo.image.clone());
    for size in photo.thumbnail_sizes.lock().unwrap().drain() {
        let request = ThumbnailRequest {
            image: photo.image.clone(),
            size,
        };
        if let Some(thumb) = cx.fetch_asset::<AvatarThumbnail>(&request) {
            let _ = window.drop_image(thumb);
        }
        cx.remove_asset::<AvatarThumbnail>(&request);
    }
}

fn resize_avatar(source: &RenderImage, pixels: u32) -> RenderImage {
    use image::{
        Frame, RgbaImage,
        imageops::{FilterType, resize},
    };

    // GPUI's sprite sampler is bilinear and aliases a large source at sidebar avatar sizes.
    let frames = (0..source.frame_count())
        .map(|index| {
            let bounds = source.size(index);
            let (width, height) = (u32::from(bounds.width), u32::from(bounds.height));
            let bgra = source.as_bytes(index).expect("known avatar frame");
            let image =
                RgbaImage::from_raw(width, height, bgra.to_vec()).expect("decoded BGRA image");
            let scale = pixels as f32 / width.min(height) as f32;
            let thumb = resize(
                &image,
                (width as f32 * scale).round() as u32,
                (height as f32 * scale).round() as u32,
                FilterType::Triangle,
            );
            Frame::from_parts(thumb, 0, 0, source.delay(index))
        })
        .collect::<Vec<_>>();
    RenderImage::new(frames)
}

#[cfg(test)]
mod tests {
    use super::{RenderImage, resize_avatar};

    #[test]
    fn avatar_thumbnail_filters_detail_at_device_resolution_and_keeps_animation_timing() {
        let alternating = image::RgbaImage::from_fn(256, 256, |x, y| {
            let value = if (x + y) % 2 == 0 { 0 } else { 255 };
            image::Rgba([value, value, value, 255])
        });
        let durations = [
            image::Delay::from_numer_denom_ms(80, 1),
            image::Delay::from_numer_denom_ms(120, 1),
        ];
        let source = RenderImage::new(
            durations
                .map(|duration| image::Frame::from_parts(alternating.clone(), 0, 0, duration))
                .to_vec(),
        );

        let resized = resize_avatar(&source, 36);
        assert_eq!(resized.size(0).width.0, 36);
        assert_eq!(resized.size(0).height.0, 36);
        assert_eq!(resized.frame_count(), 2);
        assert_eq!(resized.delay(0), durations[0]);
        assert_eq!(resized.delay(1), durations[1]);
        assert!(
            resized
                .as_bytes(0)
                .unwrap()
                .chunks_exact(4)
                .all(|pixel| { (120..=135).contains(&pixel[0]) && pixel[3] == 255 })
        );
    }
}
