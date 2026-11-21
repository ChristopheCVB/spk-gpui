use std::collections::HashMap;

use gpui_kit::component::popover::Popover;

use super::*;
use crate::{
    session::{Participant, VoiceView},
    voice::video::{CameraResolution, CameraStatus, ParticipantVideo, VideoSource},
};

#[derive(Default)]
pub(super) struct VideoStage {
    pub theater: Option<String>,
    pub fullscreen: bool,
    pub hide_preview: bool,
    pub frames: HashMap<(String, VideoSource), Arc<RenderImage>>,
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
        let report = call.and_then(|call| call.video.clone());
        let report = report.as_deref();
        let next = report
            .into_iter()
            .flat_map(|report| report.participants.iter())
            .flat_map(|(id, video)| {
                video.frames.iter().filter_map(move |(&source, frame)| {
                    Some(((id.clone(), source), frame.latest()?))
                })
            })
            .collect::<HashMap<_, _>>();
        for (key, image) in std::mem::replace(&mut self.video_stage.frames, next) {
            if self.video_stage.frames.get(&key) != Some(&image) {
                let _ = window.drop_image(image);
            }
        }
        if self.video_stage.theater.as_ref().is_some_and(|id| {
            report.is_none_or(|report| {
                !report.watching.contains(id)
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
                report
                    .and_then(|report| report.participants.get(id))
                    .is_none_or(|video| video.live.is_empty())
            })
        {
            self.exit_video_fullscreen(window);
        }
        if report.is_some_and(|report| {
            report
                .participants
                .values()
                .any(|video| !video.frames.is_empty())
        }) {
            window.request_animation_frame();
        }
    }

    fn exit_video_fullscreen(&mut self, window: &Window) {
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
        let report = sidebar.voice.as_ref().and_then(|call| call.video.as_ref());
        let status = report.map_or(CameraStatus::Off, |report| report.camera);
        let on = match status {
            CameraStatus::Failed(_) => false,
            _ => sidebar.voice_intent.camera.enabled,
        };
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
                        camera_options(sidebar, &app)
                    }),
            )
    }

    pub(super) fn video_image(&self, participant: &Participant<'_>, is_me: bool) -> Option<Img> {
        if is_me && self.video_stage.hide_preview {
            return None;
        }
        [VideoSource::Screen, VideoSource::Camera]
            .into_iter()
            .find_map(|source| {
                self.video_stage
                    .frames
                    .get(&(participant.id.to_owned(), source))
                    .map(|image| {
                        img(image.clone())
                            .size_full()
                            .object_fit(ObjectFit::Contain)
                    })
            })
    }

    pub(super) fn video_tile_controls(
        &self,
        participant: &Participant<'_>,
        call: &VoiceView,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let is_me = sidebar.profile.id.as_deref() == Some(participant.id);
        let report = call.video.as_ref();
        let live =
            participant_video(report.and_then(|report| report.participants.get(participant.id)))
                || participant
                    .video
                    .is_some_and(|media| !is_me && (media.cam || media.screen));
        let watching = sidebar.voice_intent.watching.contains(participant.id);
        let theater = self.video_stage.theater.as_deref() == Some(participant.id);
        let fullscreen = self.video_stage.fullscreen;
        let show = live && (is_me || watching);
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
            .when(live && !viewers.is_empty(), |row| {
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
            .when(!is_me && (live || watching), |row| {
                let label = if watching { "Stop watching" } else { "Watch" };
                row.child(
                    tool_button(
                        "watch",
                        if watching {
                            IconName::EyeOff
                        } else {
                            IconName::Eye
                        },
                        label,
                    )
                    .secondary()
                    .label(label)
                    .on_click(cx.listener(move |app, _, window, cx| {
                        if watching {
                            if app.video_stage.theater.as_ref() == Some(&watch_id) {
                                app.video_stage.theater = None;
                                app.exit_video_fullscreen(window);
                            }
                        } else {
                            app.video_stage.theater = Some(watch_id.clone());
                        }
                        app.send(Command::SetWatching(watch_id.clone(), !watching), None, cx);
                    })),
                )
            })
            .when(is_me && live, |row| {
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
            .when(show || theater, |row| {
                row.child(
                    tool_button(
                        "theater",
                        IconName::Monitor,
                        if theater {
                            "Exit theater mode"
                        } else {
                            "Theater mode"
                        },
                    )
                    .secondary()
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.video_stage.theater = (!theater).then(|| theater_id.clone());
                        app.exit_video_fullscreen(window);
                        cx.notify();
                    })),
                )
            })
            .when(show || fullscreen, |row| {
                row.child(
                    tool_button(
                        "fullscreen",
                        if fullscreen {
                            IconName::Minimize
                        } else {
                            IconName::Maximize
                        },
                        if fullscreen {
                            "Exit fullscreen"
                        } else {
                            "Fullscreen"
                        },
                    )
                    .secondary()
                    .on_click(cx.listener(move |app, _, window, cx| {
                        if fullscreen {
                            app.exit_video_fullscreen(window);
                        } else {
                            app.video_stage.theater = Some(id.clone());
                            app.video_stage.fullscreen = true;
                            if !window.is_fullscreen() {
                                window.toggle_fullscreen();
                            }
                        }
                        cx.notify();
                    })),
                )
            })
    }
}

fn participant_video(video: Option<&ParticipantVideo>) -> bool {
    video.is_some_and(|video| !video.live.is_empty())
}

fn camera_options(sidebar: &Sidebar, app: &Entity<Spk>) -> Div {
    let entity = app.downgrade();
    let default_camera = entity.clone();
    let resolution_entity = entity.clone();
    let choice = &sidebar.voice_intent.camera;
    let report = sidebar.voice.as_ref().and_then(|call| call.video.as_ref());
    let devices = report
        .map(|report| report.devices.as_slice())
        .unwrap_or_default();
    let chosen = choice.device.as_deref();
    let unavailable = chosen.is_some_and(|id| !devices.iter().any(|device| device.id == id));
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(Label::new("Camera").font_semibold())
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
            report.and_then(|report| match report.camera {
                CameraStatus::Live(width, height) => Some(format!("Live at {width} × {height}")),
                _ => None,
            }),
            |panel, resolution| panel.child(Label::new(resolution).text_xs()),
        )
}
