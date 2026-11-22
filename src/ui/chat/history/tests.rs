use chrono::{FixedOffset, TimeZone};
use gpui_kit::{component::Theme, rgb};

use super::{HistoryRow, date_label, history_rows, message_row_background, message_row_index};
use crate::api::ChatMessage;

#[test]
fn history_groups_messages_and_orders_local_date_and_unread_dividers() {
    let local = FixedOffset::east_opt(2 * 3600).unwrap();
    let now = local.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap();
    let midnight = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
    let midnight = local
        .from_local_datetime(&midnight)
        .unwrap()
        .timestamp_millis();
    let messages: Vec<ChatMessage> = serde_json::from_value(serde_json::json!([
        {"id":"first","channelId":"general","userId":"alice","body":"","createdAt":midnight - 660_000},
        {"id":"follow-up","channelId":"general","userId":"alice","body":"","createdAt":midnight - 361_000},
        {"id":"five-minute-gap","channelId":"general","userId":"alice","body":"","createdAt":midnight - 61_000},
        {"id":"new-author","channelId":"general","userId":"bob","body":"","createdAt":midnight - 1_000},
        {"id":"new-day","channelId":"general","userId":"bob","body":"","createdAt":midnight},
        {"id":"first-unread","channelId":"general","userId":"bob","body":"","createdAt":midnight + 1_000}
    ]))
    .unwrap();
    let describe = |unread| {
        history_rows(&messages, Some(unread), &local)
            .map(|row| match row {
                HistoryRow::Date(date) => date_label(date, now.date_naive()),
                HistoryRow::Unread => "New messages".into(),
                HistoryRow::Message {
                    message,
                    starts_group,
                } => format!(
                    "{} {}",
                    message.id,
                    if starts_group { "header" } else { "grouped" }
                ),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        describe("new-day"),
        [
            "Yesterday",
            "first header",
            "follow-up grouped",
            "five-minute-gap header",
            "new-author header",
            "Today",
            "New messages",
            "new-day header",
            "first-unread grouped",
        ]
    );
    assert_eq!(
        &describe("first-unread")[5..],
        [
            "Today",
            "new-day header",
            "New messages",
            "first-unread header"
        ]
    );
    let rows = || history_rows(&messages, Some("new-day"), &local);
    assert_eq!(message_row_index(rows(), "new-day"), Some(7));
    assert_eq!(message_row_index(rows(), "follow-up"), Some(2));
}

#[test]
fn day_labels_use_the_year_only_for_a_different_local_year() {
    let local = FixedOffset::west_opt(7 * 3600).unwrap();
    let today = local
        .with_ymd_and_hms(2026, 11, 22, 23, 0, 0)
        .unwrap()
        .date_naive();
    let date = |year, month, day| {
        local
            .with_ymd_and_hms(year, month, day, 0, 0, 0)
            .unwrap()
            .date_naive()
    };
    assert_eq!(date_label(date(2026, 11, 8), today), "Nov 8");
    assert_eq!(date_label(date(2025, 11, 8), today), "Nov 8, 2025");
}

#[test]
fn relative_time_uses_the_web_rounding_and_units() {
    use super::relative_time;
    let now = 1_800_000_000_000;
    assert_eq!(relative_time(now + 1, now), "just now");
    assert_eq!(relative_time(now - 44_499, now), "just now");
    assert_eq!(relative_time(now - 44_500, now), "1 min ago");
    assert_eq!(relative_time(now - 90_000, now), "2 min ago");
    assert_eq!(relative_time(now - 9_000_000, now), "3 h ago");
    assert_eq!(relative_time(now - 86_400_000, now), "1 d ago");
}

#[test]
fn history_distinguishes_pings_and_the_active_jump() {
    let mut theme = Theme::default();
    theme.colors.primary = rgb(0xf59e0b).into();
    theme.colors.foreground = rgb(0xe4e4e7).into();
    let messages: Vec<ChatMessage> = serde_json::from_value(serde_json::json!([
        {"id":"mention","channelId":"general","userId":"bob","body":"Hi <@alice>","createdAt":1},
        {
            "id":"reply","channelId":"general","userId":"bob","body":"Thanks!","createdAt":2,
            "replyTo":{"id":"original","userId":"alice","preview":"Hi","deleted":false}
        },
        {"id":"self","channelId":"general","userId":"alice","body":"Hi <@alice>","createdAt":3},
        {"id":"inline-code","channelId":"general","userId":"bob","body":"`<@alice>`","createdAt":4},
        {"id":"fenced-code","channelId":"general","userId":"bob","body":"```\n<@alice>\n```","createdAt":5}
    ]))
    .unwrap();
    let timezone = FixedOffset::east_opt(0).unwrap();
    let backgrounds = |direct_message, target: Option<&str>| {
        history_rows(&messages, None, &timezone)
            .filter_map(|row| row.message())
            .map(|message| {
                message_row_background(message, direct_message, Some("alice"), target, &theme)
            })
            .collect::<Vec<_>>()
    };
    let ping = Some(theme.primary.opacity(0.1));
    let jump = Some(theme.foreground.opacity(0.1));
    assert_ne!(ping, jump);
    assert_eq!(backgrounds(false, None), [ping, ping, None, None, None]);
    assert_eq!(
        backgrounds(false, Some("reply")),
        [ping, jump, None, None, None]
    );
    assert_eq!(backgrounds(true, None), [None, None, None, None, None]);
    assert_eq!(
        backgrounds(true, Some("mention")),
        [jump, None, None, None, None]
    );
}
