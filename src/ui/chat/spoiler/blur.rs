use std::{collections::BTreeMap, fmt::Write as _, ops::Range};

use gpui_kit::base::text::InlineRenderContext;
use markdown::mdast::Node;

use super::*;

struct TextSpan {
    bytes: Range<usize>,
    style: TextRun,
}

#[derive(Default)]
struct SpoilerText {
    text: String,
    spans: Vec<TextSpan>,
}

impl SpoilerText {
    fn push(&mut self, text: &str, style: &TextRun) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        let mut style = style.clone();
        style.len = text.len();
        self.spans.push(TextSpan {
            bytes: start..self.text.len(),
            style,
        });
    }

    fn collect(&mut self, node: &Node, inherited: &TextRun, colors: &SpoilerColors) {
        let mut style = inherited.clone();
        match node {
            Node::Text(text) => self.push(&text.value, &style),
            Node::Break(_) => self.push("\n", &style),
            Node::InlineCode(code) => {
                style.font.family = "monospace".into();
                style.background_color = Some(colors.code_background);
                self.push(&code.value, &style);
            }
            Node::Image(image)
                if matches!(image.url.as_str(), "spk-mention:self" | "spk-mention:other") =>
            {
                style.font.weight = FontWeight::MEDIUM;
                style.color = colors.link;
                style.background_color = Some(if image.url == "spk-mention:self" {
                    colors.link.opacity(0.2)
                } else {
                    colors.code_background
                });
                self.push(&format!(" {} ", image.alt), &style);
            }
            Node::Definition(_) => {}
            _ => {
                match node {
                    Node::Strong(_) => style.font.weight = FontWeight::BOLD,
                    Node::Emphasis(_) => style.font.style = FontStyle::Italic,
                    Node::Delete(_) => style.strikethrough = Some(StrikethroughStyle::default()),
                    Node::Link(_) | Node::LinkReference(_) => {
                        style.color = colors.link;
                        style.underline = Some(UnderlineStyle::default());
                    }
                    _ => {}
                }
                if let Some(children) = node.children() {
                    for child in children {
                        self.collect(child, &style, colors);
                    }
                }
            }
        }
    }
}

struct SpoilerColors {
    link: Hsla,
    code_background: Hsla,
}

fn xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(super) fn formatted_spoiler(
    markdown: &str,
    context: &InlineRenderContext,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let colors = SpoilerColors {
        link: theme.primary,
        code_background: theme.popover,
    };
    let mut formatted = SpoilerText::default();
    let mut options = markdown::ParseOptions::gfm();
    options.constructs.html_flow = false;
    options.constructs.html_text = false;
    if let Ok(document) = markdown::to_mdast(markdown, &options) {
        formatted.collect(&document, &context.text_style().to_run(0), &colors);
    }
    let font_size = context.font_size();
    let line_height = context.line_height();
    canvas(
        move |bounds, window, _| {
            let runs = formatted.spans.iter().map(|span| span.style.clone()).collect::<Vec<_>>();
            let lines = window.text_system().shape_text(
                formatted.text.into(),
                font_size,
                &runs,
                Some((bounds.size.width - px(4.)).max(px(1.))),
                None,
            ).ok()?;
            let width = f32::from(bounds.size.width).max(1.);
            let height = f32::from(bounds.size.height).max(1.);
            let font_size = f32::from(font_size);
            let line_height = f32::from(line_height);
            let mut backgrounds = BTreeMap::<u32, String>::new();
            let mut glyphs = BTreeMap::<u32, String>::new();
            let mut offset = 0;
            let mut y = font_size;
            for line in &lines {
                let layout = &line.unwrapped_layout;
                let mut row_start = 0;
                let rows = line.wrap_boundaries().iter().map(|boundary| {
                    layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
                }).chain([line.text.len()]);
                for row_end in rows {
                    let first = formatted.spans.partition_point(|span| span.bytes.end <= offset + row_start);
                    for span in formatted.spans[first..].iter()
                        .take_while(|span| span.bytes.start < offset + row_end)
                    {
                        let start = span.bytes.start.saturating_sub(offset).max(row_start);
                        let end = (span.bytes.end - offset).min(row_end);
                        let Some(text) = line.text.get(start..end) else { continue };
                        let advance = |index| f32::from(layout.x_for_index(index) - layout.x_for_index(row_start)) + 2.;
                        let (x, trailing) = (advance(start), advance(end));
                        let font = &span.style.font;
                        let family = if font.family.starts_with('.') { "sans-serif" } else { &font.family };
                        let weight = font.weight;
                        let font_style = if font.style == FontStyle::Normal { "normal" } else { "italic" };
                        let decoration = if span.style.underline.is_some() { "underline" } else if span.style.strikethrough.is_some() { "line-through" } else { "none" };
                        if let Some(color) = span.style.background_color {
                            let top = y - font_size;
                            let left = x.min(trailing);
                            let width = (x - trailing).abs();
                            let layer = backgrounds.entry(Rgba::from(color).into()).or_default();
                            write!(layer, r#"<rect x="{left}" y="{top}" width="{width}" height="{line_height}" rx="2"/>"#).ok()?;
                        }
                        let layer = glyphs.entry(Rgba::from(span.style.color).into()).or_default();
                        write!(layer, r#"<text x="{x}" y="{y}" font-family="{}" font-weight="{weight}" font-style="{font_style}" text-decoration="{decoration}">{}</text>"#, xml_text(family), xml_text(text)).ok()?;
                    }
                    y += line_height;
                    row_start = row_end;
                }
                offset += line.text.len() + 1;
            }
            Some(
                backgrounds.into_iter().chain(glyphs).map(|(color, shapes)| {
                    let svg = format!(
                        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}"><defs><filter id="blur" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="24"/></filter></defs><g filter="url(#blur)" fill="white" font-size="{font_size}" xml:space="preserve">{shapes}</g></svg>"#
                    );
                    let mut hash = DefaultHasher::new();
                    svg.hash(&mut hash);
                    (SharedString::from(format!("spoiler-blur-{}", hash.finish())), svg, Hsla::from(rgba(color)).opacity(0.12))
                }).collect::<Vec<_>>(),
            )
        },
        move |bounds, images, window, cx| {
            for (path, svg, color) in images.into_iter().flatten() {
                let _ = window.paint_svg(bounds, path, Some(svg.as_bytes()), Default::default(), color, cx);
            }
        },
    ).absolute().inset_0()
}
