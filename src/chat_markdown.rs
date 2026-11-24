use std::{borrow::Cow, cmp::Reverse, collections::HashSet, ops::Range};

use ::markdown::{ParseOptions, mdast::Node};
use url::Url;

use crate::api::image_message_url;

pub(crate) fn valid_mention_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(mention_id_byte)
}

fn mention_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

pub(crate) fn mention_at(source: &str) -> Option<&str> {
    let rest = source.strip_prefix("<@")?;
    let len = rest
        .bytes()
        .take_while(|byte| mention_id_byte(*byte))
        .count();
    (len > 0 && rest.as_bytes().get(len) == Some(&b'>')).then(|| &rest[..len])
}

pub(crate) fn message_markdown<'a, 'b>(
    source: &'a str,
    name_of: impl Fn(&str) -> &'b str,
    revealed: impl Fn(usize) -> bool,
    spoiler_link: Option<&str>,
) -> Cow<'a, str> {
    if !source.contains("<@")
        && !source.contains("||")
        && !source.contains("\\|")
        && !source.contains("![")
        && (spoiler_link.is_none() || !source.contains('\n'))
    {
        return Cow::Borrowed(source);
    }
    let mut options = parse_options();
    // Scan spoiler delimiters before bare URLs can absorb adjacent pipes.
    options.constructs.gfm_autolink_literal = false;
    let Ok(document) = ::markdown::to_mdast(source, &options) else {
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

pub(crate) fn reply_preview<'a>(source: &str, name_of: impl Fn(&str) -> &'a str) -> String {
    const LIMIT: usize = 80;
    let line = source
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .replace(r"\|", "|");
    let mut preview = String::new();
    for (index, part) in line.split("||").enumerate() {
        if index % 2 != 0 {
            // Mask an unclosed span as well, including spoilers spanning lines.
            preview.push_str("▒▒▒");
            continue;
        }
        let mut parts = part.split("<@");
        preview.push_str(parts.next().unwrap_or_default());
        for token in parts {
            if let Some((id, tail)) = token.split_once('>')
                && valid_mention_id(id)
            {
                preview.push('@');
                preview.push_str(name_of(id));
                preview.push_str(tail);
            } else {
                preview.push_str("<@");
                preview.push_str(token);
            }
        }
    }
    let mut units = 0;
    if let Some((end, _)) = preview.char_indices().find(|(_, ch)| {
        units += ch.len_utf16();
        units > LIMIT
    }) {
        preview.truncate(end);
    }
    preview
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
    let mut spoiler: Option<Range<usize>> = None;
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
                    if i < end {
                        continue;
                    }
                    let escaped = text.as_bytes()[..i]
                        .iter()
                        .rev()
                        .take_while(|&&b| b == b'\\')
                        .count()
                        % 2
                        != 0;
                    let start = range.start + i;
                    if ch == "|" {
                        let width = if text[i..].starts_with("||") {
                            2
                        } else if text[i..].starts_with("|\\|") {
                            3
                        } else {
                            continue;
                        };
                        end = i + width;
                        // The web renderer pairs pipes after the Markdown parser decodes escapes.
                        let marker = start - usize::from(escaped)..range.start + end;
                        if let Some(open) = spoiler.take() {
                            if source[open.end..marker.start].trim().is_empty() {
                                continue;
                            }
                            if !preview && revealed(open.start) {
                                edits.push((open, String::new()));
                                edits.push((marker, String::new()));
                            } else {
                                let label = match spoiler_link {
                                    Some(link) => format!("[Reveal spoiler]({link}{})", open.start),
                                    None => "▒▒▒".into(),
                                };
                                edits.push((open.start..marker.end, label));
                            }
                        } else {
                            spoiler = Some(marker);
                        }
                    } else if escaped {
                        continue;
                    } else if ch == "\n" && !preview {
                        let offset = start
                            - usize::from(
                                source.as_bytes().get(start.wrapping_sub(1)) == Some(&b'\r'),
                            );
                        edits.push((offset..offset, "  ".into()));
                    } else if let Some(id) = mention_at(&source[start..]) {
                        end = i + id.len() + 3;
                        let name = name_of(id);
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
        edits.push((open.start..source.len(), "▒▒▒".into()));
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

pub(crate) fn image_preview_links(source: &str) -> Vec<Url> {
    if !source.contains("://") {
        return Vec::new();
    }
    let Ok(document) = ::markdown::to_mdast(source, &parse_options()) else {
        return Vec::new();
    };
    let mut links = Vec::new();
    collect_image_links(&document, source, &mut HashSet::new(), &mut links);
    links
}

fn collect_image_links(node: &Node, source: &str, seen: &mut HashSet<Url>, links: &mut Vec<Url>) {
    if let Node::Link(link) = node
        && let Some(position) = &link.position
        && source.as_bytes().get(position.start.offset) != Some(&b'<')
        && let Some(url) = image_message_url(&link.url)
        && seen.insert(url.clone())
    {
        links.push(url);
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_image_links(child, source, seen, links);
        }
    }
}

pub(crate) fn safe_link(raw: &str) -> Option<url::Url> {
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

pub(crate) fn plain_text(source: &str) -> String {
    ::markdown::to_mdast(source, &parse_options()).map_or_else(
        |_| source.to_owned(),
        |document| node_text(&document, false),
    )
}

pub(crate) fn notification_preview<'a>(
    source: &str,
    has_attachment: bool,
    name_of: impl Fn(&str) -> &'a str,
) -> String {
    let body = message_markdown(source, name_of, |_| false, None);
    let text = ::markdown::to_mdast(&body, &parse_options())
        .map_or_else(|_| String::new(), |document| node_text(&document, true))
        .replace("▒▒▒", "[spoiler]")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() && has_attachment {
        return "Sent an attachment".into();
    }
    if let Some((limit, _)) = text.char_indices().nth(140) {
        let end = text[..limit].char_indices().next_back().unwrap().0;
        format!("{}…", text[..end].trim_end())
    } else {
        text
    }
}

fn node_text(node: &Node, hide_code: bool) -> String {
    if hide_code {
        match node {
            Node::Code(_) => return "[code]".into(),
            Node::InlineCode(code) => {
                return code.value.split("||").enumerate().fold(
                    String::new(),
                    |mut text, (index, part)| {
                        text.push_str(if index % 2 == 0 { part } else { "[spoiler]" });
                        text
                    },
                );
            }
            _ => {}
        }
    }
    if let Some(children) = node.children() {
        let separator = match node {
            Node::Root(_) | Node::Blockquote(_) | Node::ListItem(_) => "\n\n",
            Node::List(_) | Node::Table(_) => "\n",
            Node::TableRow(_) => "\t",
            _ => "",
        };
        children
            .iter()
            .map(|child| node_text(child, hide_code))
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
    fn image_previews_follow_visible_markdown_and_respect_autolink_opt_out() {
        let png = "https://media.example.com/cat.PNG?download=1";
        let jpeg = "https://media.example.com/cat.jpeg";
        let webp = "https://media.example.com/cat.webp";
        let gif = "https://media.example.com/cat.gif";
        let render = |body: &str, revealed| {
            message_markdown(body, |_| "", |_| revealed, Some("spk-spoiler:test:")).into_owned()
        };
        let body = format!(
            "A cat {png} beside [a JPEG]({jpeg}) and {webp}\n\n<{gif}>\n\n`{gif}`\n\n```\n{gif}\n```\n\n||{gif}||"
        );
        let links = |body: &str, revealed| {
            image_preview_links(&render(body, revealed))
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(links(&body, false), [png, jpeg, webp]);
        assert_eq!(links(&body, true), [png, jpeg, webp, gif]);
        assert!(links(r"\||https://media.example.com/cat.png?secret=1||", false).is_empty());
        assert_eq!(render(&format!("||{gif}||"), true), gif);
        assert!(!render(&format!("||{gif}||"), false).contains(gif));
        assert!(links("https://127.0.0.1/cat.png", false).is_empty());
        assert!(links("https://media.example.com/code.svg", false).is_empty());
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
    fn escaped_spoilers_stay_hidden_in_messages_and_notifications() {
        for body in [r"Visible \||private||", r"Visible \|\|private|\|"] {
            assert_eq!(
                message_markdown(body, |_| "", |_| false, Some("spk-spoiler:test:")),
                "Visible [Reveal spoiler](spk-spoiler:test:8)"
            );
            assert_eq!(
                message_markdown(body, |_| "", |_| true, Some("spk-spoiler:test:")),
                "Visible private"
            );
            assert_eq!(
                notification_preview(body, false, |_| ""),
                "Visible [spoiler]"
            );
        }
    }

    #[test]
    fn notification_preview_hides_spoilers_and_code_and_limits_unicode() {
        let body = "> **Hi** <@alice> 👋 ||keep **this** private||\n\n```rs\nprivate_code()\n```\n\n[Public label](https://example.test/path)";
        assert_eq!(
            notification_preview(body, false, |_| "Alice"),
            "Hi @Alice 👋 [spoiler] [code] Public label"
        );
        assert_eq!(
            notification_preview(" ", true, |_| "Alice"),
            "Sent an attachment"
        );
        let long = "é".repeat(141);
        assert_eq!(
            notification_preview(&long, false, |_| "Alice"),
            format!("{}…", "é".repeat(139))
        );
        assert_eq!(
            notification_preview("Visible ||unfinished spoiler", false, |_| "Alice"),
            "Visible [spoiler]"
        );
        assert_eq!(
            notification_preview("```rs\nunclosed_code()", false, |_| "Alice"),
            "[code]"
        );
        assert_eq!(
            notification_preview("`||inline spoiler||`", false, |_| "Alice"),
            "[spoiler]"
        );
    }

    #[test]
    fn composer_reply_preview_masks_spoilers_and_limits_visible_text() {
        let body = format!(
            " \n\n<@alice> ||keep **this** private|| {}\nsecond line",
            "😀".repeat(40)
        );
        let preview = reply_preview(&body, |_| "Alice");
        assert_eq!(preview, format!("@Alice ▒▒▒ {}", "😀".repeat(34)));

        let body = format!("{}😀 tail", "a".repeat(79));
        assert_eq!(reply_preview(&body, |_| "Alice"), "a".repeat(79));

        let body = format!(" <@lee> ||{}|| visible", "secret".repeat(30));
        assert_eq!(reply_preview(&body, |_| "Renamed"), " @Renamed ▒▒▒ visible");
        assert_eq!(
            reply_preview(
                r"`<@x_1>` \|\|hidden | secret|\| <@> <@not valid>",
                |_| "Lee"
            ),
            "`@Lee` ▒▒▒ <@> <@not valid>",
        );
        assert_eq!(
            reply_preview("Visible ||first line\nsecond line||", |_| "Alice"),
            "Visible ▒▒▒",
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
