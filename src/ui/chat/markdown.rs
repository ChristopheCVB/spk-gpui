use std::{borrow::Cow, cmp::Reverse, ops::Range};

use ::markdown::{ParseOptions, mdast::Node};

pub(in crate::ui) fn message_markdown<'a, 'b>(
    source: &'a str,
    name_of: impl Fn(&str) -> &'b str,
    revealed: impl Fn(usize) -> bool,
    spoiler_link: Option<&str>,
) -> Cow<'a, str> {
    if !source.contains("<@")
        && !source.contains("||")
        && !source.contains("![")
        && (spoiler_link.is_none() || !source.contains('\n'))
    {
        return Cow::Borrowed(source);
    }
    let Ok(document) = ::markdown::to_mdast(source, &parse_options()) else {
        return Cow::Borrowed("Message preview unavailable.");
    };
    let mut edits = Vec::new();
    find_edits(
        &document,
        source,
        &name_of,
        &revealed,
        spoiler_link,
        &mut edits,
    );
    edits.sort_by_key(|(range, _)| (range.start, Reverse(range.end)));
    let mut result = String::with_capacity(source.len());
    let mut offset = 0;
    for (range, replacement) in edits {
        if range.start >= offset {
            result.push_str(&source[offset..range.start]);
            result.push_str(&replacement);
            offset = range.end;
        }
    }
    result.push_str(&source[offset..]);
    Cow::Owned(result)
}

fn find_edits<'a>(
    parent: &Node,
    source: &str,
    name_of: &impl Fn(&str) -> &'a str,
    revealed: &impl Fn(usize) -> bool,
    spoiler_link: Option<&str>,
    edits: &mut Vec<(Range<usize>, String)>,
) {
    let Some(children) = parent.children() else {
        return;
    };
    let preview = spoiler_link.is_none();
    let mut spoiler = None;
    for child in children {
        let Some(position) = child.position() else {
            continue;
        };
        let range = position.start.offset..position.end.offset;
        match child {
            Node::Text(_) => {
                let text = &source[range.clone()];
                let mut end = 0;
                for (i, ch) in text.match_indices(['<', '|', '\n']) {
                    if i < end
                        || text.as_bytes()[..i]
                            .iter()
                            .rev()
                            .take_while(|&&b| b == b'\\')
                            .count()
                            % 2
                            != 0
                    {
                        continue;
                    }
                    let start = range.start + i;
                    if ch == "\n" && !preview {
                        let offset = start
                            - usize::from(
                                source.as_bytes().get(start.wrapping_sub(1)) == Some(&b'\r'),
                            );
                        edits.push((offset..offset, "  ".into()));
                    } else if ch == "|" && text[i..].starts_with("||") {
                        end = i + 2;
                        if let Some(open) = spoiler.take() {
                            if source[open + 2..start].trim().is_empty() {
                                continue;
                            }
                            if !preview && revealed(open) {
                                edits.push((open..open + 2, String::new()));
                                edits.push((start..start + 2, String::new()));
                            } else {
                                let label = match spoiler_link {
                                    Some(link) => format!("[Reveal spoiler]({link}{open})"),
                                    None => "▒▒▒".into(),
                                };
                                edits.push((open..start + 2, label));
                            }
                        } else {
                            spoiler = Some(start);
                        }
                    } else if let Some(token) = text[i..].strip_prefix("<@")
                        && let Some(end_id) = token.find(|ch: char| {
                            !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-')
                        })
                        && end_id > 0
                        && token.as_bytes()[end_id] == b'>'
                    {
                        end = i + end_id + 3;
                        let name = name_of(&token[..end_id]);
                        let name = if preview {
                            name.to_owned()
                        } else {
                            escape_markdown(name)
                        };
                        edits.push((start..range.start + end, format!("@{name}")));
                    }
                }
            }
            Node::Image(image) => {
                let label = if image.alt.is_empty() {
                    &image.url
                } else {
                    &image.alt
                };
                let label = escape_markdown(label);
                let link = safe_link(&image.url)
                    .map(|url| format!("[{label}](<{}>)", url.as_str().replace('>', "%3E")));
                edits.push((range, link.unwrap_or(label)));
            }
            Node::ImageReference(image) => {
                edits.push((range, escape_markdown(&image.alt)));
            }
            _ => find_edits(child, source, name_of, revealed, spoiler_link, edits),
        }
    }
    if preview && let Some(open) = spoiler {
        edits.push((open..source.len(), "▒▒▒".into()));
    }
}

fn escape_markdown(text: &str) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        if ch.is_ascii_punctuation() {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

pub(super) fn has_final_gif_link(rendered: &str, url: &str) -> bool {
    let Ok(Node::Root(root)) = ::markdown::to_mdast(rendered, &parse_options()) else {
        return false;
    };
    matches!(root.children.last(), Some(Node::Paragraph(paragraph))
        if matches!(paragraph.children.last(), Some(Node::Link(link)) if link.url == url))
}

pub(super) fn safe_link(raw: &str) -> Option<url::Url> {
    if raw.trim_start().starts_with("//") {
        return None;
    }
    let base = url::Url::parse(env!("SPK_URL")).ok()?;
    let url = base.join(raw).ok()?;
    matches!(url.scheme(), "https" | "http" | "mailto").then_some(url)
}

fn parse_options() -> ParseOptions {
    let mut options = ParseOptions::gfm();
    options.constructs.html_flow = false;
    options.constructs.html_text = false;
    options
}

pub(in crate::ui) fn plain_text(source: &str) -> String {
    ::markdown::to_mdast(source, &parse_options())
        .map_or_else(|_| source.to_owned(), |document| node_text(&document))
}

fn node_text(node: &Node) -> String {
    if let Some(children) = node.children() {
        let separator = match node {
            Node::Root(_) | Node::Blockquote(_) | Node::ListItem(_) => "\n\n",
            Node::List(_) | Node::Table(_) => "\n",
            Node::TableRow(_) => "\t",
            _ => "",
        };
        children
            .iter()
            .map(node_text)
            .collect::<Vec<_>>()
            .join(separator)
    } else if matches!(node, Node::Break(_)) {
        "\n".to_owned()
    } else {
        node.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_preview_follows_visible_markdown() {
        let url = "https://media.klipy.com/cat.gif";
        let render = |body: &str| {
            message_markdown(body, |_| "", |_| false, Some("spk-spoiler:test:")).into_owned()
        };
        assert!(has_final_gif_link(&render(&format!("A cat\n{url}")), url));
        assert!(!has_final_gif_link(&render(&format!("```\n{url}")), url));
        assert!(!has_final_gif_link(
            &render(&format!("||hidden\n{url}||")),
            url
        ));
    }

    #[test]
    fn presentation_keeps_newlines_and_displays_images_as_links() {
        let source = "One\nTwo\n\n![profile](https://example.test/person.png)\n\n<img src='x'>";
        assert_eq!(
            message_markdown(
                source,
                |_| "Unknown user",
                |_| false,
                Some("spk-spoiler:test:")
            ),
            "One  \nTwo\n\n[profile](<https://example.test/person.png>)\n\n<img src='x'>"
        );
        assert!(safe_link("javascript:alert(1)").is_none());
        assert!(safe_link("//example.test/resource").is_none());
    }

    #[test]
    fn presentation_hides_spoilers_and_resolves_mentions_without_changing_code() {
        let body = "> **Hi** <@alice> 👋 ||keep **this** private||\n\n`<@alice> ||literal||`";
        let name = |_: &str| "Alice *here*";
        let hidden = message_markdown(body, name, |_| false, Some("spk-spoiler:test:"));
        assert!(hidden.contains(r"> **Hi** @Alice \*here\* 👋 [Reveal spoiler](spk-spoiler:"));
        assert!(hidden.ends_with("`<@alice> ||literal||`"));
        assert!(!hidden.contains("keep **this** private"));
        assert_eq!(
            message_markdown(body, name, |_| true, Some("spk-spoiler:test:")),
            "> **Hi** @Alice \\*here\\* 👋 keep **this** private\n\n`<@alice> ||literal||`",
        );
        assert_eq!(
            message_markdown("<@alice> ||truncated spoiler", name, |_| false, None),
            "@Alice *here* ▒▒▒",
        );
    }

    #[test]
    fn copied_text_strips_markdown_preserving_readable_line_breaks() {
        assert_eq!(
            plain_text("**First** [link](https://example.test)  \nline\n\n- item A\n- item B"),
            "First link\nline\n\nitem A\nitem B",
        );
    }
}
