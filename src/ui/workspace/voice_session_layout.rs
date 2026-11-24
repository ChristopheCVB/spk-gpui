use gpui_kit::{prelude::FluentBuilder, *};

pub(super) const VOICE_SESSION_INSET: Rems = rems(0.5);
const MIN_DETAILS_WIDTH: Pixels = px(120.0);

#[derive(Clone, Copy)]
pub(super) enum VoiceSessionPlacement {
    Sidebar(Pixels),
    Drawer,
    BottomBar,
}

pub(super) struct VoiceSessionLayout {
    pub placement: VoiceSessionPlacement,
    pub channel: AnyElement,
    pub diagnostics: AnyElement,
    pub actions: Option<AnyElement>,
}

impl IntoElement for VoiceSessionLayout {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for VoiceSessionLayout {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
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
        let channel = self.channel.request_layout(window, cx);
        let diagnostics = self.diagnostics.request_layout(window, cx);
        let actions = self
            .actions
            .as_mut()
            .map(|actions| actions.request_layout(window, cx));
        let mut row = div().flex().items_center().gap_1().p(VOICE_SESSION_INSET);
        let stacked = if let VoiceSessionPlacement::Sidebar(width) = self.placement {
            let details = window.request_layout(
                Style::default().refined(div().min_w(MIN_DETAILS_WIDTH).style().clone()),
                [diagnostics],
                cx,
            );
            let probe = window.request_layout(
                Style::default().refined(row.style().clone()),
                [details].into_iter().chain(actions),
                cx,
            );
            window.compute_layout(
                probe,
                size(AvailableSpace::MaxContent, AvailableSpace::MaxContent),
                cx,
            );
            let card_width = width - VOICE_SESSION_INSET * window.rem_size() * 2.0;
            // Read only the probe bounds so GPUI can place the same children in the final row.
            window.layout_bounds(probe).size.width > card_width
        } else {
            matches!(self.placement, VoiceSessionPlacement::Drawer)
        };
        let mut details = div()
            .flex()
            .min_w_0()
            .when(stacked, |details| {
                details.w_full().items_center().justify_between().gap_1()
            })
            .when(!stacked, |details| {
                details.flex_1().flex_col().items_start()
            });
        let details = window.request_layout(
            Style::default().refined(details.style().clone()),
            [channel, diagnostics],
            cx,
        );
        row = row.when(stacked, |row| row.flex_col());
        (
            window.request_layout(
                Style::default().refined(row.style().clone()),
                [details].into_iter().chain(actions),
                cx,
            ),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.channel.prepaint(window, cx);
        self.diagnostics.prepaint(window, cx);
        if let Some(actions) = &mut self.actions {
            actions.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.channel.paint(window, cx);
        self.diagnostics.paint(window, cx);
        if let Some(actions) = &mut self.actions {
            actions.paint(window, cx);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{VOICE_SESSION_INSET, VoiceSessionLayout, VoiceSessionPlacement};
    use futures::{
        channel::oneshot,
        future::{Either, select},
    };
    use gpui_kit::{
        AnyElement, AppContext, Bounds, Context, IntoElement, ParentElement, Pixels, Render,
        Styled, Window, WindowOptions, application, canvas, div, px,
    };
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        time::Duration,
    };

    type PaintedBounds = (Bounds<Pixels>, Bounds<Pixels>);

    struct VoiceSessionFixture {
        placement: VoiceSessionPlacement,
        diagnostics_width: Pixels,
        actions_width: Pixels,
        painted: Option<oneshot::Sender<PaintedBounds>>,
    }

    impl Render for VoiceSessionFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let pane = match self.placement {
                VoiceSessionPlacement::Sidebar(width) => div().w(width).px(VOICE_SESSION_INSET),
                VoiceSessionPlacement::Drawer => div().w(px(288.)).px_4(),
                VoiceSessionPlacement::BottomBar => div().w(px(400.)),
            };
            let diagnostics = Rc::new(Cell::new(Bounds::default()));
            let painted = self.painted.take();
            pane.child(VoiceSessionLayout {
                placement: self.placement,
                channel: div().w(px(95.)).h(px(32.)).into_any_element(),
                diagnostics: measured_box(self.diagnostics_width, {
                    let diagnostics = diagnostics.clone();
                    move |bounds| diagnostics.set(bounds)
                }),
                actions: Some(measured_box(self.actions_width, move |actions| {
                    if let Some(painted) = painted {
                        let _ = painted.send((diagnostics.get(), actions));
                    }
                })),
            })
        }
    }

    fn measured_box(width: Pixels, painted: impl FnOnce(Bounds<Pixels>) + 'static) -> AnyElement {
        canvas(|_, _, _| (), move |bounds, _, _, _| painted(bounds))
            .w(width)
            .h(px(32.))
            .flex_shrink_0()
            .into_any_element()
    }

    #[test]
    #[ignore = "requires a graphical session or Xvfb"]
    fn sidebar_voice_layout_uses_rendered_control_widths() {
        let samples = Rc::new(RefCell::new(Vec::new()));
        let result = samples.clone();
        let sidebar = VoiceSessionPlacement::Sidebar(px(400.));
        let cases = [
            (sidebar, 108., 236., false),
            (sidebar, 172., 236., true),
            (sidebar, 108., 260., true),
            (VoiceSessionPlacement::Drawer, 108., 32., true),
        ];
        application().run(move |cx| {
            let handle = cx
                .open_window(WindowOptions::default(), |_, cx| {
                    cx.new(|_| VoiceSessionFixture {
                        placement: sidebar,
                        diagnostics_width: px(108.),
                        actions_width: px(236.),
                        painted: None,
                    })
                })
                .unwrap();
            cx.spawn(async move |cx| {
                for (placement, diagnostics_width, actions_width, _) in cases {
                    let (report, painted) = oneshot::channel();
                    handle
                        .update(cx, |fixture, _, cx| {
                            fixture.placement = placement;
                            fixture.diagnostics_width = px(diagnostics_width);
                            fixture.actions_width = px(actions_width);
                            fixture.painted = Some(report);
                            cx.notify();
                        })
                        .unwrap();
                    let timeout = cx.background_executor().timer(Duration::from_secs(5));
                    let Either::Left((Ok(bounds), _)) = select(painted, timeout).await else {
                        break;
                    };
                    result.borrow_mut().push(bounds);
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
        let samples = samples.take();
        assert_eq!(samples.len(), cases.len(), "each fixture case must paint");
        for ((diagnostics, actions), (_, diagnostics_width, actions_width, stacked)) in
            samples.into_iter().zip(cases)
        {
            assert_eq!(diagnostics.size.width, px(diagnostics_width));
            assert_eq!(actions.size.width, px(actions_width));
            if stacked {
                assert!(actions.top() >= diagnostics.bottom());
            } else {
                assert!(actions.left() >= diagnostics.right());
                assert!(actions.top() < diagnostics.bottom());
            }
        }
    }
}
