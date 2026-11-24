mod markdown;

pub(in crate::ui) use markdown::Format;

use std::ops::Range;

use gpui_kit::{
    base::input::TextDecoration,
    component::input::{InlineToken, InputContent, InputToken},
};

use super::super::*;
use markdown::{Projection, current_line, enter_change, quote_prefix};

const SYNTAX: &str = "spk:markdown";
const PENDING: &str = "spk:pending-format";
const PENDING_OFF: &str = "spk:pending-unformatted";
const MENTION: &str = "spk:mention";
const SPOILER_MENTION: &str = "spk:spoiler-mention";
const CLIPBOARD: &str = "spk-message-markdown-v1";

pub(in crate::ui) fn textarea(input: &Entity<TextareaState>) -> Textarea {
    Textarea::new(input).token(|context, _, cx| match context.token().id().as_ref() {
        SYNTAX | PENDING | PENDING_OFF => div()
            .text_color(cx.theme().muted_foreground)
            .child(context.token().label().trim_end_matches(' ').to_owned())
            .when(context.token().label().ends_with(' '), |element| {
                element.pr_1()
            })
            .into_any_element(),
        MENTION => InputToken::new(context)
            .bg(cx.theme().accent)
            .text_color(cx.theme().link)
            .into_any_element(),
        SPOILER_MENTION => InputToken::new(context)
            .bg(cx.theme().muted_foreground)
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
        _ => InputToken::new(context).into_any_element(),
    })
}

pub(in crate::ui) fn refresh(
    input: &Entity<TextareaState>,
    directory: Option<&Sidebar>,
    cx: &mut App,
) {
    let theme = cx.theme();
    let muted = theme.muted;
    let muted_text = theme.muted_foreground;
    let link = theme.link;
    input.update(cx, |input, cx| {
        let source = input.value();
        let projection = Projection::parse(&source);
        let mut tokens = InputContent::new(source.clone());
        for span in input
            .tokens()
            .iter()
            .filter(|span| span.token().id() != SYNTAX && !is_mention(span.token()))
        {
            tokens = attach(tokens, span.range(), span.token().clone());
        }
        let selection = input.selected_range();
        let splits_selection = |range: &Range<usize>| {
            [selection.start, selection.end]
                .iter()
                .any(|offset| range.start < *offset && *offset < range.end)
        };
        for range in &projection.mentions {
            if splits_selection(range) {
                continue;
            }
            let text = &source[range.clone()];
            let id = &text[2..text.len() - 1];
            let name = directory
                .and_then(|directory| directory.mention_name(id))
                .unwrap_or(id);
            let kind = if projection.active(Format::Spoiler, range) {
                SPOILER_MENTION
            } else {
                MENTION
            };
            let label = format!(
                "@{}",
                name.replace(
                    |ch: char| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'),
                    " "
                )
            );
            tokens = attach(tokens, range.clone(), syntax_token(kind, text, &label));
        }
        for syntax in projection.syntax {
            if splits_selection(&syntax.range) {
                continue;
            }
            let text = &source[syntax.range.clone()];
            let token = syntax_token(SYNTAX, text, &syntax.label);
            tokens = attach(tokens, syntax.range, token);
        }
        let _ = input.set_tokens(tokens, cx);
        let decorations = projection
            .marks
            .into_iter()
            .map(|mark| {
                let mut style = HighlightStyle::default();
                match mark.format {
                    Format::Bold => style.font_weight = Some(FontWeight::BOLD),
                    Format::Italic => style.font_style = Some(FontStyle::Italic),
                    Format::Strike => {
                        style.strikethrough = Some(StrikethroughStyle {
                            thickness: px(1.),
                            ..Default::default()
                        });
                    }
                    Format::Code | Format::CodeBlock => style.background_color = Some(muted),
                    Format::Quote => style.color = Some(muted_text),
                    Format::Spoiler => {
                        style.background_color = Some(muted_text);
                        style.color = Some(muted);
                    }
                    Format::Link => {
                        style.color = Some(link);
                        style.underline = Some(UnderlineStyle {
                            thickness: px(1.),
                            ..Default::default()
                        });
                    }
                }
                TextDecoration::new(mark.range, style)
            })
            .collect();
        input.set_decorations(decorations, cx);
    });
}

pub(super) fn is_mention(token: &InlineToken) -> bool {
    matches!(token.id().as_ref(), MENTION | SPOILER_MENTION)
}

pub(super) fn replace_with_mention(
    input: &mut TextareaState,
    range: Range<usize>,
    id: &str,
    window: &mut Window,
    cx: &mut Context<TextareaState>,
) {
    let (range, text, caret) = markdown::mention_change(&input.value(), range, &format!("<@{id}>"));
    replace(input, range, text, caret..caret, window, cx);
    input.focus(window, cx);
}

pub(in crate::ui) fn backspace_mention(
    entity: &Entity<TextareaState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    entity.update(cx, |input, cx| {
        let caret = input.selected_range();
        if !caret.is_empty()
            || !input.is_editable()
            || input.marked_text_range(window, cx).is_some()
        {
            return false;
        }
        let Some(span) = input
            .tokens()
            .iter()
            .find(|span| span.range().end == caret.end && is_mention(span.token()))
        else {
            return false;
        };
        input.set_selected_range(span.range(), cx);
        input.replace("@", window, cx);
        true
    })
}

fn syntax_token(id: &'static str, source: &str, label: &str) -> InlineToken {
    // GPUI requires even a hidden token to have a nonempty label.
    InlineToken::new(id, source.to_owned())
        .with_label(if label.is_empty() { " " } else { label }.to_owned())
}

fn attach(content: InputContent, range: Range<usize>, token: InlineToken) -> InputContent {
    content.clone().with_token(range, token).unwrap_or(content)
}

fn replace(
    input: &mut TextareaState,
    range: Range<usize>,
    text: String,
    selection: Range<usize>,
    window: &mut Window,
    cx: &mut Context<TextareaState>,
) {
    input.set_selected_range(range, cx);
    input.replace(text, window, cx);
    input.set_selected_range(selection, cx);
}

fn pending_range(
    input: &TextareaState,
    caret: usize,
    delimiter: &str,
) -> Option<(Range<usize>, bool)> {
    [PENDING, PENDING_OFF].into_iter().find_map(|id| {
        let matches = |span: &gpui_kit::component::input::InlineTokenSpan| {
            span.token().id() == id && span.token().text() == delimiter
        };
        let left = input
            .tokens()
            .iter()
            .any(|span| matches(span) && span.range().end == caret);
        let right = input
            .tokens()
            .iter()
            .any(|span| matches(span) && span.range().start == caret);
        (left && right).then(|| {
            (
                caret - delimiter.len()..caret + delimiter.len(),
                id == PENDING,
            )
        })
    })
}

pub(in crate::ui) fn active_formats(input: &TextareaState) -> impl Fn(Format) -> bool + '_ {
    let selection = input.selected_range();
    let projection = Projection::parse(&input.value());
    move |format| {
        if selection.is_empty()
            && let Some(delimiter) = format.delimiter()
            && let Some((_, active)) = pending_range(input, selection.start, delimiter)
        {
            return active;
        }
        projection.active(format, &selection)
    }
}

pub(in crate::ui) fn toggle(
    entity: &Entity<TextareaState>,
    format: Format,
    window: &mut Window,
    cx: &mut App,
) {
    entity.update(cx, |input, cx| {
        if !input.is_editable() || input.marked_text_range(window, cx).is_some() {
            return;
        }
        let source = input.value();
        let selection = input.selected_range();
        let projection = Projection::parse(&source);
        let mut pending = None;
        if let Some(delimiter) = format.delimiter() {
            if let Some((range, _)) = selection
                .is_empty()
                .then(|| pending_range(input, selection.start, delimiter))
                .flatten()
            {
                let start = range.start;
                replace(input, range, String::new(), start..start, window, cx);
            } else if let Some(mark) = projection.covering(format, &selection) {
                if selection.is_empty() && selection.start == mark.range.end {
                    let end = mark.close.end;
                    input.set_selected_range(end..end, cx);
                } else if selection.is_empty() && selection.start == mark.range.start {
                    let start = mark.open.start;
                    input.set_selected_range(start..start, cx);
                } else {
                    let open = &source[mark.open.clone()];
                    let close = &source[mark.close.clone()];
                    let mut text = String::new();
                    if selection.start > mark.range.start {
                        text.push_str(&source[mark.open.start..selection.start]);
                        text.push_str(close);
                    }
                    let middle = mark.open.start + text.len();
                    text.push_str(&source[selection.clone()]);
                    if selection.end < mark.range.end {
                        text.push_str(open);
                        text.push_str(&source[selection.end..mark.close.end]);
                    }
                    replace(
                        input,
                        mark.open.start..mark.close.end,
                        text,
                        middle..middle + selection.len(),
                        window,
                        cx,
                    );
                    if selection.is_empty() && open == close && open == delimiter {
                        pending = Some((middle, delimiter, PENDING_OFF));
                    }
                }
            } else if !projection.active(Format::CodeBlock, &selection)
                && !projection.active(Format::Code, &selection)
            {
                let text = &source[selection.clone()];
                let space_start = text.len() - text.trim_start().len();
                let space_end = text.trim_end().len().max(space_start);
                let range = selection.start + space_start..selection.start + space_end;
                let body = &source[range.clone()];
                let markup = if format == Format::Code && body.contains('`') {
                    let ticks = body
                        .split(|ch| ch != '`')
                        .map(str::len)
                        .max()
                        .unwrap_or_default();
                    "`".repeat(ticks + 1)
                } else {
                    delimiter.to_owned()
                };
                let padding =
                    if format == Format::Code && (body.starts_with('`') || body.ends_with('`')) {
                        " "
                    } else {
                        ""
                    };
                let text = format!("{markup}{padding}{body}{padding}{markup}");
                let start = range.start + markup.len() + padding.len();
                replace(input, range, text, start..start + body.len(), window, cx);
                if selection.is_empty() {
                    pending = Some((start, delimiter, PENDING));
                }
            }
        } else if format == Format::Quote {
            let line = current_line(&source, &selection);
            let lines = source[line.clone()].split('\n');
            let unquote = lines.clone().all(|line| quote_prefix(line).is_some());
            let mut offset = line.start;
            let mut start = selection.start;
            let mut end = selection.end;
            let mut replacement = Vec::new();
            for line in lines {
                if unquote {
                    let (from, to) = quote_prefix(line).unwrap();
                    start -= selection.start.saturating_sub(offset + from).min(to - from);
                    end -= selection.end.saturating_sub(offset + from).min(to - from);
                    replacement.push(format!("{}{}", &line[..from], &line[to..]));
                } else {
                    start += 2 * usize::from(offset <= selection.start);
                    end += 2 * usize::from(offset <= selection.end);
                    replacement.push(format!("> {line}"));
                }
                offset += line.len() + 1;
            }
            replace(input, line, replacement.join("\n"), start..end, window, cx);
        } else if format == Format::CodeBlock {
            if let Some(mark) = projection.covering(format, &selection) {
                let start = selection.start - mark.range.start + mark.open.start;
                let body = &source[mark.range.clone()];
                let body = if mark.close.is_empty() {
                    body
                } else {
                    body.strip_suffix('\n').unwrap_or(body)
                };
                let end = (start + selection.len()).min(mark.open.start + body.len());
                replace(
                    input,
                    mark.open.start..mark.close.end,
                    body.to_owned(),
                    start.min(end)..end,
                    window,
                    cx,
                );
            } else {
                let line = current_line(&source, &selection);
                let body = &source[line.clone()];
                let start = selection.start + 4;
                replace(
                    input,
                    line,
                    format!("```\n{body}\n```"),
                    start..start + selection.len(),
                    window,
                    cx,
                );
            }
        }
        if let Some((middle, delimiter, kind)) = pending {
            let mut content = input.content();
            for range in [
                middle - delimiter.len()..middle,
                middle..middle + delimiter.len(),
            ] {
                content = attach(content, range, syntax_token(kind, delimiter, ""));
            }
            let _ = input.set_tokens(content, cx);
        }
        input.focus(window, cx);
    });
}

pub(in crate::ui) fn enter(
    entity: &Entity<TextareaState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    entity.update(cx, |input, cx| {
        if input.marked_text_range(window, cx).is_some() {
            return true;
        }
        let Some((range, text, caret)) = enter_change(&input.value(), input.selected_range())
        else {
            return false;
        };
        replace(input, range, text, caret..caret, window, cx);
        true
    })
}

pub(super) fn suggestion_prefix(
    input: &TextareaState,
    source: &str,
    caret: usize,
) -> Option<Range<usize>> {
    let prefix = source.get(..caret)?;
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    let mut start = line_start;
    for span in input.tokens().iter().rev() {
        let range = span.range();
        if range.start >= caret {
            continue;
        }
        if range.end > caret {
            return None;
        }
        // A Markdown escape changes one character without splitting the text node.
        if span.token().id() == SYNTAX && span.token().text() == "\\" {
            continue;
        }
        start = start.max(range.end);
        break;
    }
    (start < caret).then_some(start..caret)
}

pub(super) fn empty_suggestion_range(source: &str, mut range: Range<usize>) -> Range<usize> {
    if range.len() == source.len() {
        return range;
    }
    // Erase the Markdown delimiters when the command was the entire marked span.
    for mark in Projection::parse(source).marks.into_iter().rev() {
        if range == mark.range
            && !mark.open.is_empty()
            && !mark.close.is_empty()
            && !matches!(mark.format, Format::CodeBlock)
        {
            range = mark.open.start..mark.close.end;
        }
    }
    range
}

pub(super) fn plain_text(source: &str) -> String {
    Projection::parse(source).plain(source, 0..source.len())
}

pub(in crate::ui) fn wire(input: &TextareaState) -> String {
    message_wire(input, "")
}

pub(super) fn edit_wire(input: &TextareaState, original: &str) -> String {
    message_wire(input, original)
}

fn message_wire(input: &TextareaState, original: &str) -> String {
    let source = input.value();
    let projection = Projection::parse(&source);
    let mut omissions = Vec::new();
    let pending: Vec<_> = input
        .tokens()
        .iter()
        .filter(|span| matches!(span.token().id().as_ref(), PENDING | PENDING_OFF))
        .collect();
    for pair in pending.windows(2) {
        let left = pair[0].range();
        let right = pair[1].range();
        if pair[0].token().id() == pair[1].token().id()
            && pair[0].token().text() == pair[1].token().text()
            && source[left.end..right.start].trim().is_empty()
        {
            omissions.extend([left, right]);
        }
    }
    for mark in projection.marks {
        if matches!(mark.format, Format::Quote | Format::CodeBlock)
            && source[mark.range.clone()].trim().is_empty()
        {
            let range = mark.open.start..mark.range.end.max(mark.close.end);
            if !original.contains(&source[range.clone()]) {
                omissions.push(range);
            }
        }
    }
    omissions.sort_by_key(|range| range.start);
    let mut offset = 0;
    let mut result = String::new();
    for omission in omissions {
        if omission.start >= offset {
            result.push_str(&source[offset..omission.start]);
            offset = omission.end;
        }
    }
    result.push_str(&source[offset..]);
    result
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ClipboardMarkdown {
    kind: String,
    source: String,
}

pub(in crate::ui) fn clipboard_key(
    entity: &Entity<TextareaState>,
    cut: bool,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    entity.update(cx, |input, cx| {
        let selection = input.selected_range();
        if selection.is_empty() || input.marked_text_range(window, cx).is_some() {
            return false;
        }
        let source = input.value();
        let projection = Projection::parse(&source);
        let mut start = String::new();
        let mut end = String::new();
        for mark in projection.marks.iter().filter(|mark| {
            mark.format.delimiter().is_some()
                && mark.range.start < selection.end
                && selection.start < mark.range.end
        }) {
            if selection.start >= mark.range.start {
                start.push_str(&source[mark.open.clone()]);
            }
            if selection.end <= mark.range.end {
                end.insert_str(0, &source[mark.close.clone()]);
            }
        }
        let markdown = format!("{start}{}{end}", &source[selection.clone()]);
        let mut plain = String::new();
        let mut offset = selection.start;
        for span in input.tokens().iter().filter(|span| {
            span.token().id() != SYNTAX
                && selection.start <= span.range().start
                && span.range().end <= selection.end
        }) {
            plain.push_str(&projection.plain(&source, offset..span.range().start));
            if !matches!(span.token().id().as_ref(), PENDING | PENDING_OFF) {
                plain.push_str(span.token().label());
            }
            offset = span.range().end;
        }
        plain.push_str(&projection.plain(&source, offset..selection.end));
        cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(
            plain,
            ClipboardMarkdown {
                kind: CLIPBOARD.to_owned(),
                source: markdown,
            },
        ));
        if cut && input.is_editable() {
            input.replace("", window, cx);
        }
        true
    })
}

pub(in crate::ui) fn paste(
    entity: &Entity<TextareaState>,
    item: &ClipboardItem,
    plain: bool,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    entity.update(cx, |input, cx| {
        if !input.is_editable() || input.marked_text_range(window, cx).is_some() {
            return false;
        }
        let projection = Projection::parse(&input.value());
        let selection = input.selected_range();
        let code = projection.active(Format::Code, &selection)
            || projection.active(Format::CodeBlock, &selection);
        if !plain
            && !code
            && let Some(metadata) = item.metadata()
            && let Ok(ClipboardMarkdown { kind, source }) = serde_json::from_str(metadata)
            && kind == CLIPBOARD
        {
            input.replace(source, window, cx);
            return true;
        }
        let Some(text) = item.text() else {
            return false;
        };
        let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
        if !code {
            if plain {
                text = text
                    .chars()
                    .flat_map(|ch| {
                        let escape = matches!(
                            ch,
                            '\\' | '*' | '_' | '~' | '`' | '[' | ']' | '|' | '>' | '<'
                        );
                        escape
                            .then_some('\\')
                            .into_iter()
                            .chain(std::iter::once(ch))
                    })
                    .collect();
            } else {
                text = text.replace("||", "\\|\\|").replace("](", "\\](");
            }
        }
        input.replace(text, window, cx);
        true
    })
}
