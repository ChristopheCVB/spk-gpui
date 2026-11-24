use super::{
    voice::{
        device_picker,
        media::{MediaResult, media_results},
    },
    *,
};
use crate::api::{Connection, SoundboardClip};
use gpui_kit::component::{
    Disableable,
    slider::{Slider, SliderEvent, SliderState, SliderValue},
    switch::Switch,
};

struct SoundboardPanel {
    app: Entity<Spk>,
    query: Entity<InputState>,
    volume: Entity<SliderState>,
    clip_scroll: ScrollHandle,
}

pub(super) fn soundboard_panel(
    app: Entity<Spk>,
    initial: u8,
    window: &mut Window,
    cx: &mut App,
) -> (AnyView, FocusHandle) {
    let panel = cx.new(|cx: &mut Context<SoundboardPanel>| {
        cx.observe(&app, |_, _, cx| cx.notify()).detach();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search sounds"));
        cx.observe(&query, |_, _, cx| cx.notify()).detach();
        let volume = cx.new(|_| {
            SliderState::new()
                .max(100.0)
                .default_value(f32::from(initial))
        });
        cx.subscribe(&volume, |panel, _, event, cx| {
            if let SliderEvent::Release(SliderValue::Single(value)) = event {
                panel.send(Command::SetSoundboardVolume(*value as u8), cx);
            }
            cx.notify();
        })
        .detach();
        SoundboardPanel {
            app,
            query,
            volume,
            clip_scroll: ScrollHandle::new(),
        }
    });
    let focus = panel.read(cx).query.focus_handle(cx);
    (panel.into(), focus)
}

impl SoundboardPanel {
    fn send(&self, command: Command, cx: &mut Context<Self>) {
        self.app.update(cx, |app, cx| app.send(command, None, cx));
    }

    fn clip_tile(clip: &SoundboardClip, can_play: bool, cx: &Context<Self>) -> impl IntoElement {
        let play_id = clip.id.clone();
        let preview_id = clip.id.clone();
        div()
            .relative()
            .min_w_0()
            .child(
                Button::new("play")
                    .w_full()
                    .h(px(64.0))
                    .px_1()
                    .tab_stop(true)
                    .disabled(!can_play)
                    .accessibility_label(format!("Play {} for everyone", clip.name))
                    .tooltip(clip.name.clone())
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        panel.send(Command::PlaySound(play_id.clone()), cx);
                    }))
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(clip_icon(&clip.icon))
                            .child(
                                Label::new(clip.name.clone())
                                    .text_xs()
                                    .truncate()
                                    .max_w_full(),
                            ),
                    ),
            )
            .child(
                div().absolute().top_0().right_0().occlude().child(
                    tool_button(
                        "preview",
                        IconName::Headphones,
                        format!("Preview {} (only you)", clip.name),
                    )
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        cx.stop_propagation();
                        panel.send(Command::PreviewSound(preview_id.clone()), cx);
                    })),
                ),
            )
    }
}

impl Render for SoundboardPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let View::SignedIn(sidebar) = &self.app.read(cx).view else {
            return div();
        };
        if !sidebar
            .voice
            .as_ref()
            .is_some_and(|voice| voice.is_participating())
        {
            return div().child(Label::new("Join a voice channel to use Soundboard."));
        }

        let board = &sidebar.soundboard;
        let enabled = sidebar.voice_intent.preferred.soundboard_enabled;
        let can_play =
            enabled && !board.cooling && matches!(sidebar.connection, Connection::Online);
        let query = self.query.read(cx).value().trim().to_lowercase();
        let shown = board
            .clips
            .iter()
            .flat_map(|clips| clips.iter())
            .filter(|clip| clip.name.to_lowercase().contains(&query))
            .collect::<Vec<_>>();
        let theme = cx.theme();
        let status = if board.loading {
            Some("Loading sounds…")
        } else if board.clips.as_ref().is_some_and(|clips| clips.is_empty()) {
            Some("No sounds yet.")
        } else if !query.is_empty() && shown.is_empty() && board.clips.is_some() {
            Some("No sounds match.")
        } else {
            None
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                Input::new(&self.query)
                    .prefix(themed_icon(IconName::Search, theme.muted_foreground, 16.0))
                    .aria_label("Search sounds"),
            )
            .children(status.map(|message| Label::new(message).text_color(theme.muted_foreground)))
            .children(board.directory_error.map(|error| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new(error).text_color(theme.danger))
                    .child(
                        control_button("retry")
                            .label("Retry")
                            .tab_stop(true)
                            .on_click(cx.listener(|panel, _, _, cx| {
                                panel.send(Command::LoadSoundboard, cx)
                            })),
                    )
            }))
            .when(
                !board.loading && board.clips.is_none() && board.directory_error.is_none(),
                |panel| {
                    panel.child(
                        control_button("load")
                            .label("Load sounds")
                            .tab_stop(true)
                            .on_click(cx.listener(|panel, _, _, cx| {
                                panel.send(Command::LoadSoundboard, cx)
                            })),
                    )
                },
            )
            .children(
                board
                    .playback_error
                    .map(|error| Label::new(error).text_color(theme.danger)),
            )
            .child(media_results(
                &self.clip_scroll,
                div()
                    .id("clips")
                    .grid()
                    .grid_cols(3)
                    .gap_2()
                    .max_h((window.viewport_size().height - px(295.0)).min(px(240.0)))
                    .children(
                        shown
                            .into_iter()
                            .enumerate()
                            .map(|(index, clip)| MediaResult {
                                id: clip.id.clone().into(),
                                index,
                                scroll: self.clip_scroll.clone(),
                                content: Self::clip_tile(clip, can_play, cx).into_any_element(),
                            }),
                    ),
            ))
            .child(
                Switch::new("soundboard-enabled")
                    .label("Hear soundboard sounds")
                    .accessibility_label("Hear soundboard sounds")
                    .checked(enabled)
                    .tab_stop(true)
                    .on_change(cx.listener(|panel, enabled: &bool, _, cx| {
                        panel.send(Command::SetSoundboardEnabled(*enabled), cx);
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new("Volume (only you)").text_xs())
                    .child(
                        Slider::new(&self.volume)
                            .flex_1()
                            .tab_stop(true)
                            .accessibility_label("Soundboard volume"),
                    )
                    .child(
                        Label::new(format!("{:.0}%", self.volume.read(cx).value().start()))
                            .text_xs(),
                    ),
            )
            .child(
                div()
                    .child(Label::new("Media Output").text_xs())
                    .child(device_picker(sidebar, DeviceKind::MediaOutput, &self.app)),
            )
    }
}

pub(super) fn clip_icon(name: &str) -> Icon {
    bundled_icon(name, Icon::new(IconName::Megaphone))
}
