use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone};

use crate::{api::ChatMessage, session::ChatView};

#[cfg(test)]
mod tests;

pub(super) enum HistoryRow<'a> {
    Date(NaiveDate),
    Unread,
    Message {
        message: &'a ChatMessage,
        starts_group: bool,
    },
}

impl<'a> HistoryRow<'a> {
    fn message(&self) -> Option<&'a ChatMessage> {
        match self {
            Self::Message { message, .. } => Some(message),
            Self::Date(_) | Self::Unread => None,
        }
    }
}

pub(super) fn history_rows<'a>(
    messages: &'a [ChatMessage],
    first_unread: Option<&str>,
    timezone: &impl TimeZone,
) -> impl Iterator<Item = HistoryRow<'a>> {
    let mut previous: Option<(&ChatMessage, Option<NaiveDate>)> = None;
    messages.iter().flat_map(move |message| {
        let date = DateTime::from_timestamp_millis(message.created_at)
            .map(|timestamp| timestamp.with_timezone(timezone).date_naive());
        let new_day = previous.is_none_or(|(_, previous_date)| previous_date != date);
        let unread = first_unread == Some(message.id.as_str());
        let starts_group = new_day
            || unread
            || previous.is_none_or(|(previous, _)| {
                previous.user_id != message.user_id
                    || message.created_at.saturating_sub(previous.created_at) >= 5 * 60_000
            });
        previous = Some((message, date));

        [
            date.filter(|_| new_day).map(HistoryRow::Date),
            unread.then_some(HistoryRow::Unread),
            Some(HistoryRow::Message {
                message,
                starts_group,
            }),
        ]
        .into_iter()
        .flatten()
    })
}

pub(super) fn history_message_index(chat: &ChatView, id: &str) -> Option<usize> {
    message_row_index(
        history_rows(&chat.messages, chat.first_unread_id.as_deref(), &Local),
        id,
    )
}

pub(super) fn message_row_index<'a>(
    mut rows: impl Iterator<Item = HistoryRow<'a>>,
    id: &str,
) -> Option<usize> {
    rows.position(|row| row.message().is_some_and(|message| message.id == id))
}

pub(super) fn history_message_at(
    chat: &ChatView,
    row_index: usize,
) -> Option<(usize, &ChatMessage)> {
    history_rows(&chat.messages, chat.first_unread_id.as_deref(), &Local)
        .enumerate()
        .skip(row_index)
        .find_map(|(index, row)| Some((index, row.message()?)))
}

pub(super) fn date_label(date: NaiveDate, today: NaiveDate) -> String {
    if date == today {
        "Today".into()
    } else if date.succ_opt() == Some(today) {
        "Yesterday".into()
    } else if date.year() == today.year() {
        date.format("%b %-d").to_string()
    } else {
        date.format("%b %-d, %Y").to_string()
    }
}

pub(super) fn relative_time(sent: i64, now: i64) -> String {
    let seconds = now.saturating_sub(sent).max(0).saturating_add(500) / 1000;
    match seconds {
        ..45 => "just now".into(),
        ..3600 => format!("{} min ago", ((seconds + 30) / 60).max(1)),
        ..86_400 => format!("{} h ago", (seconds + 1800) / 3600),
        _ => format!("{} d ago", (seconds + 43_200) / 86_400),
    }
}
