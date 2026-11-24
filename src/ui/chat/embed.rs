use super::*;
use crate::api::{ChatEmbed, EmbedColor};

impl Spk {
    pub(super) fn webhook_embed(
        &self,
        embed: &ChatEmbed,
        description: Option<TextView>,
        now: i64,
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

        div()
            .mt_1()
            .w_full()
            .max_w(px(512.))
            .flex()
            .flex_col()
            .gap_1()
            .rounded_md()
            .border_l_4()
            .border_color(color)
            .bg(theme.secondary)
            .px_3()
            .py_2()
            .text_sm()
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
                            .label(title.to_owned())
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
                card.child(div().grid().grid_cols(3).gap_x_4().gap_y_1().children(
                    embed.fields.iter().enumerate().map(|(index, field)| {
                        div()
                            .id(("embed-field", index))
                            .min_w_0()
                            .col_span(if field.inline { 1 } else { 3 })
                            .flex()
                            .flex_col()
                            .child(
                                Label::new(field.name.clone())
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(Label::new(field.value.clone()))
                    }),
                ))
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
    }
}
