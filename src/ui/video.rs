use std::{cell::RefCell, collections::HashMap};

use gpui_kit::component::{
    IndexPath,
    popover::Popover,
    select::{SearchableVec, SelectEvent, SelectItem},
    slider::Slider,
    switch::Switch,
    tooltip::Tooltip,
};

use super::{
    voice::{VoiceButtonGroup, quick_level, voice_button, voice_shortcut_tooltip},
    *,
};
use crate::{
    session::{Participant, VoiceView},
    voice::{
        ParticipantVolumeChange,
        screen::{Quality, State as ScreenState},
        video::{CameraDevice, CameraResolution, CameraStatus, ParticipantVideo, VideoSource},
    },
};

#[derive(Default)]
pub(super) struct VideoStage {
    pub theater: Option<String>,
    pub fullscreen: bool,
    pub participants_hidden: bool,
    pub participant_scroll: ScrollHandle,
    pub grid_scroll: ScrollHandle,
    pub grid_focus: RefCell<Option<FocusHandle>>,
    pub tile_focus: HashMap<String, FocusHandle>,
    strip_focus: RefCell<Option<FocusHandle>>,
    pub hide_preview: bool,
    pub hovered: Option<String>,
    pub pointer_in_window: bool,
    focused: Option<FocusHandle>,
    pub keyboard_focus: bool,
    pub menu_for: Option<String>,
    pub screen_volume_for: Option<String>,
    pub frames: HashMap<(String, VideoSource), Arc<RenderImage>>,
    pub sidebar_watch: Option<SidebarWatch>,
}

pub(super) struct SidebarWatch {
    pub channel_id: String,
    pub participant_id: String,
}

impl VideoStage {
    pub(super) fn pointer_over_window(&self, window: &Window) -> bool {
        // Windows signals window hover changes without dispatching MouseExitEvent.
        self.pointer_in_window && (!cfg!(windows) || window.is_window_hovered())
    }
}

#[derive(Clone, Copy)]
enum TileAction {
    Watch,
    Theater,
    Fullscreen,
}

#[derive(Clone, Copy)]
pub(super) struct TileActions {
    live: bool,
    can_watch: bool,
    watching: bool,
    show: bool,
    theater: bool,
    fullscreen: bool,
    is_me: bool,
    pub sharing_screen: bool,
    pub watched_screen: bool,
    pub screen_quality: Option<Quality>,
    screen_volume: bool,
}

pub(super) struct TileControls {
    pub actions: TileActions,
    pub revealed: bool,
    pub compact: bool,
    pub screen_overlay: bool,
}

impl TileActions {
    pub(super) fn for_participant(
        app: &Spk,
        participant: &Participant<'_>,
        call: &VoiceView,
        sidebar: &Sidebar,
    ) -> Self {
        let is_me = sidebar.profile.id.as_deref() == Some(participant.id);
        let report = call.video.as_ref();
        let video = report.and_then(|report| report.participants.get(participant.id));
        let local_screen = is_me
            && call
                .screen
                .as_ref()
                .is_some_and(|screen| screen.state.active());
        let feeds = sidebar.video_feeds(&call.channel_id, participant.id);
        let sharing_screen = if is_me { local_screen } else { feeds.screen };
        let live = if is_me {
            local_screen || participant_video(video)
        } else {
            feeds.is_live()
        };
        let watching = sidebar.voice_intent.watching.contains(participant.id);
        let watched_screen = watching && sharing_screen;
        Self {
            live,
            can_watch: !is_me
                && live
                && (watching || sidebar.can_watch_video(&call.channel_id, participant.id)),
            watching,
            show: live && (is_me || watching),
            theater: app.video_stage.theater.as_deref() == Some(participant.id),
            fullscreen: app.video_stage.fullscreen,
            is_me,
            sharing_screen,
            watched_screen,
            screen_quality: video
                .filter(|_| watched_screen)
                .and_then(|video| video.screen_quality),
            screen_volume: watched_screen
                && video.is_some_and(|video| video.has_unmuted_screen_audio),
        }
    }

    fn permits(&self, action: TileAction) -> bool {
        match action {
            TileAction::Watch => self.can_watch,
            TileAction::Theater => true,
            TileAction::Fullscreen => self.show || self.fullscreen,
        }
    }
}

impl Spk {
    pub(super) fn advance_sidebar_watch(&mut self, sidebar: &Sidebar, cx: &mut Context<Self>) {
        let Some(watch) = self.video_stage.sidebar_watch.as_ref() else {
            return;
        };
        let feeds = sidebar.video_feeds(&watch.channel_id, &watch.participant_id);
        if (!feeds.is_live() && !sidebar.advertises_video(&watch.channel_id, &watch.participant_id))
            || self.selected_voice_channel.as_deref() != Some(&watch.channel_id)
        {
            self.video_stage.sidebar_watch = None;
            return;
        }
        let local = sidebar.profile.id.as_deref() == Some(&watch.participant_id);
        let watching = sidebar
            .voice_intent
            .watching
            .contains(&watch.participant_id);
        if !sidebar.voice.as_ref().is_some_and(|voice| {
            voice.channel_id == watch.channel_id
                && voice.is_participating()
                && (local
                    || (watching && feeds.is_live())
                    || sidebar.can_watch_video(&watch.channel_id, &watch.participant_id))
        }) {
            return;
        }
        let watch = self
            .video_stage
            .sidebar_watch
            .take()
            .expect("watch request");
        self.video_stage.theater = Some(watch.participant_id.clone());
        self.video_stage.participants_hidden = false;
        if !local && !watching {
            self.send(Command::SetWatching(watch.participant_id, true), None, cx);
        }
    }

    pub(super) fn prepare_video_stage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = window.focused(cx);
        if self.video_stage.focused != focused {
            self.video_stage.keyboard_focus = window.last_input_was_keyboard();
            self.video_stage.focused = focused;
        }
        let call = match &self.view {
            View::SignedIn(sidebar) => sidebar.voice.as_ref().filter(|call| {
                call.is_participating()
                    && self
                        .selected_voice_destination(sidebar)
                        .is_some_and(|channel| channel.id == call.channel_id)
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
        let participants = match (&self.view, call) {
            (View::SignedIn(sidebar), Some(call)) => joined_participants(sidebar, call),
            _ => Vec::new(),
        };
        self.video_stage
            .tile_focus
            .retain(|id, _| participants.iter().any(|person| person.id == id));
        if let Some(id) = &self.video_stage.screen_volume_for {
            let can_adjust = match (&self.view, call) {
                (View::SignedIn(sidebar), Some(call)) => participants
                    .iter()
                    .find(|person| person.id == id)
                    .is_some_and(|person| {
                        TileActions::for_participant(self, person, call, sidebar).screen_volume
                    }),
                _ => false,
            };
            if !can_adjust {
                self.video_stage.screen_volume_for = None;
            }
        }
        for person in participants {
            if !self.video_stage.tile_focus.contains_key(person.id) {
                self.video_stage
                    .tile_focus
                    .insert(person.id.to_owned(), cx.focus_handle().tab_stop(true));
            }
        }
        if self
            .video_stage
            .menu_for
            .as_ref()
            .is_some_and(|id| !self.video_stage.tile_focus.contains_key(id))
        {
            self.video_stage.menu_for = None;
        }
        if self
            .video_stage
            .theater
            .as_ref()
            .is_some_and(|id| !self.video_stage.tile_focus.contains_key(id))
        {
            self.video_stage.theater = None;
            self.exit_video_fullscreen(window);
        }
        if self.video_stage.theater.is_none() {
            self.video_stage.participants_hidden = false;
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

    pub(super) fn scroll_to_focused_participant(
        &self,
        ids: impl Iterator<Item = impl AsRef<str>>,
        window: &Window,
        cx: &App,
    ) {
        let focused = window.focused(cx);
        if focused == *self.video_stage.strip_focus.borrow() {
            return;
        }
        self.video_stage.strip_focus.replace(focused);
        if let Some(index) = ids
            .into_iter()
            .position(|id| self.video_stage.tile_focus[id.as_ref()].contains_focused(window, cx))
        {
            self.video_stage.participant_scroll.scroll_to_item(index);
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
        location: VoiceControls,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let on = sidebar.camera_publishing();
        let label = if on {
            "Turn camera off"
        } else {
            "Turn camera on"
        };
        let app = cx.weak_entity();
        VoiceButtonGroup::new(
            "camera-controls",
            voice_shortcut_tooltip(
                voice_button(
                    "toggle-camera",
                    if on {
                        IconName::Video
                    } else {
                        IconName::VideoOff
                    },
                    label,
                    location,
                )
                .selected(on)
                .toggled(on)
                .on_click(cx.listener(move |app, _, _, cx| {
                    if let View::SignedIn(sidebar) = &app.view {
                        app.send(
                            Command::SetCameraEnabled(!sidebar.camera_publishing()),
                            None,
                            cx,
                        );
                    }
                })),
                label,
                super::workspace_keys::media_shortcut_keys("E", cx)
                    .map(str::to_owned)
                    .to_vec(),
            ),
            "Camera options",
            floating_popover("camera-options", cx)
                .w(px(280.0))
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

    pub(super) fn screen_controls(
        &self,
        call: &VoiceView,
        location: VoiceControls,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (label, command) = screen_share_action(call);
        let active = !matches!(command, Command::OpenScreenShare);
        let app = cx.weak_entity();
        let focus = if location == VoiceControls::Session {
            self.screen_sidebar_focus.clone()
        } else {
            self.screen_channel_focus.clone()
        };
        VoiceButtonGroup::new(
            "screen-controls",
            voice_shortcut_tooltip(
                voice_button("screen-share", IconName::Monitor, label, location)
                    .selected(active)
                    .track_focus(&focus)
                    .on_click(cx.listener(move |app, _, window, cx| {
                        let View::SignedIn(sidebar) = &app.view else {
                            return;
                        };
                        if let Some(call) = sidebar
                            .voice
                            .as_ref()
                            .filter(|call| call.is_participating())
                        {
                            focus.focus(window, cx);
                            app.send(screen_share_action(call).1, None, cx);
                        }
                    })),
                label,
                super::workspace_keys::media_shortcut_keys("S", cx)
                    .map(str::to_owned)
                    .to_vec(),
            ),
            "Screen Share options",
            floating_popover("screen-options", cx)
                .w(px(280.0))
                .content(move |_, _, cx| {
                    let Some(app) = app.upgrade() else {
                        return div();
                    };
                    let View::SignedIn(sidebar) = &app.read(cx).view else {
                        return div();
                    };
                    let intent = &sidebar.voice_intent;
                    screen_preferences(intent.screen.quality, intent.preferred.screen_audio, &app)
                }),
        )
    }

    pub(super) fn screen_capture_status(
        &self,
        call: &VoiceView,
        sidebar: &Sidebar,
    ) -> Option<Stateful<Div>> {
        let report = call.screen.as_ref()?;
        let selected = sidebar.voice_intent.screen.quality;
        let panel = div()
            .id("screen-capture")
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .px_2()
            .pb_2();
        Some(match &report.state {
            ScreenState::Picking if self.screen_picker_focus.is_none() => {
                panel.child(Label::new("Choose a display or window…"))
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
            ScreenState::Ended(message) if self.screen_picker_focus.is_none() => {
                panel.child(Label::new(*message))
            }
            _ => return None,
        })
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
        if self.video_stage.pointer_over_window(window)
            && let Some(id) = self.video_stage.hovered.clone()
        {
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
            || self
                .selected_voice_destination(sidebar)
                .is_none_or(|channel| channel.id != call.channel_id)
        {
            return;
        }
        let Some(participant) = joined_participants(sidebar, call)
            .into_iter()
            .find(|participant| participant.id == id)
        else {
            return;
        };
        let available = TileActions::for_participant(self, &participant, call, sidebar);
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
        window: &mut Window,
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
                if let Some(focus) = self.video_stage.tile_focus.get(id) {
                    focus.focus(window, cx);
                }
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
        controls: TileControls,
        menu: Option<impl IntoElement>,
        cx: &Context<Self>,
    ) -> Div {
        let TileControls {
            actions: available,
            revealed: reveal_actions,
            compact,
            screen_overlay,
        } = controls;
        let id = participant.id.to_owned();
        let watch_id = id.clone();
        let theater_id = id.clone();
        let viewers = sidebar.viewers(&call.channel_id, participant.id);
        let row = div()
            .absolute()
            .bottom_2()
            .left_2()
            .right_2()
            .flex()
            .when(!screen_overlay, |row| row.flex_wrap())
            .when(screen_overlay, |row| row.min_h(CONTROL_HEIGHT))
            .items_center()
            .justify_end()
            .gap_1()
            .when(available.live && !viewers.is_empty(), |row| {
                if screen_overlay {
                    row.child(screen_viewers(&viewers, compact, sidebar, cx))
                } else {
                    let names = viewers.iter().map(|viewer| viewer.name).collect::<Vec<_>>();
                    row.child(
                        Label::new(format!("Watching: {}", names.join(", ")))
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .rounded_md()
                            .bg(cx.theme().background)
                            .text_xs()
                            .truncate(),
                    )
                }
            });
        if screen_overlay && !reveal_actions {
            return row;
        }
        let watch = available.permits(TileAction::Watch).then(|| {
            let label = if available.watching {
                "Stop watching"
            } else {
                "Watch"
            };
            tool_button_with_label(
                "watch",
                if available.watching {
                    IconName::EyeOff
                } else {
                    IconName::Eye
                },
                label,
                !available.watching,
            )
            .secondary()
            .flex_shrink_0()
            .on_click(cx.listener(move |app, _, window, cx| {
                app.apply_tile_action(&watch_id, TileAction::Watch, &available, window, cx);
            }))
        });
        let (watch, screen_watch) = if screen_overlay {
            (None, watch)
        } else {
            (watch, None)
        };
        row.children(watch).child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .when(screen_overlay, |row| row.gap_0p5().flex_shrink_0())
                .when(!reveal_actions, |actions| {
                    actions
                        .invisible()
                        .group_hover("voice-tile", |style| style.visible())
                })
                .children(screen_watch)
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
                        .on_click(cx.listener(
                            move |app, _, window, cx| {
                                app.apply_tile_action(
                                    &theater_id,
                                    TileAction::Theater,
                                    &available,
                                    window,
                                    cx,
                                );
                            },
                        )),
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
                        .on_click(cx.listener(
                            move |app, _, window, cx| {
                                app.apply_tile_action(
                                    &id,
                                    TileAction::Fullscreen,
                                    &available,
                                    window,
                                    cx,
                                );
                            },
                        )),
                    )
                })
                .when(available.screen_volume, |row| {
                    row.child(screen_volume(participant.id, cx))
                })
                .children(menu),
        )
    }
}

fn screen_viewers(viewers: &[Participant<'_>], compact: bool, sidebar: &Sidebar, cx: &App) -> Div {
    // The compact row has room for two circles beside five standard action buttons.
    let limit = if compact && viewers.len() > 2 {
        1
    } else if compact {
        2
    } else {
        5
    };
    div()
        .flex()
        .mr_auto()
        .flex_shrink_0()
        .children(
            viewers
                .iter()
                .take(limit)
                .enumerate()
                .map(|(index, viewer)| {
                    let name = viewer.name.to_owned();
                    div()
                        .id(SharedString::from(format!("screen-viewer-{}", viewer.id)))
                        .role(Role::Image)
                        .aria_label(name.clone())
                        .when(index > 0, |avatar| avatar.ml(px(-6.0)))
                        .tooltip(move |window, cx| {
                            floating_tooltip(Tooltip::new(name.clone()), cx).build(window, cx)
                        })
                        .child(profile_avatar(
                            Avatar::new().name(viewer.name.to_owned()),
                            sidebar.avatar(Some(viewer.id)),
                            24.0,
                            false,
                            cx,
                        ))
                }),
        )
        .when(viewers.len() > limit, |row| {
            let hidden = &viewers[limit..];
            let names = hidden
                .iter()
                .map(|viewer| viewer.name)
                .collect::<Vec<_>>()
                .join(", ");
            row.child(
                div()
                    .id("more-viewers")
                    .ml(px(-6.0))
                    .size_6()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .text_xs()
                    .aria_label(format!("More viewers: {names}"))
                    .tooltip(move |window, cx| {
                        floating_tooltip(Tooltip::new(names.clone()), cx).build(window, cx)
                    })
                    .child(format!("+{}", hidden.len())),
            )
        })
}

fn screen_volume(id: &str, cx: &Context<Spk>) -> Popover {
    let id = id.to_owned();
    let open_id = id.clone();
    let app = cx.weak_entity();
    floating_popover("screen-volume", cx)
        .anchor(Anchor::BottomRight)
        .w(px(192.0))
        .trigger(
            tool_button(
                "screen-volume-trigger",
                IconName::Volume2,
                "Screen Share volume",
            )
            .secondary(),
        )
        .on_open_change({
            let app = app.clone();
            move |open, _, cx| {
                let _ = app.update(cx, |app, cx| {
                    if *open {
                        app.video_stage.screen_volume_for = Some(open_id.clone());
                    } else if app.video_stage.screen_volume_for.as_deref() == Some(&open_id) {
                        app.video_stage.screen_volume_for = None;
                    }
                    cx.notify();
                });
            }
        })
        .content(move |_, window, cx| {
            let Some(app) = app.upgrade() else {
                return div();
            };
            let View::SignedIn(sidebar) = &app.read(cx).view else {
                return div();
            };
            let volume = sidebar
                .voice_intent
                .preferred
                .participant_volume(&id)
                .screen;
            let id = id.clone();
            let slider = quick_level(
                "screen-volume-level",
                volume,
                200,
                &app,
                window,
                cx,
                move |value| {
                    Command::SetParticipantVolume(
                        id.clone(),
                        ParticipantVolumeChange::Screen(value),
                    )
                },
            );
            div().child(
                Slider::new(&slider)
                    .tab_stop(true)
                    .accessibility_label("Screen Share volume"),
            )
        })
}

pub(super) fn screen_share_action(call: &VoiceView) -> (&'static str, Command) {
    match &call.screen {
        Some(report) if report.state.active() => (
            "Stop Screen Share",
            Command::StopScreenShare(report.attempt),
        ),
        Some(report) if !matches!(report.state, ScreenState::Ended(_)) => (
            "Cancel Screen Share",
            Command::StopScreenShare(report.attempt),
        ),
        _ => ("Share Screen", Command::OpenScreenShare),
    }
}

fn screen_preferences(selected: Quality, share_audio: bool, app: &Entity<Spk>) -> Div {
    let audio = app.downgrade();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            Switch::new("screen-share-audio")
                .label("Share audio")
                .accessibility_label("Share audio")
                .tab_stop(true)
                .checked(share_audio)
                .on_change(move |enabled, _, cx| {
                    let _ = audio.update(cx, |app, cx| {
                        app.send(Command::SetScreenAudio(*enabled), None, cx)
                    });
                }),
        )
        .children(
            [
                (
                    "Max resolution",
                    Quality::HEIGHTS,
                    selected.height,
                    "p",
                    Command::SetScreenHeight as fn(u32) -> Command,
                ),
                (
                    "Frame rate",
                    Quality::FPS,
                    selected.fps,
                    " fps",
                    Command::SetScreenFps,
                ),
            ]
            .into_iter()
            .map(|(label, choices, selected, unit, command)| {
                let options = choices
                    .map(|value| VoiceOption {
                        title: format!("{value}{unit}").into(),
                        value,
                    })
                    .to_vec();
                div()
                    .id(label)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(label).font_semibold())
                    .child(VoiceSelect {
                        label,
                        options,
                        value: selected,
                        app: app.downgrade(),
                        command,
                    })
            }),
        )
}

pub(super) fn joined_participants<'a>(
    sidebar: &'a Sidebar,
    call: &VoiceView,
) -> Vec<Participant<'a>> {
    let mut participants = sidebar.participants(&call.channel_id);
    if let Some(id) = sidebar.profile.id.as_deref()
        && !participants.iter().any(|person| person.id == id)
    {
        // Local media can connect before SPK echoes our voice presence.
        participants.insert(
            0,
            Participant {
                id,
                name: sidebar.profile.name(),
                flags: Default::default(),
                microphone_connecting: false,
                video: None,
            },
        );
    }
    participants
}

fn participant_video(video: Option<&ParticipantVideo>) -> bool {
    video.is_some_and(|video| !video.live.is_empty())
}

#[derive(Clone, PartialEq)]
struct VoiceOption<T> {
    title: SharedString,
    value: T,
}

impl<T: Clone + PartialEq> SelectItem for VoiceOption<T> {
    type Value = T;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &T {
        &self.value
    }
}

struct VoiceSelectState<T: Clone + PartialEq + 'static> {
    select: Entity<SelectState<SearchableVec<VoiceOption<T>>>>,
    options: Vec<VoiceOption<T>>,
    selected: IndexPath,
}

#[derive(IntoElement)]
struct VoiceSelect<T: Clone + PartialEq + 'static> {
    label: &'static str,
    options: Vec<VoiceOption<T>>,
    value: T,
    app: WeakEntity<Spk>,
    command: fn(T) -> Command,
}

impl<T: Clone + PartialEq + 'static> RenderOnce for VoiceSelect<T> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let selected = IndexPath::default().row(
            self.options
                .iter()
                .position(|option| option.value == self.value)
                .unwrap_or(0),
        );
        let state = window.use_keyed_state(self.label, cx, |window, cx| {
            let select = cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(self.options.clone()),
                    Some(selected),
                    window,
                    cx,
                )
            });
            let app = self.app;
            let command = self.command;
            cx.subscribe(&select, move |_, _, event, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let _ = app.update(cx, |app, cx| {
                        app.send(command(value.clone()), None, cx);
                    });
                }
            })
            .detach();
            VoiceSelectState {
                select,
                options: self.options.clone(),
                selected,
            }
        });
        let picker = state.read(cx);
        let select = picker.select.clone();
        if picker.options != self.options || picker.selected != selected {
            state.update(cx, |picker, cx| {
                select.update(cx, |select, cx| {
                    if picker.options != self.options {
                        select.set_items(SearchableVec::new(self.options.clone()), window, cx);
                    }
                    select.set_selected_index(Some(selected), window, cx);
                });
                picker.options = self.options;
                picker.selected = selected;
            });
        }
        control_select(&select, cx.theme().background, cx)
            .small()
            .w_full()
            .accessibility_label(self.label)
    }
}

pub(super) fn camera_options(
    sidebar: &Sidebar,
    app: &Entity<Spk>,
    devices: &[CameraDevice],
) -> Div {
    let choice = &sidebar.voice_intent.camera;
    let options = std::iter::once(VoiceOption {
        title: "System default".into(),
        value: None,
    })
    .chain(devices.iter().map(|device| VoiceOption {
        title: device.name.clone().into(),
        value: Some(device.id.clone()),
    }))
    .collect();
    let unavailable = choice
        .device
        .as_ref()
        .is_some_and(|id| !devices.iter().any(|device| &device.id == id));
    let resolutions = CameraResolution::ALL
        .map(|resolution| VoiceOption {
            title: resolution.label().into(),
            value: resolution,
        })
        .to_vec();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(Label::new("Camera").font_semibold())
        .child(VoiceSelect {
            label: "Camera",
            options,
            value: choice.device.clone(),
            app: app.downgrade(),
            command: Command::ChooseCamera,
        })
        .when(unavailable, |panel| {
            panel.child(Label::new("Selected Camera unavailable").text_xs())
        })
        .when(devices.is_empty(), |panel| {
            panel.child(Label::new("No Cameras detected").text_xs())
        })
        .child(Label::new("Webcam resolution").font_semibold())
        .child(VoiceSelect {
            label: "Webcam resolution",
            options: resolutions,
            value: choice.resolution,
            app: app.downgrade(),
            command: Command::ChooseCameraResolution,
        })
        .when_some(
            match sidebar.camera_status() {
                CameraStatus::Live(width, height) => Some(format!("Live at {width} × {height}")),
                CameraStatus::Failed(message) => Some(message.into()),
                _ => None,
            },
            |panel, status| panel.child(Label::new(status).text_xs()),
        )
}
