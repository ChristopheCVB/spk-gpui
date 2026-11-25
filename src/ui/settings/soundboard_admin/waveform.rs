use super::*;
use crate::session::{SoundboardCrop, SoundboardTrim as Trim, SoundboardWaveform};

const HANDLE_WIDTH: Pixels = px(16.0);
const GRIP_SIZE: Pixels = px(20.0);
const KEY_STEP: f64 = 0.1;

#[derive(Clone, Copy)]
pub(super) struct Drag {
    target: Trim,
    mouse_x: Pixels,
    seconds: f64,
}

pub(super) fn timing(crop: SoundboardCrop) -> impl IntoElement {
    div().flex().flex_wrap().gap_x_4().children(
        [
            ("Start", crop.start),
            ("End", crop.end),
            ("Duration", crop.end - crop.start),
        ]
        .map(|(label, seconds)| Label::new(format!("{label}: {seconds:.3} s")).id(label)),
    )
}

impl SoundboardForm {
    pub(super) fn waveform(
        &self,
        waveform: &SoundboardWaveform,
        crop: SoundboardCrop,
        playhead: f64,
        disabled: bool,
        cx: &Context<Settings>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let drawing = waveform.clone();
        let duration = waveform.duration;
        let peak_color = theme.muted_foreground;
        let selection_color = theme.primary.opacity(0.22);
        let move_drag = cx.listener(
            move |settings: &mut Settings, &(x, width): &(Pixels, Pixels), _, cx| {
                if let Some(drag) = settings.soundboard.drag
                    && width > px(0.0)
                {
                    let seconds = drag.seconds + f64::from((x - drag.mouse_x) / width) * duration;
                    settings.soundboard_action(Action::Trim(drag.target, seconds), cx);
                }
            },
        );
        let settings = cx.weak_entity();
        let draw = canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                for (bin, peak) in drawing.peaks.iter().enumerate() {
                    let left = drawing.peak_width * bin as f64;
                    let right = (left + drawing.peak_width).min(1.0);
                    let width = bounds.size.width * (right - left) as f32;
                    let height = (bounds.size.height * (peak * 0.85)).max(px(1.0));
                    window.paint_quad(fill(
                        Bounds::new(
                            point(
                                bounds.left() + bounds.size.width * left as f32,
                                bounds.center().y - height / 2.0,
                            ),
                            size(width * 0.75, height),
                        ),
                        peak_color,
                    ));
                }
                if !disabled {
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase == DispatchPhase::Capture {
                            if event.dragging() {
                                move_drag(&(event.position.x, bounds.size.width), window, cx);
                            }
                        }
                    });
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                            let _ = settings.update(cx, |settings, _| {
                                settings.soundboard.drag = None;
                            });
                        }
                    });
                }
            },
        )
        .absolute()
        .inset_0();

        div()
            .id("source-waveform")
            .role(Role::Group)
            .aria_label("Source waveform")
            .aria_description(format!("Source duration: {duration:.3} s"))
            .w_full()
            .h(px(96.0))
            .relative()
            .rounded_md()
            .bg(theme.muted)
            .child(
                div()
                    .absolute()
                    .left(HANDLE_WIDTH)
                    .right(HANDLE_WIDTH)
                    .top_0()
                    .bottom_0()
                    .child(draw)
                    .child(
                        div()
                            .id("sound-selection")
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(relative((crop.start / duration) as f32))
                            .w(relative(((crop.end - crop.start) / duration) as f32))
                            .bg(selection_color)
                            .when(!disabled, |region| {
                                region.cursor(CursorStyle::OpenHand).on_mouse_down(
                                    MouseButton::Left,
                                    begin_drag(Trim::Region, crop.start, cx),
                                )
                            }),
                    )
                    .child(trim_handle(
                        Trim::Start,
                        crop,
                        waveform,
                        disabled,
                        &self.trim_focus[0],
                        cx,
                    ))
                    .child(trim_handle(
                        Trim::End,
                        crop,
                        waveform,
                        disabled,
                        &self.trim_focus[1],
                        cx,
                    ))
                    .child(
                        div()
                            .id("preview-playhead")
                            .role(Role::ProgressIndicator)
                            .aria_label("Preview playhead")
                            .aria_value(format!("{playhead:.3} s"))
                            .aria_numeric_value(playhead)
                            .aria_min_numeric_value(crop.start)
                            .aria_max_numeric_value(crop.end)
                            .absolute()
                            .left(relative((playhead / duration) as f32))
                            .ml(px(-1.0))
                            .top_0()
                            .bottom_0()
                            .w(px(2.0))
                            .bg(theme.foreground),
                    )
                    .child(
                        div()
                            .id("move-sound-selection")
                            .absolute()
                            .bottom_0()
                            .left(relative(
                                ((crop.start + crop.end) / (2.0 * duration)) as f32,
                            ))
                            .ml(-GRIP_SIZE / 2.0)
                            .size(GRIP_SIZE)
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(theme.primary)
                            .child(
                                div()
                                    .h(px(2.0))
                                    .w(px(10.0))
                                    .rounded_full()
                                    .bg(theme.primary_foreground),
                            )
                            .when(!disabled, |grip| {
                                grip.cursor(CursorStyle::OpenHand).on_mouse_down(
                                    MouseButton::Left,
                                    begin_drag(Trim::Region, crop.start, cx),
                                )
                            }),
                    ),
            )
    }
}

fn begin_drag(
    target: Trim,
    seconds: f64,
    cx: &Context<Settings>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |settings, event: &MouseDownEvent, _, _| {
        settings.soundboard.drag = Some(Drag {
            target,
            mouse_x: event.position.x,
            seconds,
        });
    })
}

fn trim_handle(
    target: Trim,
    crop: SoundboardCrop,
    waveform: &SoundboardWaveform,
    disabled: bool,
    focus: &FocusHandle,
    cx: &Context<Settings>,
) -> impl IntoElement {
    let theme = cx.theme();
    let start = matches!(target, Trim::Start);
    let label = if start { "Trim start" } else { "Trim end" };
    let seconds = if start { crop.start } else { crop.end };
    let duration = waveform.duration;
    let (minimum, maximum) = waveform.trim_bounds(crop, target);
    div()
        .absolute()
        .left(relative((seconds / duration) as f32))
        .top_0()
        .bottom(GRIP_SIZE)
        .child(TrimHandle {
            disabled,
            control: div()
                .id(label)
                .absolute()
                .left(if start { -HANDLE_WIDTH } else { px(0.0) })
                .top_0()
                .bottom_0()
                .w(HANDLE_WIDTH)
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .bg(theme.primary)
                .border_2()
                .border_color(theme.primary)
                .focus_visible(|style| style.border_color(theme.foreground))
                .role(Role::Slider)
                .aria_label(label)
                .aria_value(format!("{seconds:.3} s"))
                .aria_numeric_value(seconds)
                .aria_min_numeric_value(minimum)
                .aria_max_numeric_value(maximum)
                .aria_numeric_value_step(KEY_STEP)
                .aria_orientation(Orientation::Horizontal)
                .child(
                    div()
                        .h(px(20.0))
                        .w(px(2.0))
                        .rounded_full()
                        .bg(theme.primary_foreground),
                )
                .when(!disabled, |handle| {
                    handle
                        .track_focus(focus)
                        .cursor(CursorStyle::ResizeLeftRight)
                        .on_mouse_down(MouseButton::Left, begin_drag(target, seconds, cx))
                        .on_key_down(cx.listener(move |settings, event: &KeyDownEvent, _, cx| {
                            let modifiers = event.keystroke.modifiers;
                            if modifiers.control || modifiers.alt || modifiers.platform {
                                return;
                            }
                            let step = if modifiers.shift { 1.0 } else { KEY_STEP };
                            let action = match event.keystroke.key.as_str() {
                                "left" | "down" => Action::NudgeTrim(target, -step),
                                "right" | "up" => Action::NudgeTrim(target, step),
                                "home" => Action::Trim(target, minimum),
                                "end" => Action::Trim(target, maximum),
                                _ => return,
                            };
                            settings.soundboard_action(action, cx);
                            cx.stop_propagation();
                        }))
                        .on_a11y_action(AccessibleAction::Increment, {
                            let settings = cx.weak_entity();
                            move |_, _, cx| {
                                let _ = settings.update(cx, |settings, cx| {
                                    settings
                                        .soundboard_action(Action::NudgeTrim(target, KEY_STEP), cx);
                                });
                            }
                        })
                        .on_a11y_action(AccessibleAction::Decrement, {
                            let settings = cx.weak_entity();
                            move |_, _, cx| {
                                let _ = settings.update(cx, |settings, cx| {
                                    settings.soundboard_action(
                                        Action::NudgeTrim(target, -KEY_STEP),
                                        cx,
                                    );
                                });
                            }
                        })
                        .on_a11y_action(AccessibleAction::SetValue, {
                            let settings = cx.weak_entity();
                            move |data, _, cx| {
                                if let Some(accesskit::ActionData::NumericValue(seconds)) = data {
                                    let _ = settings.update(cx, |settings, cx| {
                                        settings
                                            .soundboard_action(Action::Trim(target, *seconds), cx);
                                    });
                                }
                            }
                        })
                }),
        })
}

// GPUI Kit's disabled-node wrapper is private to its built-in controls.
struct TrimHandle {
    control: Stateful<Div>,
    disabled: bool,
}

impl Element for TrimHandle {
    type RequestLayoutState = <Stateful<Div> as Element>::RequestLayoutState;
    type PrepaintState = <Stateful<Div> as Element>::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        Element::id(&self.control)
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.control.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.control.request_layout(id, inspector, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.control
            .prepaint(id, inspector, bounds, layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.control
            .paint(id, inspector, bounds, layout, prepaint, window, cx);
    }

    fn a11y_role(&self) -> Option<Role> {
        self.control.a11y_role()
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        self.control.write_a11y_info(node);
        if self.disabled {
            node.set_disabled();
        }
    }
}
