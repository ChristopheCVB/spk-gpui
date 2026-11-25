use std::time::Instant;

use super::*;

pub(super) fn chat_image(image: Arc<ChatImage>) -> Div {
    if image.pixels.frame_count() == 1 {
        return div().child(
            img(image.pixels.clone())
                .size_full()
                .object_fit(ObjectFit::Contain),
        );
    }

    div().child(AnimatedGif(image))
}

struct AnimatedGif(Arc<ChatImage>);

#[derive(Default)]
struct Playback {
    elapsed: Duration,
    visible_since: Option<Instant>,
    wake: Option<(Instant, Task<()>)>,
}

impl Element for AnimatedGif {
    type RequestLayoutState = ();
    type PrepaintState = Option<usize>;

    fn id(&self) -> Option<ElementId> {
        Some(("chat-gif", self.0.pixels.id.0).into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: size(relative(1.).into(), relative(1.).into()),
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        let image_bounds = ObjectFit::Contain.get_bounds(bounds, self.0.pixels.size(0));
        let visible = window.content_mask().bounds.intersect(&image_bounds).size;
        let visible = visible.width > px(0.) && visible.height > px(0.);
        window.with_element_state(id.unwrap(), |state: Option<Playback>, window| {
            let mut state = state.unwrap_or_default();
            if cx.reduce_motion() {
                return (visible.then_some(0), Playback::default());
            }
            let now = Instant::now();
            if !visible {
                if let Some(since) = state.visible_since.take() {
                    state.elapsed += now - since;
                }
                state.wake = None;
                return (None, state);
            }
            let since = *state.visible_since.get_or_insert(now);
            let elapsed = state.elapsed + (now - since);
            let (frame, next) = self.0.frame_at(elapsed);
            let deadline = next.map(|delay| now + delay);
            if state.wake.as_ref().map(|(deadline, _)| *deadline) != deadline {
                state.wake = deadline.map(|deadline| {
                    let task = window.spawn(cx, async move |cx| {
                        cx.background_executor()
                            .timer(deadline.saturating_duration_since(Instant::now()))
                            .await;
                        let _ = cx.update(|window, _| window.refresh());
                    });
                    (deadline, task)
                });
            }
            (Some(frame), state)
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        frame: &mut Option<usize>,
        window: &mut Window,
        _: &mut App,
    ) {
        if let Some(frame) = *frame {
            let image = &self.0.pixels;
            let image_bounds = ObjectFit::Contain.get_bounds(bounds, image.size(frame));
            let _ = window.paint_image(
                bounds,
                image_bounds,
                Corners::default(),
                image.clone(),
                frame,
                false,
            );
        }
    }
}
