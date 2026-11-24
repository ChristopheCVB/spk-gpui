use std::ops::Range;

use ::markdown::{ParseOptions, mdast::Node};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Format {
    Bold,
    Italic,
    Strike,
    Code,
    CodeBlock,
    Quote,
    Spoiler,
    Link,
}

impl Format {
    pub(super) fn delimiter(self) -> Option<&'static str> {
        match self {
            Self::Bold => Some("**"),
            Self::Italic => Some("_"),
            Self::Strike => Some("~~"),
            Self::Code => Some("`"),
            Self::Spoiler => Some("||"),
            _ => None,
        }
    }
}

pub(super) struct Syntax {
    pub range: Range<usize>,
    pub label: String,
}

pub(super) struct Mark {
    pub format: Format,
    pub range: Range<usize>,
    pub open: Range<usize>,
    pub close: Range<usize>,
}

#[derive(Default)]
pub(super) struct Projection {
    pub marks: Vec<Mark>,
    pub syntax: Vec<Syntax>,
}

impl Projection {
    pub fn parse(source: &str) -> Self {
        let mut options = ParseOptions::gfm();
        options.constructs.html_flow = false;
        options.constructs.html_text = false;
        options.constructs.gfm_autolink_literal = false;
        let Ok(document) = ::markdown::to_mdast(source, &options) else {
            return Self::default();
        };
        let mut result = Self::default();
        result.visit(&document, source, &mut None);
        result.syntax.sort_by_key(|syntax| syntax.range.start);
        result
    }

    pub fn covering(&self, format: Format, selection: &Range<usize>) -> Option<&Mark> {
        self.marks.iter().rev().find(|mark| {
            mark.format == format
                && mark.range.start <= selection.start
                && selection.end <= mark.range.end
        })
    }

    pub fn active(&self, format: Format, selection: &Range<usize>) -> bool {
        self.covering(format, selection).is_some()
    }

    fn add_syntax(&mut self, range: Range<usize>, label: impl Into<String>) {
        if !range.is_empty() {
            self.syntax.push(Syntax {
                range,
                label: label.into(),
            });
        }
    }

    fn mark(
        &mut self,
        format: Format,
        range: Range<usize>,
        open: Range<usize>,
        close: Range<usize>,
    ) {
        self.add_syntax(open.clone(), "");
        self.add_syntax(close.clone(), "");
        self.marks.push(Mark {
            format,
            range,
            open,
            close,
        });
    }

    fn visit(&mut self, node: &Node, source: &str, spoiler: &mut Option<Range<usize>>) {
        let Some(range) = node_range(node) else {
            return;
        };
        match node {
            Node::Strong(_) | Node::Emphasis(_) | Node::Delete(_) => {
                if let Some(body) = children_range(node) {
                    let format = match node {
                        Node::Strong(_) => Format::Bold,
                        Node::Emphasis(_) => Format::Italic,
                        _ => Format::Strike,
                    };
                    self.mark(
                        format,
                        body.clone(),
                        range.start..body.start,
                        body.end..range.end,
                    );
                }
            }
            Node::InlineCode(code) => {
                let text = &source[range.clone()];
                let tick_count = text.bytes().take_while(|byte| *byte == b'`').count();
                let mut body = range.start + tick_count..range.end - tick_count;
                if source[body.clone()].starts_with(' ')
                    && source[body.clone()].ends_with(' ')
                    && !code.value.trim().is_empty()
                {
                    body.start += 1;
                    body.end -= 1;
                }
                self.mark(
                    Format::Code,
                    body.clone(),
                    range.start..body.start,
                    body.end..range.end,
                );
            }
            Node::Code(code) => {
                let text = &source[range.clone()];
                if let Some((fence, body)) = text.split_once('\n')
                    && let Some((marker, count)) = fence_marker(fence)
                {
                    let open = range.start..range.start + fence.trim_end_matches('\r').len();
                    let start = range.start + fence.len() + 1;
                    let close = body
                        .rsplit('\n')
                        .next()
                        .filter(|line| {
                            let line = line.trim();
                            line.len() >= count && line.bytes().all(|byte| byte == marker)
                        })
                        .map_or(range.end..range.end, |line| {
                            range.end - line.len()..range.end
                        });
                    self.add_syntax(open.clone(), code.lang.as_deref().unwrap_or("plain text"));
                    self.add_syntax(close.clone(), "");
                    self.marks.push(Mark {
                        format: Format::CodeBlock,
                        range: start..close.start,
                        open,
                        close,
                    });
                }
            }
            Node::Blockquote(_) => {
                let mut offset = range.start;
                let mut first = range.start;
                for line in source[range.clone()].split_inclusive('\n') {
                    if let Some((start, end)) = quote_prefix(line) {
                        self.add_syntax(offset + start..offset + end, "│ ");
                        if offset == range.start {
                            first += end;
                        }
                    }
                    offset += line.len();
                }
                self.marks.push(Mark {
                    format: Format::Quote,
                    range: first..range.end,
                    open: range.start..first,
                    close: range.end..range.end,
                });
            }
            Node::ListItem(_) => {
                let line = source[range.clone()].lines().next().unwrap_or_default();
                if let Some((end, ordinal)) = list_prefix(line) {
                    self.add_syntax(
                        range.start..range.start + end,
                        ordinal.map_or_else(|| "• ".to_owned(), |number| format!("{number}. ")),
                    );
                }
            }
            Node::Link(_) => {
                if let Some(body) = children_range(node) {
                    self.mark(
                        Format::Link,
                        body.clone(),
                        range.start..body.start,
                        body.end..range.end,
                    );
                }
            }
            Node::Text(_) => {
                let text = &source[range.clone()];
                let mut chars = text.char_indices().peekable();
                while let Some((index, ch)) = chars.next() {
                    let offset = range.start + index;
                    if ch == '\n' {
                        *spoiler = None;
                    } else if ch == '\\'
                        && chars
                            .peek()
                            .is_some_and(|(_, next)| next.is_ascii_punctuation())
                    {
                        self.add_syntax(offset..offset + 1, "");
                        chars.next();
                    } else if ch == '|' && chars.peek().is_some_and(|(_, next)| *next == '|') {
                        chars.next();
                        let marker = offset..offset + 2;
                        let opening = spoiler.take();
                        if let Some(open) = opening.filter(|open| {
                            let body = &source[open.end..marker.start];
                            !body.is_empty() && body.trim() == body
                        }) {
                            self.mark(Format::Spoiler, open.end..marker.start, open, marker);
                        } else {
                            *spoiler = Some(marker);
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some(children) = node.children() {
            let spoiler = if matches!(node, Node::Paragraph(_)) {
                &mut None
            } else {
                spoiler
            };
            for child in children {
                self.visit(child, source, spoiler);
            }
        }
    }

    pub fn plain(&self, source: &str, selection: Range<usize>) -> String {
        let mut text = String::new();
        let mut offset = selection.start;
        for syntax in &self.syntax {
            let start = syntax.range.start.max(selection.start);
            let end = syntax.range.end.min(selection.end);
            if start < offset || start >= end {
                continue;
            }
            text.push_str(&source[offset..start]);
            if syntax.label == "• " || syntax.label.ends_with(". ") {
                text.push_str(&syntax.label);
            }
            offset = end;
        }
        text.push_str(&source[offset..selection.end]);
        text
    }
}

fn node_range(node: &Node) -> Option<Range<usize>> {
    let position = node.position()?;
    Some(position.start.offset..position.end.offset)
}

fn children_range(node: &Node) -> Option<Range<usize>> {
    let children = node.children()?;
    Some(node_range(children.first()?)?.start..node_range(children.last()?)?.end)
}

pub(super) fn fence_language(line: &str) -> Option<&str> {
    let language = line.strip_prefix("```")?;
    language
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"_+#.-".contains(&byte))
        .then_some(language)
}

fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let line = line.trim_start_matches(' ');
    let marker = *line.as_bytes().first()?;
    let count = line.bytes().take_while(|byte| *byte == marker).count();
    (matches!(marker, b'`' | b'~') && count >= 3).then_some((marker, count))
}

pub(super) fn quote_prefix(line: &str) -> Option<(usize, usize)> {
    let offset = line.len() - line.trim_start_matches(' ').len();
    let after = line[offset..].strip_prefix('>')?;
    Some((offset, offset + 1 + usize::from(after.starts_with(' '))))
}

pub(super) fn list_prefix(line: &str) -> Option<(usize, Option<u32>)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let text = &line[indent..];
    if text
        .as_bytes()
        .get(0..2)
        .is_some_and(|prefix| matches!(prefix, b"- " | b"* " | b"+ "))
    {
        return Some((indent + 2, None));
    }
    let number = text.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&number)
        && text
            .as_bytes()
            .get(number..number + 2)
            .is_some_and(|suffix| matches!(suffix, b". " | b") "))
    {
        return Some((indent + number + 2, text[..number].parse().ok()));
    }
    None
}

pub(super) fn current_line(source: &str, selection: &Range<usize>) -> Range<usize> {
    let start = source[..selection.start]
        .rfind('\n')
        .map_or(0, |offset| offset + 1);
    let end = source[selection.end..]
        .find('\n')
        .map_or(source.len(), |offset| selection.end + offset);
    start..end
}

pub(super) fn enter_change(
    source: &str,
    selection: Range<usize>,
) -> Option<(Range<usize>, String, usize)> {
    let start = selection.start;
    let line = current_line(source, &selection);
    let text = &source[line.clone()];
    let projection = Projection::parse(source);
    if projection.active(Format::CodeBlock, &selection) {
        return Some((selection, "\n".to_owned(), start + 1));
    }
    if selection.is_empty() && fence_language(text).is_some() {
        return Some((line.end..line.end, "\n\n```".to_owned(), line.end + 1));
    }
    let (end, ordinal) = list_prefix(text)?;
    if !selection.is_empty() || start < line.start + end {
        return None;
    }
    if text[end..].trim().is_empty() {
        return Some((line.clone(), String::new(), line.start));
    }
    let marker = match ordinal {
        Some(number) => {
            let indent = text.len() - text.trim_start_matches(' ').len();
            let delimiter = text.as_bytes()[end - 2] as char;
            format!(
                "{}{number}{delimiter} ",
                &text[..indent],
                number = number + 1
            )
        }
        None => text[..end].to_owned(),
    };
    let newline = format!("\n{marker}");
    let caret = start + newline.len();
    Some((selection, newline, caret))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_markdown_shows_formatting_without_delimiters() {
        let source = "**bold _both_** ~~strike~~ `code` ||spoiler|| [Link](https://example.org)\n\n> Quote\n\n- Item\n\n```rust\nlet x = 1;\n```";
        let draft = Projection::parse(source);
        assert_eq!(
            draft.plain(source, 0..source.len()),
            "bold both strike code spoiler Link\n\nQuote\n\n• Item\n\nlet x = 1;\n"
        );
        for (word, format) in [
            ("bold", Format::Bold),
            ("both", Format::Italic),
            ("strike", Format::Strike),
            ("code", Format::Code),
            ("spoiler", Format::Spoiler),
            ("Link", Format::Link),
            ("Quote", Format::Quote),
            ("let", Format::CodeBlock),
        ] {
            let start = source.find(word).unwrap();
            assert!(draft.active(format, &(start..start + word.len())), "{word}");
        }
        assert!(draft.syntax.iter().any(|syntax| syntax.label == "rust"));
    }

    #[test]
    fn enter_opens_a_fence_continues_a_list_and_exits_an_empty_item() {
        assert_eq!(
            enter_change("intro\n```rust", 13..13),
            Some((13..13, "\n\n```".to_owned(), 14))
        );
        assert_eq!(
            enter_change("3. item", 7..7),
            Some((7..7, "\n4. ".to_owned(), 11))
        );
        assert_eq!(
            enter_change("3. item\n4. ", 11..11),
            Some((8..11, String::new(), 8))
        );
        assert!(enter_change("normal text", 11..11).is_none());
    }
}
