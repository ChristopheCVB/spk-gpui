use super::{voice::device_options, *};
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
}

impl Spk {
    pub(super) fn open_soundboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let initial = f32::from(sidebar.voice_intent.preferred.soundboard_volume.min(100));
        self.device_picker = None;
        let app = cx.entity();
        let panel = cx.new(|cx: &mut Context<SoundboardPanel>| {
            cx.observe(&app, |_, _, cx| cx.notify()).detach();
            let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search sounds"));
            cx.observe(&query, |_, _, cx| cx.notify()).detach();
            let volume = cx.new(|_| SliderState::new().max(100.0).default_value(initial));
            cx.subscribe(&volume, |panel, _, event, cx| {
                if let SliderEvent::Release(SliderValue::Single(value)) = event {
                    panel.send(Command::SetSoundboardVolume(*value as u8), cx);
                }
                cx.notify();
            })
            .detach();
            SoundboardPanel { app, query, volume }
        });
        self.send(Command::LoadSoundboard, None, cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .centered()
                .width(px(420.0))
                .title("Soundboard")
                .child(panel.clone())
        });
    }
}

impl SoundboardPanel {
    fn send(&self, command: Command, cx: &mut Context<Self>) {
        self.app.update(cx, |app, cx| app.send(command, None, cx));
    }

    fn clip_row(clip: &SoundboardClip, can_play: bool, cx: &Context<Self>) -> impl IntoElement {
        let play_id = clip.id.clone();
        let preview_id = clip.id.clone();
        let theme = cx.theme();
        let icon_path = format!("icons/{}.svg", clip.icon);
        let icon = if clip
            .icon
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && assets::AllAssets
                .load(&icon_path)
                .is_ok_and(|data| data.is_some())
        {
            Icon::new(IconName::Megaphone).path(icon_path)
        } else {
            Icon::new(IconName::Megaphone)
        };
        div()
            .id(SharedString::from(clip.id.clone()))
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("play")
                    .ghost()
                    .flex_1()
                    .min_w_0()
                    .tab_stop(true)
                    .disabled(!can_play)
                    .accessibility_label(format!("Play {} for everyone", clip.name))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        panel.send(Command::PlaySound(play_id.clone()), cx);
                    }))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(icon)
                            .child(Label::new(clip.name.clone()).truncate().flex_1())
                            .child(
                                Label::new(format!(
                                    "{:.1} s",
                                    f64::from(clip.duration_ms) / 1000.0
                                ))
                                .text_xs()
                                .text_color(theme.muted_foreground),
                            ),
                    ),
            )
            .child(
                tool_button(
                    "preview",
                    IconName::Headphones,
                    format!("Preview {} (only you)", clip.name),
                )
                .on_click(cx.listener(move |panel, _, _, cx| {
                    panel.send(Command::PreviewSound(preview_id.clone()), cx);
                })),
            )
    }
}

impl Render for SoundboardPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(Input::new(&self.query).aria_label("Search sounds"))
            .children(status.map(|message| Label::new(message).text_color(theme.muted_foreground)))
            .children(board.directory_error.map(|error| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new(error).text_color(theme.danger))
                    .child(Button::new("retry").label("Retry").tab_stop(true).on_click(
                        cx.listener(|panel, _, _, cx| panel.send(Command::LoadSoundboard, cx)),
                    ))
            }))
            .when(
                !board.loading && board.clips.is_none() && board.directory_error.is_none(),
                |panel| {
                    panel.child(
                        Button::new("load")
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
            .child(
                div()
                    .id("clips")
                    .flex()
                    .flex_col()
                    .max_h(px(180.0))
                    .overflow_y_scroll()
                    .children(
                        shown
                            .into_iter()
                            .map(|clip| Self::clip_row(clip, can_play, cx)),
                    ),
            )
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
                    .child(device_options(
                        sidebar,
                        DeviceKind::MediaOutput,
                        &self.app,
                        cx,
                    )),
            )
    }
}
