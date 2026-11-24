mod blur;

use std::collections::HashMap;

use gpui_kit::base::text::{InlineElement, InlineRenderContext};

use super::*;

pub(super) struct Content {
    pub formatted: SharedString,
    pub plain: SharedString,
    pub revealed: bool,
}

pub(super) struct MessageSpoiler {
    pub app: WeakEntity<Spk>,
    pub key: (String, u64),
    pub link_prefix: String,
    pub contents: HashMap<usize, Content>,
    pub max_width: Pixels,
}

impl MarkdownPlugin for MessageSpoiler {
    fn name(&self) -> &str {
        "spk-spoiler"
    }

    fn parse(
        &self,
        node: &markdown::mdast::Node,
        _: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let markdown::mdast::Node::Image(image) = node else {
            return None;
        };
        let offset = image.url.strip_prefix(&self.link_prefix)?.parse().ok()?;
        let content = self.contents.get(&offset)?;
        let node = MarkdownNode::new(self.name().to_owned(), offset);
        Some(if content.revealed {
            node.text(content.plain.clone())
                .markdown(content.formatted.clone())
        } else {
            node.text("[spoiler]")
                .markdown("[spoiler]")
                .accessibility_label("Spoiler: click to reveal")
        })
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let &offset = node.data::<usize>()?;
        let content = self.contents.get(&offset)?;
        let id: SharedString = format!("spoiler-{}-{}-{offset}", self.key.0, self.key.1).into();
        let app = self.app.clone();
        let rich_text = TextView::markdown(id.clone(), content.formatted.clone())
            .style(chat_text_style(cx))
            .text_size(context.font_size())
            .line_height(context.line_height())
            .selectable(false)
            .markdown_extensions(
                MarkdownExtensions::default()
                    .plugin(MessageMention)
                    .parser_revision(1)
                    .raw_html(false),
            )
            .link(|link, _| safe_link(&link.url).map(|_| link.clone()))
            .on_link_click(move |href, _, _, cx| {
                if let Some(url) = safe_link(href) {
                    search::activate_chat_link(&app, url, cx);
                }
            });
        let container = div()
            .relative()
            .overflow_hidden()
            .max_w(self.max_width)
            .min_w(px(24.))
            .min_h(context.line_height())
            .px_0p5()
            .rounded_sm()
            .bg(cx.theme().popover);
        if content.revealed {
            return Some(InlineElement::new(container.child(rich_text)));
        }

        let focus = window
            .use_keyed_state(id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let theme = cx.theme();
        let border = if focus.is_focused(window) && window.last_input_was_keyboard() {
            theme.ring
        } else {
            theme.transparent
        };
        let app = self.app.clone();
        let key = self.key.clone();
        let label = "Spoiler: click to reveal";
        Some(InlineElement::new(
            container
                .child(LayoutOnly(rich_text.into_any_element()))
                .child(blur::formatted_spoiler(&content.formatted, context, cx))
                .child(
                    Button::new(id)
                        .ghost()
                        .absolute()
                        .inset_0()
                        .h_auto()
                        .p_0()
                        .cursor_pointer()
                        .rounded_sm()
                        .border_1()
                        .border_color(border)
                        .track_focus(&focus)
                        .tab_stop(true)
                        .accessibility_label(label)
                        .tooltip(label)
                        .child(
                            Icon::new(IconName::Eye)
                                .size(px(16.))
                                .text_color(theme.foreground),
                        )
                        .on_click(move |event, window, cx| {
                            let _ = app.update(cx, |app, cx| {
                                app.preserve_chat_position();
                                app.chat_spoilers
                                    .entry(key.clone())
                                    .or_default()
                                    .insert(offset);
                                if event.is_keyboard()
                                    && let Some(focus) = app.chat_row_focus.borrow().get(&key.0)
                                {
                                    focus.focus(window, cx);
                                }
                                cx.notify();
                            });
                        }),
                ),
        ))
    }
}

// Match the revealed Markdown's layout without painting or exposing its content to input or accessibility.
struct LayoutOnly(AnyElement);

impl IntoElement for LayoutOnly {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for LayoutOnly {
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
        (self.0.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }
}
