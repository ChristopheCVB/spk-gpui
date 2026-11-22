use std::collections::HashMap;

use gpui_kit::component::popover::Popover;

use super::*;
use crate::{
    session::{Participant, VoiceView},
    voice::{
        screen::{Quality, State as ScreenState},
        video::{CameraDevice, CameraResolution, CameraStatus, ParticipantVideo, VideoSource},
    },
};

#[derive(Default)]
pub(super) struct VideoStage {
    pub theater: Option<String>,
    pub fullscreen: bool,
    pub hide_preview: bool,
    pub hovered: Option<String>,
    pub frames: HashMap<(String, VideoSource), Arc<RenderImage>>,
}

#[derive(Clone, Copy)]
enum TileAction {
    Watch,
    Theater,
    Fullscreen,
}

#[derive(Clone, Copy)]
struct TileActions {
    live: bool,
    can_watch: bool,
    watching: bool,
    show: bool,
    theater: bool,
    fullscreen: bool,
    is_me: bool,
}

impl TileActions {
    fn for_participant(
        app: &Spk,
        participant: &Participant<'_>,
        call: &VoiceView,
        sidebar: &Sidebar,
    ) -> Self {
        let is_me = sidebar.profile.id.as_deref() == Some(participant.id);
        let report = call.video.as_ref();
        let live = (is_me
            && call
                .screen
                .as_ref()
                .is_some_and(|screen| screen.state.active()))
            || participant_video(report.and_then(|report| report.participants.get(participant.id)))
            || participant
                .video
                .is_some_and(|media| !is_me && (media.cam || media.screen));
        let watching = sidebar.voice_intent.watching.contains(participant.id);
        Self {
            live,
            can_watch: !is_me
                && (watching
                    || (live
                        && report.is_none_or(|report| {
                            !report.reconnecting && report.participants.contains_key(participant.id)
                        }))),
            watching,
            show: live && (is_me || watching),
            theater: app.video_stage.theater.as_deref() == Some(participant.id),
            fullscreen: app.video_stage.fullscreen,
            is_me,
        }
    }

    fn permits(&self, action: TileAction) -> bool {
        match action {
            TileAction::Watch => self.can_watch,
            TileAction::Theater => self.show || self.theater,
            TileAction::Fullscreen => self.show || self.fullscreen,
        }
    }
}

impl Spk {
    pub(super) fn prepare_video_stage(&mut self, window: &mut Window) {
        let call = match &self.view {
            View::SignedIn(sidebar) => sidebar.voice.as_ref().filter(|call| {
                call.is_participating()
                    && self
                        .selected_voice_channel
                        .as_ref()
                        .is_none_or(|id| *id == call.channel_id)
            }),
            _ => None,
        };
        let (watching, me) = match &self.view {
            View::SignedIn(sidebar) => (
                Some(&sidebar.voice_intent.watching),
                sidebar.profile.id.as_deref(),
            ),
            _ => (None, None),
        };
        let report = call.and_then(|call| call.video.clone());
        let report = report.as_deref();
        let mut next = report
            .into_iter()
            .flat_map(|report| report.participants.iter())
            .filter(|(id, _)| {
                me == Some(id.as_str()) || watching.is_some_and(|ids| ids.contains(*id))
            })
            .flat_map(|(id, video)| {
                video.frames.iter().filter_map(move |(&source, frame)| {
                    Some(((id.clone(), source), frame.latest()?))
                })
            })
            .collect::<HashMap<_, _>>();
        let screen = match &self.view {
            View::SignedIn(sidebar) => sidebar
                .profile
                .id
                .as_ref()
                .zip(call.and_then(|call| call.screen.as_ref())),
            _ => None,
        }
        .and_then(|(id, report)| match &report.state {
            ScreenState::Live { frames, .. } => Some((id.clone(), frames.clone())),
            _ => None,
        });
        if let Some((id, frames)) = &screen
            && let Some(frame) = frames.latest()
        {
            next.insert((id.clone(), VideoSource::Screen), frame);
        }
        for (key, image) in std::mem::replace(&mut self.video_stage.frames, next) {
            if self.video_stage.frames.get(&key) != Some(&image) {
                let _ = window.drop_image(image);
            }
        }
        if self.video_stage.theater.as_ref().is_some_and(|id| {
            report.is_none_or(|report| {
                !watching.is_some_and(|ids| ids.contains(id))
                    && screen.as_ref().is_none_or(|(me, _)| me != id)
                    && report
                        .participants
                        .get(id)
                        .is_none_or(|video| video.live.is_empty())
            })
        }) {
            self.video_stage.theater = None;
            self.exit_video_fullscreen(window);
        }
        if self.video_stage.fullscreen
            && self.video_stage.theater.as_ref().is_none_or(|id| {
                screen.as_ref().is_none_or(|(me, _)| me != id)
                    && report
                        .and_then(|report| report.participants.get(id))
                        .is_none_or(|video| video.live.is_empty())
            })
        {
            self.exit_video_fullscreen(window);
        }
        if screen.is_some()
            || report.is_some_and(|report| {
                report
                    .participants
                    .values()
                    .any(|video| !video.frames.is_empty())
            })
        {
            window.request_animation_frame();
        }
    }

    pub(super) fn exit_video_fullscreen(&mut self, window: &Window) {
        if self.video_stage.fullscreen && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.video_stage.fullscreen = false;
    }

    pub(super) fn dismiss_video_fullscreen(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.video_stage.fullscreen && event.keystroke.key == "escape" {
            self.exit_video_fullscreen(window);
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn camera_controls(
        &self,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let on = sidebar.camera_publishing();
        let label = if on { "Stop Camera" } else { "Start Camera" };
        let app = cx.weak_entity();
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                tool_button(
                    "toggle-camera",
                    if on {
                        IconName::Video
                    } else {
                        IconName::VideoOff
                    },
                    label,
                )
                .label(label)
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.send(Command::SetCameraEnabled(!on), None, cx);
                })),
            )
            .child(
                Popover::new("camera-options")
                    .anchor(Anchor::BottomRight)
                    .w(px(280.0))
                    .trigger(tool_button(
                        "camera-settings",
                        IconName::ChevronUp,
                        "Camera settings",
                    ))
                    .content(move |_, _, cx| {
                        let Some(app) = app.upgrade() else {
                            return div();
                        };
                        let View::SignedIn(sidebar) = &app.read(cx).view else {
                            return div();
                        };
                        let devices = sidebar
                            .voice
                            .as_ref()
                            .and_then(|call| call.video.as_ref())
                            .map_or(&[][..], |report| report.devices.as_slice());
                        camera_options(sidebar, &app, devices)
                    }),
            )
    }

    pub(super) fn screen_button(&self, call: Option<&VoiceView>, cx: &Context<Self>) -> Button {
        let report = call.and_then(|call| call.screen.as_ref());
        let active = report.filter(|report| !matches!(report.state, ScreenState::Ended(_)));
        let attempt = active.map(|report| report.attempt);
        let label = match active.map(|report| &report.state) {
            Some(ScreenState::Live { .. }) => "Stop Screen Share",
            Some(_) => "Cancel Screen Share",
            None => "Share Screen",
        };
        tool_button("screen-share", IconName::Monitor, label)
            .selected(active.is_some())
            .label(label)
            .on_click(cx.listener(move |app, _, _, cx| {
                let command = attempt.map_or(Command::OpenScreenShare, Command::StopScreenShare);
                app.send(command, None, cx);
            }))
    }

    pub(super) fn screen_options(
        &self,
        call: &VoiceView,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let report = call.screen.as_ref()?;
        let attempt = report.attempt;
        let selected = sidebar.voice_intent.screen.quality;
        let panel = div().flex().flex_col().items_center().gap_2().px_4().pb_2();
        Some(match &report.state {
            ScreenState::Picking => panel.child(Label::new("Choose a display or window…")),
            ScreenState::Choices(choices) => panel.child(
                div()
                    .id("screen-sources")
                    .max_h(px(140.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_1()
                    .children(choices.iter().map(|choice| {
                        let id = choice.id.clone();
                        Button::new(SharedString::from(format!("screen-source-{id}")))
                            .small()
                            .secondary()
                            .tab_stop(true)
                            .label(choice.label.clone())
                            .on_click(cx.listener(move |app, _, _window, cx| {
                                #[cfg(windows)]
                                if id == crate::voice::screen::SYSTEM_PICKER {
                                    app.pick_windows_screen(attempt, _window, cx);
                                    return;
                                }
                                app.send(
                                    Command::ChooseScreenSource(attempt, id.clone()),
                                    None,
                                    cx,
                                );
                            }))
                    })),
            ),
            ScreenState::Confirm { label, audio } => {
                let enabled = sidebar.voice_intent.preferred.screen_audio;
                let audio_label = match (audio, enabled) {
                    (Some(scope), true) => scope.label(),
                    (None, true) => "Video only. Audio is unavailable for this source.",
                    _ => "Video only",
                };
                let confirmed_audio = enabled && audio.is_some();
                panel
                    .child(Label::new(label.clone()).font_semibold())
                    .child(Label::new(audio_label).text_xs())
                    .child(
                        Button::new("include-screen-audio")
                            .small()
                            .secondary()
                            .tab_stop(true)
                            .label(if enabled {
                                "Share audio on"
                            } else {
                                "Share audio off"
                            })
                            .selected(enabled)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.send(Command::SetScreenAudio(!enabled), None, cx);
                            })),
                    )
                    .child(Self::screen_quality_options(selected, cx))
                    .child(
                        Button::new("start-screen-share")
                            .small()
                            .primary()
                            .tab_stop(true)
                            .label("Start Screen Share")
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.send(
                                    Command::StartScreenShare(attempt, confirmed_audio),
                                    None,
                                    cx,
                                );
                            })),
                    )
            }
            ScreenState::Starting => panel.child(Label::new("Starting Screen Share…")),
            ScreenState::Live {
                width,
                height,
                fps,
                audio,
                audio_unavailable,
                quality,
                quality_failed,
                ..
            } => panel
                .child(Self::screen_quality_options(selected, cx))
                .child(
                    Label::new(format!("Sharing at {width} × {height} · up to {fps} fps"))
                        .text_xs(),
                )
                .when(selected != *quality, |panel| {
                    panel.child(Label::new("Changing Screen Share quality…").text_xs())
                })
                .when(selected == *quality && quality_failed.is_some(), |panel| {
                    panel.child(Label::new("Could not change Screen Share quality.").text_xs())
                })
                .child(
                    Label::new(if *audio_unavailable {
                        "Video only. Screen Share audio is unavailable."
                    } else {
                        audio.map_or("Video only", |scope| scope.label())
                    })
                    .text_xs(),
                ),
            ScreenState::Ended(message) => panel.child(Label::new(*message)),
        })
    }

    fn screen_quality_options(selected: Quality, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .justify_center()
            .gap_1()
            .children(Quality::HEIGHTS.into_iter().map(|height| {
                let label = SharedString::from(format!("{height}p"));
                Button::new(label.clone())
                    .small()
                    .ghost()
                    .tab_stop(true)
                    .label(label)
                    .selected(selected.height == height)
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.send(Command::SetScreenHeight(height), None, cx);
                    }))
            }))
            .children(Quality::FPS.into_iter().map(|fps| {
                let label = SharedString::from(format!("{fps} fps"));
                Button::new(label.clone())
                    .small()
                    .ghost()
                    .tab_stop(true)
                    .label(label)
                    .selected(selected.fps == fps)
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.send(Command::SetScreenFps(fps), None, cx);
                    }))
            }))
    }

    #[cfg(windows)]
    fn pick_windows_screen(&mut self, attempt: u64, window: &Window, cx: &mut Context<Self>) {
        use crate::voice::screen::{CaptureError, pick_system_source};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let hwnd = HasWindowHandle::window_handle(window)
            .ok()
            .and_then(|window| match window.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                _ => None,
            });
        cx.spawn(async move |app, cx| {
            let result = match hwnd {
                Some(hwnd) => pick_system_source(hwnd).await,
                None => Err(CaptureError::Unavailable),
            };
            let _ = app.update(cx, |app, cx| {
                app.send(
                    Command::ChooseWindowsScreenSource(attempt, result),
                    None,
                    cx,
                );
            });
        })
        .detach();
    }

    pub(super) fn video_images(
        &self,
        participant: &Participant<'_>,
        is_me: bool,
    ) -> (Option<Img>, Option<Img>) {
        if is_me && self.video_stage.hide_preview {
            return (None, None);
        }
        let image_for = |source| {
            self.video_stage
                .frames
                .get(&(participant.id.to_owned(), source))
                .map(|image| {
                    img(image.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain)
                })
        };
        let screen = image_for(VideoSource::Screen);
        let camera = image_for(VideoSource::Camera);
        if screen.is_some() {
            (screen, camera)
        } else {
            (camera, None)
        }
    }

    pub(super) fn hovered_video_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.video_stage.hovered.clone() {
            self.video_tile_key(&id, event, window, cx);
        }
    }

    pub(super) fn video_tile_key(
        &mut self,
        id: &str,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.is_held
            || event.keystroke.modifiers.modified()
            || window.has_focused_input(cx)
            || window.has_active_dialog(cx)
        {
            return;
        }
        let action = match event.keystroke.key.as_str() {
            "w" => TileAction::Watch,
            "t" => TileAction::Theater,
            "f" => TileAction::Fullscreen,
            _ => return,
        };
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let Some(call) = sidebar
            .voice
            .as_ref()
            .filter(|call| call.is_participating())
        else {
            return;
        };
        if sidebar.chat.is_some()
            || self.selected_voice_channel.as_deref().or_else(|| {
                super::workspace::voice_channels(sidebar)
                    .next()
                    .map(|channel| channel.id.as_str())
            }) != Some(call.channel_id.as_str())
        {
            return;
        }
        let participants = sidebar.participants(&call.channel_id);
        let own_tile = Participant {
            id,
            name: sidebar.profile.name(),
            flags: Default::default(),
            video: None,
        };
        let Some(participant) = participants
            .iter()
            .find(|participant| participant.id == id)
            .or_else(|| (sidebar.profile.id.as_deref() == Some(id)).then_some(&own_tile))
        else {
            return;
        };
        let available = TileActions::for_participant(self, participant, call, sidebar);
        if available.permits(action) {
            self.apply_tile_action(id, action, &available, window, cx);
            cx.stop_propagation();
            window.prevent_default();
        }
    }

    fn apply_tile_action(
        &mut self,
        id: &str,
        action: TileAction,
        available: &TileActions,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            TileAction::Watch => {
                if available.watching {
                    if available.theater {
                        self.video_stage.theater = None;
                        self.exit_video_fullscreen(window);
                    }
                } else {
                    self.video_stage.theater = Some(id.to_owned());
                }
                self.send(
                    Command::SetWatching(id.to_owned(), !available.watching),
                    None,
                    cx,
                );
            }
            TileAction::Theater => {
                self.video_stage.theater = (!available.theater).then(|| id.to_owned());
                self.exit_video_fullscreen(window);
                cx.notify();
            }
            TileAction::Fullscreen => {
                if available.fullscreen {
                    self.exit_video_fullscreen(window);
                } else {
                    self.video_stage.theater = Some(id.to_owned());
                    self.video_stage.fullscreen = true;
                    if !window.is_fullscreen() {
                        window.toggle_fullscreen();
                    }
                }
                cx.notify();
            }
        }
    }

    pub(super) fn video_tile_controls(
        &self,
        participant: &Participant<'_>,
        call: &VoiceView,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let available = TileActions::for_participant(self, participant, call, sidebar);
        let id = participant.id.to_owned();
        let watch_id = id.clone();
        let theater_id = id.clone();
        let viewers = sidebar.viewers(&call.channel_id, participant.id);
        div()
            .absolute()
            .bottom_2()
            .left_2()
            .right_2()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap_1()
            .when(available.live && !viewers.is_empty(), |row| {
                row.child(
                    Label::new(format!("Watching: {}", viewers.join(", ")))
                        .flex_1()
                        .px_2()
                        .rounded_md()
                        .bg(cx.theme().background)
                        .text_xs()
                        .truncate(),
                )
            })
            .when(available.permits(TileAction::Watch), |row| {
                let label = if available.watching {
                    "Stop watching"
                } else {
                    "Watch"
                };
                row.child(
                    tool_button(
                        "watch",
                        if available.watching {
                            IconName::EyeOff
                        } else {
                            IconName::Eye
                        },
                        label,
                    )
                    .secondary()
                    .label(label)
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.apply_tile_action(&watch_id, TileAction::Watch, &available, window, cx);
                    })),
                )
            })
            .when(available.is_me && available.live, |row| {
                let label = if self.video_stage.hide_preview {
                    "Show preview"
                } else {
                    "Hide preview"
                };
                row.child(
                    tool_button("camera-preview", IconName::Eye, label)
                        .secondary()
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.video_stage.hide_preview = !app.video_stage.hide_preview;
                            cx.notify();
                        })),
                )
            })
            .when(available.permits(TileAction::Theater), |row| {
                row.child(
                    tool_button(
                        "theater",
                        IconName::Monitor,
                        if available.theater {
                            "Exit theater mode"
                        } else {
                            "Theater mode"
                        },
                    )
                    .secondary()
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.apply_tile_action(
                            &theater_id,
                            TileAction::Theater,
                            &available,
                            window,
                            cx,
                        );
                    })),
                )
            })
            .when(available.permits(TileAction::Fullscreen), |row| {
                row.child(
                    tool_button(
                        "fullscreen",
                        if available.fullscreen {
                            IconName::Minimize
                        } else {
                            IconName::Maximize
                        },
                        if available.fullscreen {
                            "Exit fullscreen"
                        } else {
                            "Fullscreen"
                        },
                    )
                    .secondary()
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.apply_tile_action(&id, TileAction::Fullscreen, &available, window, cx);
                    })),
                )
            })
    }
}

fn participant_video(video: Option<&ParticipantVideo>) -> bool {
    video.is_some_and(|video| !video.live.is_empty())
}

pub(super) fn camera_options(
    sidebar: &Sidebar,
    app: &Entity<Spk>,
    devices: &[CameraDevice],
) -> Div {
    let entity = app.downgrade();
    let default_camera = entity.clone();
    let resolution_entity = entity.clone();
    let choice = &sidebar.voice_intent.camera;
    let chosen = choice.device.as_deref();
    let unavailable = chosen.is_some_and(|id| !devices.iter().any(|device| device.id == id));
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id("camera-devices")
                .max_h(px(180.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .child(
                    Button::new("default-camera")
                        .small()
                        .ghost()
                        .tab_stop(true)
                        .label("System default")
                        .selected(chosen.is_none())
                        .on_click(move |_, _, cx| {
                            let _ = default_camera.update(cx, |app, cx| {
                                app.send(Command::ChooseCamera(None), None, cx)
                            });
                        }),
                )
                .children(devices.iter().map(|device| {
                    let id = device.id.clone();
                    let entity = entity.clone();
                    Button::new(SharedString::from(format!("camera-{id}")))
                        .small()
                        .ghost()
                        .tab_stop(true)
                        .label(device.name.clone())
                        .selected(chosen == Some(&id))
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |app, cx| {
                                app.send(Command::ChooseCamera(Some(id.clone())), None, cx)
                            });
                        })
                }))
                .when(unavailable, |list| {
                    list.child("Selected Camera unavailable")
                })
                .when(devices.is_empty(), |list| list.child("No Cameras detected")),
        )
        .child(Label::new("Resolution").font_semibold())
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(CameraResolution::ALL.into_iter().map(|resolution| {
                    let entity = resolution_entity.clone();
                    Button::new(resolution.label())
                        .small()
                        .ghost()
                        .tab_stop(true)
                        .label(resolution.label())
                        .selected(choice.resolution == resolution)
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |app, cx| {
                                app.send(Command::ChooseCameraResolution(resolution), None, cx)
                            });
                        })
                })),
        )
        .when_some(
            match sidebar.camera_status() {
                CameraStatus::Live(width, height) => Some(format!("Live at {width} × {height}")),
                _ => None,
            },
            |panel, resolution| panel.child(Label::new(resolution).text_xs()),
        )
}
