use std::{cell::RefCell, ops::Range};

use gpui_kit::gpui::App;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use uuid::Uuid;

use super::{Format, Projection};

struct RichClipboard {
    clipboard: arboard::Clipboard,
    copied: Option<(String, String)>,
}

thread_local! {
    static CLIPBOARD: RefCell<Option<RichClipboard>> = const { RefCell::new(None) };
}

fn with_clipboard<T>(cx: &App, action: impl FnOnce(&mut RichClipboard) -> Option<T>) -> Option<T> {
    // shortcut: fall back to GPUI on native Wayland until its clipboard supports text/html.
    if cfg!(target_os = "linux") && cx.compositor_name() != "X11" {
        return None;
    }
    CLIPBOARD.with_borrow_mut(|state| {
        if state.is_none() {
            *state = Some(RichClipboard {
                clipboard: arboard::Clipboard::new().ok()?,
                copied: None,
            });
        }
        action(state.as_mut()?)
    })
}

pub(super) fn write(
    markdown: &str,
    plain: &str,
    mentions: Vec<(Range<usize>, String)>,
    cx: &App,
) -> Option<()> {
    with_clipboard(cx, |state| {
        let nonce = Uuid::new_v4();
        let marker = format!("<!--spk-copy:{nonce}-->");
        let rendered = rich_html(markdown, mentions)?;
        state
            .clipboard
            .set_html(format!("{marker}{rendered}"), Some(plain.to_owned()))
            .ok()?;
        state.copied = Some((marker, markdown.to_owned()));
        Some(())
    })
}

pub(super) fn read(plain: Option<&str>, cx: &App) -> Option<String> {
    with_clipboard(cx, |state| {
        let html = state.clipboard.get().html().ok()?;
        // GPUI already read the plain representation. Reject HTML if ownership changed.
        if let Some(plain) = plain
            && state.clipboard.get_text().ok()?.replace("\r\n", "\n") != plain.replace("\r\n", "\n")
        {
            return None;
        }
        if let Some((marker, markdown)) = &state.copied
            && html.contains(marker)
        {
            return Some(markdown.clone());
        }
        rich_markdown(&html)
    })
}

fn rich_html(markdown: &str, mentions: Vec<(Range<usize>, String)>) -> Option<String> {
    let mut edits = Vec::new();
    for (range, label) in mentions {
        let id = mention_id(&markdown[range.clone()])?;
        let label = escape_html(label.strip_prefix('@').unwrap_or(&label));
        edits.push((range, format!(
            r#"<span data-type="mention" class="mention" data-id="{id}" data-label="{label}" data-mention-suggestion-char="@">@{label}</span>"#
        )));
    }
    for mark in Projection::parse(markdown).marks {
        if mark.format == Format::Spoiler {
            edits.push((mark.open, "<span data-spoiler>".into()));
            edits.push((mark.close, "</span>".into()));
        }
    }
    // Use punctuation at both ends so Markdown emphasis still sees the same boundaries.
    let nonce = Uuid::new_v4();
    let mut source = markdown.to_owned();
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let replacements: Vec<_> = edits
        .into_iter()
        .enumerate()
        .map(|(index, (range, html))| {
            let placeholder = format!("§{nonce}-{index}§");
            source.replace_range(range, &placeholder);
            (placeholder, html)
        })
        .collect();
    let mut options = ::markdown::Options::gfm();
    options.parse.constructs.html_flow = false;
    options.parse.constructs.html_text = false;
    options.parse.constructs.gfm_autolink_literal = false;
    let mut html = ::markdown::to_html_with_options(&source, &options).ok()?;
    for (placeholder, replacement) in replacements {
        html = html.replace(&placeholder, &replacement);
    }
    Some(html)
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn mention_id(text: &str) -> Option<&str> {
    let id = text.strip_prefix("<@")?.strip_suffix('>')?;
    valid_id(id).then_some(id)
}

fn rich_markdown(html: &str) -> Option<String> {
    if html.len() > 1024 * 1024 {
        return None;
    }
    let dom = html5ever::parse_document(RcDom::default(), Default::default()).one(html);
    let markdown = markdown_node(&dom.document, 0)?;
    let markdown = markdown.trim_matches('\n');
    (!markdown.is_empty()).then(|| markdown.to_owned())
}

fn markdown_node(node: &Handle, depth: usize) -> Option<String> {
    if depth > 64 {
        return None;
    }
    if let NodeData::Text { contents } = &node.data {
        let text = contents.borrow();
        return Some(
            text.chars()
                .flat_map(|ch| {
                    matches!(
                        ch,
                        '\\' | '*'
                            | '_'
                            | '~'
                            | '`'
                            | '['
                            | ']'
                            | '|'
                            | '>'
                            | '<'
                            | '#'
                            | '+'
                            | '-'
                            | '!'
                            | '.'
                    )
                    .then_some('\\')
                    .into_iter()
                    .chain(std::iter::once(ch))
                })
                .collect(),
        );
    }
    let children = node.children.borrow();
    let content = || {
        children
            .iter()
            .map(|child| markdown_node(child, depth + 1))
            .collect::<Option<String>>()
    };
    let NodeData::Element { name, attrs, .. } = &node.data else {
        return content();
    };
    let attrs = attrs.borrow();
    let attr = |name: &str| {
        attrs
            .iter()
            .find(|attribute| attribute.name.local.as_ref() == name)
            .map(|attribute| attribute.value.as_ref())
    };
    let tag = name.local.as_ref();
    let render = match tag {
        "head" | "script" | "style" | "template" | "noscript" | "svg" | "iframe" | "object"
        | "img" => String::new(),
        "span" if attr("data-type") == Some("mention") => {
            if let Some(id) = attr("data-id").filter(|id| valid_id(id)) {
                format!("<@{id}>")
            } else {
                content()?
            }
        }
        "span" if attr("data-spoiler").is_some() => format!("||{}||", content()?),
        "strong" | "b" => format!("**{}**", content()?),
        "em" | "i" => format!("_{}_", content()?),
        "s" | "del" | "strike" => format!("~~{}~~", content()?),
        "p" | "div" => format!("{}\n\n", content()?),
        "br" => "\n".into(),
        "blockquote" => {
            let body = content()?;
            let mut quote = String::new();
            for line in body.trim_end_matches('\n').split('\n') {
                quote.push_str("> ");
                quote.push_str(line);
                quote.push('\n');
            }
            quote.push('\n');
            quote
        }
        "code" => {
            let text = literal_text(node, depth)?;
            let fence = backticks(&text, 1);
            let pad = if text.starts_with('`') || text.ends_with('`') {
                " "
            } else {
                ""
            };
            format!("{fence}{pad}{text}{pad}{fence}")
        }
        "pre" => {
            let lang = children
                .iter()
                .find_map(|child| {
                    let NodeData::Element { name, attrs, .. } = &child.data else {
                        return None;
                    };
                    if name.local.as_ref() != "code" {
                        return None;
                    }
                    attrs
                        .borrow()
                        .iter()
                        .find(|attr| attr.name.local.as_ref() == "class")?
                        .value
                        .split_ascii_whitespace()
                        .find_map(|class| class.strip_prefix("language-"))
                        .filter(|lang| valid_id(lang))
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            let text = literal_text(node, depth)?;
            let fence = backticks(&text, 3);
            let text = text.strip_suffix('\n').unwrap_or(&text);
            format!("{fence}{lang}\n{text}\n{fence}\n\n")
        }
        "ul" | "ol" => {
            let mut items = String::new();
            let start = attr("start")
                .and_then(|start| start.parse::<u32>().ok())
                .unwrap_or(1);
            for (index, child) in children.iter().filter(|child| {
                matches!(&child.data, NodeData::Element { name, .. } if name.local.as_ref() == "li")
            }).enumerate() {
                let prefix = if tag == "ul" { "- ".to_owned() } else { format!("{}. ", u64::from(start) + index as u64) };
                let body = markdown_node(child, depth + 1)?;
                for (line_index, line) in body.trim_matches('\n').split('\n').enumerate() {
                    if line_index == 0 {
                        items.push_str(&prefix);
                    } else if !line.is_empty() {
                        items.push_str(&" ".repeat(prefix.len()));
                    }
                    items.push_str(line);
                    items.push('\n');
                }
            }
            items.push('\n');
            items
        }
        "a" => {
            let text = content()?;
            if let Some(url) = attr("href").and_then(crate::chat_markdown::safe_link) {
                format!("[{text}](<{}>)", url.as_str().replace('>', "%3E"))
            } else {
                text
            }
        }
        _ => content()?,
    };
    Some(render)
}

fn backticks(text: &str, minimum: usize) -> String {
    let longest = text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(minimum.max(longest + 1))
}

fn literal_text(node: &Handle, depth: usize) -> Option<String> {
    if depth > 64 {
        return None;
    }
    if let NodeData::Text { contents } = &node.data {
        return Some(contents.borrow().to_string());
    }
    if matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == "br") {
        return Some("\n".into());
    }
    node.children
        .borrow()
        .iter()
        .map(|child| literal_text(child, depth + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_selection_preserves_marks_and_explicit_mention_identity() {
        let html = r#"<p data-pm-slice="1 1 []"><strong>Hi</strong> <span data-spoiler><span data-type="mention" class="mention" data-id="bob_1" data-label="Bób Fixture" data-mention-suggestion-char="@">@Bób Fixture</span></span> 👋 <em>there</em> <code>&lt;@literal&gt;</code></p>"#;
        assert_eq!(
            rich_markdown(html).as_deref(),
            Some("**Hi** ||<@bob_1>|| 👋 _there_ `<@literal>`"),
        );
    }
    #[test]
    fn desktop_selection_exports_web_mention_with_an_escaped_nickname() {
        let source = "Hi ||**<@bob_1>**|| 👋 and `<@literal>`";
        let html = rich_html(source, vec![(7..15, "Bób & <friends>".to_owned())]).unwrap();
        assert!(html.contains(r#"<span data-spoiler><strong><span data-type="mention" class="mention" data-id="bob_1" data-label="Bób &amp; &lt;friends&gt;" data-mention-suggestion-char="@">@Bób &amp; &lt;friends&gt;</span></strong></span>"#));
        assert!(html.contains("<code>&lt;@literal&gt;</code>"));
        assert_eq!(rich_markdown(&html).as_deref(), Some(source));
    }
}
