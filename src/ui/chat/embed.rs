use super::*;
use crate::api::{ChatEmbed, EmbedColor};
use gpui_kit::base::text::{InlineElement, InlineRenderContext};
use markdown::mdast::Node;
use url::Url;

pub(super) struct EmbedLinks {
    pub app: WeakEntity<Spk>,
    pub max_width: Pixels,
}

enum LinkLabel {
    Code(String),
    Text(Vec<LinkLabelPart>),
}

enum LinkLabelPart {
    Text(String),
    Emoji { label: String, path: String },
}

impl MarkdownPlugin for EmbedLinks {
    fn name(&self) -> &str {
        "spk-embed-link"
    }

    fn parse(&self, node: &Node, context: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Link(link) = node else {
            return None;
        };
        let url = safe_link(&link.url)?;
        let (label, content) = if let [Node::InlineCode(code)] = link.children.as_slice() {
            (code.value.clone(), LinkLabel::Code(code.value.clone()))
        } else {
            let mut label = String::new();
            let mut parts = Vec::new();
            for child in &link.children {
                let text = match child {
                    Node::Text(text) => text.value.as_str(),
                    Node::InlineMath(_) => context.node_source(child)?,
                    Node::Image(image) if image.url == emoji::EMOJI_URL => {
                        label.push_str(&image.alt);
                        parts.push(LinkLabelPart::Emoji {
                            path: reaction_artwork_path(&image.alt)?,
                            label: image.alt.clone(),
                        });
                        continue;
                    }
                    _ => return None,
                };
                label.push_str(text);
                if let Some(LinkLabelPart::Text(previous)) = parts.last_mut() {
                    previous.push_str(text);
                } else {
                    parts.push(LinkLabelPart::Text(text.to_owned()));
                }
            }
            (label, LinkLabel::Text(parts))
        };
        if label.is_empty() {
            return None;
        }
        Some(
            MarkdownNode::new(self.name(), (url, content))
                .text(label)
                .markdown(context.node_source(node)?.to_owned()),
        )
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let (url, content) = node.data::<(Url, LinkLabel)>()?;
        let url = url.clone();
        let label = node.as_text().to_owned();
        let offset = node.source_range()?.start;
        let app = self.app.clone();
        let font_size = context.font_size();
        let content = match content {
            LinkLabel::Code(code) => div()
                .font_family(
                    gpui_kit::base::Theme::global(cx)
                        .tokens
                        .typography
                        .mono
                        .clone(),
                )
                .child(code.clone()),
            LinkLabel::Text(parts) => {
                div()
                    .flex()
                    .items_center()
                    .children(parts.iter().enumerate().map(|(index, part)| {
                        match part {
                            LinkLabelPart::Text(text) => div()
                                .min_w_0()
                                .whitespace_normal()
                                .child(text.clone())
                                .into_any_element(),
                            LinkLabelPart::Emoji { label, path } => div()
                                .id(("embed-link-emoji", index))
                                .role(Role::Image)
                                .flex_shrink_0()
                                .aria_label(label.clone())
                                .child(img(path.clone()).size(font_size * 1.2))
                                .into_any_element(),
                        }
                    }))
            }
        };
        Some(InlineElement::new(
            Button::new(("embed-description-link", offset))
                .link()
                .tab_stop(true)
                .max_w(self.max_width)
                .h_auto()
                .p_0()
                .text_color(cx.theme().primary)
                .accessibility_label(label)
                .child(
                    content
                        .text_size(font_size)
                        .line_height(context.line_height()),
                )
                .on_click(move |_, _, cx| search::activate_chat_link(&app, url.clone(), cx)),
        ))
    }
}

impl Spk {
    pub(super) fn webhook_embed(
        &self,
        embed: &ChatEmbed,
        description: Option<TextView>,
        avatar: Option<Arc<RenderImage>>,
        now: i64,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let color = match embed.color {
            Some(EmbedColor::Success) => theme.success,
            Some(EmbedColor::Error) => theme.danger,
            Some(EmbedColor::Warning) => theme.warning,
            Some(EmbedColor::Info) => theme.info,
            Some(EmbedColor::Neutral) | None => theme.border,
        };
        let timestamp = embed
            .timestamp
            .as_deref()
            .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok());
        let footer = match (embed.footer.as_deref(), timestamp) {
            (Some(footer), Some(time)) => Some(format!(
                "{footer} · {}",
                relative_time(time.timestamp_millis(), now)
            )),
            (None, Some(time)) => Some(relative_time(time.timestamp_millis(), now)),
            (footer, None) => footer.map(str::to_owned),
        };

        let content = div()
            .relative()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_1()
            .child(measure_content_width(self.webhook_content_width.clone()))
            .when_some(embed.title.as_deref(), |card, title| {
                if let Some(link) = embed.title_link() {
                    let app = cx.weak_entity();
                    card.child(
                        Button::new("embed-title")
                            .link()
                            .self_start()
                            .max_w_full()
                            .h_auto()
                            .font_semibold()
                            .tab_stop(true)
                            .accessibility_label(title.to_owned())
                            .child(Label::new(title.to_owned()).text_color(theme.primary))
                            .tooltip(title.to_owned())
                            .on_click(move |_, _, cx| {
                                search::activate_chat_link(&app, link.clone(), cx);
                            }),
                    )
                } else {
                    card.child(Label::new(title.to_owned()).font_semibold())
                }
            })
            .children(description)
            .when(!embed.fields.is_empty(), |card| {
                let app = cx.weak_entity();
                let gap = window.rem_size();
                let minimum = gap * 11.;
                let width = self.webhook_content_width.get();
                let columns = f32::from((width + gap) / (minimum + gap)).floor().max(1.) as u16;
                card.child(
                    div()
                        .grid()
                        .grid_cols(columns)
                        .gap_x(gap)
                        .gap_y_1()
                        .children(embed.fields.iter().enumerate().map(|(index, field)| {
                            let link = field.link();
                            let full_row = !field.inline
                                || field.value.encode_utf16().count() > 40
                                || link.is_some();
                            let element = div()
                                .id(("embed-field", index))
                                .min_w_0()
                                .when(full_row, |field| field.col_span_full())
                                .flex()
                                .flex_col()
                                .child(
                                    Label::new(field.name.clone())
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(theme.muted_foreground),
                                );
                            if let Some(link) = link {
                                let app = app.clone();
                                element.child(
                                    Button::new("embed-field-link")
                                        .link()
                                        .w_full()
                                        .h_auto()
                                        .tab_stop(true)
                                        .accessibility_label(field.value.clone())
                                        .child(
                                            div()
                                                .w_full()
                                                .min_w_0()
                                                .whitespace_normal()
                                                .text_color(theme.primary)
                                                .child(field.value.clone()),
                                        )
                                        .on_click(move |_, _, cx| {
                                            search::activate_chat_link(&app, link.clone(), cx);
                                        }),
                                )
                            } else {
                                element.child(Label::new(field.value.clone()))
                            }
                        })),
                )
            })
            .when_some(footer, |card, footer| {
                card.child(
                    div()
                        .id("embed-footer")
                        .when_some(timestamp, |line, time| {
                            let exact = time
                                .with_timezone(&Local)
                                .format("%b %-d, %Y, %H:%M:%S")
                                .to_string();
                            line.tooltip(move |window, cx| {
                                floating_tooltip(Tooltip::new(exact.clone()), cx).build(window, cx)
                            })
                        })
                        .child(
                            Label::new(footer)
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        ),
                )
            })
            .when_some(embed.author(), |card, author| {
                let name = author.name.clone();
                let mut line = div().h(px(20.)).flex().items_center().gap_1p5();
                if let Some(avatar) = avatar {
                    line = line.child(
                        img(avatar)
                            .size(px(20.))
                            .flex_shrink_0()
                            .rounded_full()
                            .object_fit(ObjectFit::Cover),
                    );
                }
                if let Some(url) = author.link() {
                    let app = cx.weak_entity();
                    line = line.child(
                        Button::new("embed-author")
                            .link()
                            .tab_stop(true)
                            .min_w_0()
                            .h_auto()
                            .p_0()
                            .text_xs()
                            .font_semibold()
                            .accessibility_label(name.clone())
                            .child(Label::new(name).text_color(theme.primary))
                            .on_click(move |_, _, cx| {
                                search::activate_chat_link(&app, url.clone(), cx);
                            }),
                    );
                } else {
                    line = line.child(Label::new(name).text_xs().font_semibold());
                }
                card.child(line)
            });

        div()
            .mt_1()
            .w_full()
            .max_w(rems(48.))
            .rounded_md()
            .border_l_4()
            .border_color(color)
            .bg(theme.popover)
            .px_3()
            .py_2()
            .text_sm()
            .child(content)
    }
}
