use std::{borrow::Cow, ops::Range};

use gpui_kit::base::text::{InlineElement, InlineRenderContext};
use icu_properties::{
    CodePointMapData, CodePointSetData,
    props::{
        EmojiModifier, ExtendedPictographic, GeneralCategory, GeneralCategoryGroup,
        RegionalIndicator,
    },
};
use markdown::mdast::{Node, ReferenceKind, Text};
use unicode_segmentation::UnicodeSegmentation as _;

use super::*;
use crate::chat_markdown::{apply_edits, escape_markdown, markdown_escaped, parse_options};

const EMOJI_URL: &str = "spk-emoji:";

pub(super) fn emoji_only(body: &str) -> bool {
    let mut count = 0;
    for word in body.split_whitespace() {
        for emoji in word.graphemes(true) {
            count += 1;
            if count > 6 || !is_emoji_sequence(emoji) {
                return false;
            }
        }
    }
    count > 0
}

fn is_emoji_sequence(emoji: &str) -> bool {
    let mut chars = emoji.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if matches!(first, '#' | '*' | '0'..='9') {
        return matches!(
            (chars.next(), chars.next(), chars.next()),
            (Some('\u{20e3}'), None, None) | (Some('\u{fe0f}'), Some('\u{20e3}'), None)
        );
    }
    let regional = CodePointSetData::new::<RegionalIndicator>();
    if regional.contains(first) {
        return chars.next().is_some_and(|ch| regional.contains(ch)) && chars.next().is_none();
    }
    let pictographic = CodePointSetData::new::<ExtendedPictographic>();
    if !pictographic.contains(first) {
        return false;
    }
    let modifier = CodePointSetData::new::<EmojiModifier>();
    let mut chars = chars.peekable();
    loop {
        while chars
            .next_if(|&ch| ch == '\u{fe0f}' || modifier.contains(ch))
            .is_some()
        {}
        match chars.next() {
            None => return true,
            Some('\u{200d}') if chars.next().is_some_and(|ch| pictographic.contains(ch)) => {}
            Some('\u{e0020}'..='\u{e007e}') => {
                while chars
                    .next_if(|ch| matches!(ch, '\u{e0020}'..='\u{e007e}'))
                    .is_some()
                {}
                return chars.next() == Some('\u{e007f}') && chars.next().is_none();
            }
            _ => return false,
        }
    }
}

pub(super) struct MessageEmoji {
    large: bool,
}

enum EmojiPresentation {
    Artwork(String),
    Unicode,
}

impl MessageEmoji {
    pub(super) fn new(large: bool) -> Self {
        Self { large }
    }

    pub(super) fn display_markdown(&self, source: &SharedString) -> SharedString {
        if source.is_ascii() && !source.contains('&') {
            return source.clone();
        }
        let mut options = parse_options();
        // GPUI Kit renders dollar-delimited blocks as code.
        options.constructs.math_flow = true;
        let Ok(document) = markdown::to_mdast(source, &options) else {
            return source.clone();
        };
        // GPUI Kit's plugin receives one AST node at a time. Use display-only image
        // syntax to split mixed text into individual objects while keeping its Markdown marks.
        let mut edits = Vec::new();
        self.find_emoji(&document, source, &mut edits);
        match apply_edits(source, edits) {
            Cow::Borrowed(_) => source.clone(),
            Cow::Owned(display) => {
                // shortcut: keep the original formatting when unusual delimiter runs
                // cannot be rewritten without changing their parse. Remove once GPUI Kit
                // can split Markdown text nodes into inline objects directly.
                if source.contains(['*', '_', '~']) && !same_markdown(source, &display, options) {
                    source.clone()
                } else {
                    display.into()
                }
            }
        }
    }

    fn find_emoji(&self, node: &Node, source: &str, edits: &mut Vec<(Range<usize>, String)>) {
        let Some(position) = node.position() else {
            return;
        };
        let range = position.start.offset..position.end.offset;
        let raw = &source[range.clone()];
        if let Node::Text(text) = node {
            if !text.value.is_ascii() && !is_emoji_sequence(&text.value) {
                self.text_edits(source, range, edits);
            }
            return;
        }
        if let Node::Link(link) = node
            && !raw.starts_with('[')
        {
            // Autolinks share source with their label, so create an explicit link before
            // inserting glyphs in the label. The URL is still handled by the normal link policy.
            let label = link
                .children
                .iter()
                .map(ToString::to_string)
                .collect::<String>();
            let mut rendered = String::new();
            let mut changed = false;
            for part in label.graphemes(true) {
                if self.presentation(part).is_some() {
                    rendered.push_str(&emoji_marker(part));
                    changed = true;
                } else {
                    rendered.push_str(&escape_markdown(part));
                }
            }
            if changed {
                let url = escape_markdown(&link.url);
                edits.push((range, format!("[{rendered}](<{url}>)")));
            }
            return;
        }
        let Some(children) = node.children() else {
            return;
        };
        let previous_edits = edits.len();
        for (index, child) in children.iter().enumerate() {
            let first_edit = edits.len();
            self.find_emoji(child, source, edits);
            if let [first, ..] = &edits[first_edit..]
                && matches!(child, Node::Emphasis(_) | Node::Strong(_) | Node::Delete(_))
                && let Some(position) = child.position()
            {
                let start = position.start.offset;
                let end = position.end.offset;
                let delimiter = source.as_bytes()[start];
                let opening = source.as_bytes()[start..end]
                    .iter()
                    .take_while(|&&byte| byte == delimiter)
                    .count();
                let closing = source.as_bytes()[start..end]
                    .iter()
                    .rev()
                    .take_while(|&&byte| byte == delimiter)
                    .count();
                let touches_opening = first.0.start == start + opening;
                let touches_closing = edits[first_edit..]
                    .iter()
                    .any(|edit| edit.0.end == end - closing);
                // Entity escapes keep the neighboring text while giving CommonMark
                // punctuation on both sides of a delimiter next to an image marker.
                let before = children.get(index.wrapping_sub(1));
                let after = children.get(index + 1);
                if touches_opening {
                    escape_neighbor(source, start, false, before, edits);
                }
                if touches_closing {
                    escape_neighbor(source, end, true, after, edits);
                }
                if touches_opening || touches_closing {
                    if let Some(Node::Text(text)) = before
                        && let Some(span) = &text.position
                    {
                        escape_literal_delimiters(source, span.start.offset..start, false, edits);
                    }
                    if let Some(Node::Text(text)) = after
                        && let Some(span) = &text.position
                    {
                        escape_literal_delimiters(source, end..span.end.offset, true, edits);
                    }
                }
            }
        }
        if let Node::LinkReference(link) = node
            && link.reference_kind != ReferenceKind::Full
            && edits.len() > previous_edits
        {
            let label_end = if link.reference_kind == ReferenceKind::Collapsed {
                range.end - 2
            } else {
                range.end
            };
            edits.push((
                label_end..range.end,
                source[range.start..label_end].to_owned(),
            ));
        }
    }

    fn text_edits(
        &self,
        source: &str,
        range: Range<usize>,
        edits: &mut Vec<(Range<usize>, String)>,
    ) {
        let raw = &source[range.clone()];
        let decoded = raw
            .contains('&')
            .then(|| decode_references(source, range.clone()));
        let (text, offsets) = decoded.as_ref().map_or((raw, None), |(text, offsets)| {
            (text.as_str(), Some(offsets))
        });
        for (offset, emoji) in text.grapheme_indices(true) {
            if self.presentation(emoji).is_none() {
                continue;
            }
            let start = range.start + offsets.map_or(offset, |offsets| offsets[offset]);
            let end = offset + emoji.len();
            let end = range.start + offsets.map_or(end, |offsets| offsets[end]);
            if end <= start {
                continue;
            }
            let escaped = markdown_escaped(&source[..start]);
            let mut replacement = emoji_marker(emoji);
            if escaped && !emoji.as_bytes()[0].is_ascii_punctuation() {
                replacement.insert_str(0, "&#92;");
            }
            edits.push((start - usize::from(escaped)..end, replacement));
            escape_literal_delimiters(source, range.start..start, false, edits);
            escape_literal_delimiters(source, end..range.end, true, edits);
        }
    }

    fn presentation(&self, emoji: &str) -> Option<EmojiPresentation> {
        if !is_emoji_sequence(emoji) {
            return None;
        }
        match reaction_artwork_path(emoji) {
            Some(path) => Some(EmojiPresentation::Artwork(path)),
            None if self.large => Some(EmojiPresentation::Unicode),
            None => None,
        }
    }
}

fn same_markdown(source: &str, display: &str, parse: markdown::ParseOptions) -> bool {
    let options = markdown::Options {
        parse,
        compile: markdown::CompileOptions {
            // This HTML is compared in memory, so preserve the marker URL for comparison.
            allow_dangerous_protocol: true,
            ..Default::default()
        },
    };
    let (Ok(original), Ok(projected)) = (
        markdown::to_html_with_options(source, &options),
        markdown::to_html_with_options(display, &options),
    ) else {
        return false;
    };
    let marker = "<img src=\"spk-emoji:\" alt=\"";
    let mut normalized = String::with_capacity(projected.len());
    let mut rest = projected.as_str();
    while let Some((before, image)) = rest.split_once(marker) {
        let Some((emoji, after)) = image.split_once("\" />") else {
            return false;
        };
        normalized.push_str(before);
        normalized.push_str(emoji);
        rest = after;
    }
    normalized.push_str(rest);
    original == normalized
}

fn decode_references(source: &str, range: Range<usize>) -> (String, Vec<usize>) {
    let raw = &source[range.clone()];
    let mut decoded = String::with_capacity(raw.len());
    let mut offsets = Vec::with_capacity(raw.len() + 1);
    let mut offset = 0;
    while offset < raw.len() {
        let rest = &raw[offset..];
        if rest.starts_with('&')
            && !markdown_escaped(&source[..range.start + offset])
            && let Some((reference, value)) = decode_reference(rest)
        {
            offsets.resize(decoded.len() + value.len(), offset);
            decoded.push_str(&value);
            offset += reference;
        } else if let Some(ch) = rest.chars().next() {
            offsets.resize(decoded.len() + ch.len_utf8(), offset);
            decoded.push(ch);
            offset += ch.len_utf8();
        }
    }
    offsets.push(raw.len());
    (decoded, offsets)
}

fn decode_reference(source: &str) -> Option<(usize, String)> {
    let end = source.bytes().take(33).position(|byte| byte == b';')?;
    let name = &source[1..end];
    let value = if let Some(number) = name.strip_prefix('#') {
        let (digits, radix, limit) = match number.strip_prefix(['x', 'X']) {
            Some(digits) => (digits, 16, 6),
            None => (number, 10, 7),
        };
        if digits.len() > limit || !digits.chars().all(|ch| ch.is_digit(radix)) || digits.is_empty()
        {
            return None;
        }
        markdown::decode_numeric(digits, radix)
    } else {
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return None;
        }
        markdown::decode_named(name, true)?
    };
    Some((end + 1, value))
}

fn escape_neighbor(
    source: &str,
    offset: usize,
    after: bool,
    sibling: Option<&Node>,
    edits: &mut Vec<(Range<usize>, String)>,
) {
    let Some(sibling) = sibling else {
        return;
    };
    let Some(position) = edge_text(sibling, after).and_then(|text| text.position.as_ref()) else {
        return;
    };
    let offset = if matches!(sibling, Node::Text(_)) {
        offset
    } else {
        let (edge, gap, delimiter) = if after {
            (
                position.start.offset,
                offset..position.start.offset,
                source.as_bytes()[offset - 1],
            )
        } else {
            (
                position.end.offset,
                position.end.offset..offset,
                source.as_bytes()[offset],
            )
        };
        if !source
            .get(gap)
            .is_some_and(|gap| gap.bytes().all(|byte| byte == delimiter))
        {
            return;
        }
        edge
    };
    let raw = if after {
        source.get(offset..position.end.offset)
    } else {
        source.get(position.start.offset..offset)
    };
    let Some(raw) = raw else {
        return;
    };
    let Some(ch) = (if after {
        raw.chars().next()
    } else {
        raw.chars().next_back()
    })
    .filter(|&ch| {
        let punctuation = GeneralCategoryGroup::Punctuation.union(GeneralCategoryGroup::Symbol);
        !ch.is_whitespace()
            && !ch.is_ascii_punctuation()
            && !punctuation.contains(CodePointMapData::<GeneralCategory>::new().get(ch))
    }) else {
        return;
    };
    let entity = format!("&#{};", u32::from(ch));
    // Adjacent literal delimiters in the same text node must stay literal once
    // this character reference gives them a punctuation boundary.
    if after {
        let start = offset + ch.len_utf8();
        let width = source.as_bytes()[start..position.end.offset]
            .iter()
            .take_while(|&&byte| attention_delimiter(byte))
            .count();
        edits.push((
            offset..start + width,
            entity + &escape_markdown(&source[start..start + width]),
        ));
    } else {
        let end = offset - ch.len_utf8();
        let width = source.as_bytes()[position.start.offset..end]
            .iter()
            .rev()
            .take_while(|&&byte| attention_delimiter(byte))
            .count();
        let start = end - width;
        let start = start + usize::from(width > 0 && markdown_escaped(&source[..start]));
        edits.push((
            start..offset,
            escape_markdown(&source[start..end]) + &entity,
        ));
    }
}

fn edge_text(node: &Node, first: bool) -> Option<&Text> {
    match node {
        Node::Text(text) => Some(text),
        Node::Emphasis(_) | Node::Strong(_) | Node::Delete(_) => {
            let children = node.children()?;
            let child = if first {
                children.first()?
            } else {
                children.last()?
            };
            edge_text(child, first)
        }
        _ => None,
    }
}

fn attention_delimiter(byte: u8) -> bool {
    matches!(byte, b'*' | b'_' | b'~')
}

fn escape_literal_delimiters(
    source: &str,
    range: Range<usize>,
    after: bool,
    edits: &mut Vec<(Range<usize>, String)>,
) {
    let mut run = range.clone();
    if after {
        run.end = range.start
            + source[range]
                .bytes()
                .take_while(|&b| attention_delimiter(b))
                .count();
    } else {
        run.start = range.end
            - source[range]
                .bytes()
                .rev()
                .take_while(|&b| attention_delimiter(b))
                .count();
    }
    if !run.is_empty() && markdown_escaped(&source[..run.start]) {
        run.start += 1;
    }
    if !run.is_empty() {
        let literal = escape_markdown(&source[run.clone()]);
        edits.push((run, literal));
    }
}

fn emoji_marker(emoji: &str) -> String {
    format!("![{emoji}]({EMOJI_URL})")
}

impl MarkdownPlugin for MessageEmoji {
    fn name(&self) -> &str {
        "spk-emoji"
    }

    fn parse(&self, node: &Node, _: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let emoji = match node {
            Node::Text(text) => &text.value,
            Node::Image(image) if image.url == EMOJI_URL => &image.alt,
            _ => return None,
        };
        let presentation = self.presentation(emoji)?;
        Some(
            MarkdownNode::new(self.name().to_owned(), presentation)
                .text(emoji.clone())
                .markdown(emoji.clone()),
        )
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        _: &mut App,
    ) -> Option<InlineElement> {
        let presentation = node.data::<EmojiPresentation>()?;
        let size = if self.large {
            context.rem_size() * 2.25
        } else {
            context.font_size() * 1.2
        };
        Some(match presentation {
            EmojiPresentation::Artwork(path) => InlineElement::new(
                div()
                    .id("emoji-glyph")
                    .role(Role::Image)
                    .aria_label(node.as_text().to_owned())
                    .child(img(path.clone()).size(size)),
            ),
            EmojiPresentation::Unicode => InlineElement::new(
                div()
                    .text_size(size)
                    .line_height(size)
                    .child(node.as_text().to_owned()),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MessageEmoji, Node, SharedString, parse_options};

    #[test]
    fn posted_emoji_preserves_intraword_emphasis_and_decodes_references() {
        let source = SharedString::from("_left_a*1️⃣hello*\n\n*bye1️⃣*b_right_ &#x1f44d;&#127997;");
        let display = MessageEmoji::new(false).display_markdown(&source);
        let tree = markdown::to_mdast(&display, &parse_options()).unwrap();
        let [Node::Paragraph(first), Node::Paragraph(second)] = tree.children().unwrap().as_slice()
        else {
            panic!("expected two paragraphs: {display}");
        };
        let [Node::Text(before), Node::Emphasis(first_mark)] = first.children.as_slice() else {
            panic!("expected literal text followed by emphasis: {display}");
        };
        let [
            Node::Emphasis(second_mark),
            Node::Text(after),
            Node::Image(tone),
        ] = second.children.as_slice()
        else {
            panic!("expected emphasis followed by literal text and the skin-tone emoji: {display}");
        };
        let [Node::Image(first_keycap), Node::Text(first_label)] = first_mark.children.as_slice()
        else {
            panic!("expected artwork inside the first emphasized span");
        };
        let [Node::Text(second_label), Node::Image(second_keycap)] =
            second_mark.children.as_slice()
        else {
            panic!("expected artwork inside the second emphasized span");
        };
        assert_eq!(before.value, "_left_a");
        assert_eq!(first_keycap.alt, "1️⃣");
        assert_eq!(first_label.value, "hello");
        assert_eq!(second_label.value, "bye");
        assert_eq!(second_keycap.alt, "1️⃣");
        assert_eq!(after.value, "b_right_ ");
        assert_eq!(tone.alt, "👍🏽");
    }
    #[test]
    fn posted_emoji_preserves_combining_marks_and_code_like_blocks() {
        let source = SharedString::from("e\u{301}*1️⃣hello*\n\n$$\n🙂\n$$");
        let display = MessageEmoji::new(false).display_markdown(&source);
        assert!(display.ends_with("$$\n🙂\n$$"));
        let tree = markdown::to_mdast(&display, &parse_options()).unwrap();
        let Node::Paragraph(paragraph) = &tree.children().unwrap()[0] else {
            panic!("expected the opening paragraph");
        };
        let [Node::Text(text), Node::Emphasis(mark)] = paragraph.children.as_slice() else {
            panic!("expected the combining mark and emphasis: {display}");
        };
        assert_eq!(text.value, "e\u{301}");
        assert!(matches!(&mark.children[0], Node::Image(image) if image.alt == "1️⃣"));
    }
    #[test]
    fn posted_emoji_keeps_unmatched_emphasis_markers_literal() {
        let source = SharedString::from("*😀*1️⃣");
        let display = MessageEmoji::new(false).display_markdown(&source);
        let tree = markdown::to_mdast(&display, &parse_options()).unwrap();
        let [Node::Paragraph(paragraph)] = tree.children().unwrap().as_slice() else {
            panic!("expected a paragraph");
        };
        let [
            Node::Text(open),
            Node::Image(face),
            Node::Text(close),
            Node::Image(keycap),
        ] = paragraph.children.as_slice()
        else {
            panic!("expected two literal asterisks and two emoji: {display}");
        };
        assert_eq!(open.value, "*");
        assert_eq!(face.alt, "😀");
        assert_eq!(close.value, "*");
        assert_eq!(keycap.alt, "1️⃣");
    }
    #[test]
    fn posted_emoji_preserves_shared_emphasis_delimiters() {
        let source = SharedString::from("**Status***1️⃣ step*\n\na._1️⃣hi__");
        let display = MessageEmoji::new(false).display_markdown(&source);
        let tree = markdown::to_mdast(&display, &parse_options()).unwrap();
        let [Node::Paragraph(first), Node::Paragraph(second)] = tree.children().unwrap().as_slice()
        else {
            panic!("expected two paragraphs: {display}");
        };
        let [Node::Strong(status), Node::Emphasis(step)] = first.children.as_slice() else {
            panic!("expected adjacent bold and italic text: {display}");
        };
        assert_eq!(status.children[0].to_string(), "Status");
        assert!(matches!(&step.children[0], Node::Image(image) if image.alt == "1️⃣"));
        let [Node::Text(_), Node::Emphasis(mark), Node::Text(last)] = second.children.as_slice()
        else {
            panic!("expected emphasis and a trailing underscore: {display}");
        };
        assert!(matches!(&mark.children[0], Node::Image(image) if image.alt == "1️⃣"));
        assert_eq!(last.value, "_");
    }
    #[test]
    fn posted_emoji_preserves_original_source_if_markdown_would_change() {
        let source = SharedString::from("**hi😀****Status**");
        assert_eq!(MessageEmoji::new(false).display_markdown(&source), source);
    }
}
