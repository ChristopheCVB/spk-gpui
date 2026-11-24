use super::*;
use crate::{api::Poll, session::MessageAction};

pub(super) fn poll_results(
    sidebar: &Sidebar,
    chat: &ChatView,
    message_id: &str,
    poll: &Poll,
    cx: &Context<Spk>,
) -> impl IntoElement {
    let theme = cx.theme();
    let own_id = sidebar.profile.id.as_deref();
    let total = poll.total_votes();
    div()
        .mt_1()
        .max_w(px(448.))
        .flex()
        .flex_col()
        .gap_1()
        .children(poll.options.iter().map(|option| {
            let count = option.user_ids.len();
            let percent = (count as f64 / total.max(1) as f64 * 100.).round() as u32;
            let mine = own_id.is_some_and(|id| option.user_ids.iter().any(|voter| voter == id));
            let channel_id = chat.channel_id.clone();
            let message_id = message_id.to_owned();
            let choice_id = (!mine).then(|| option.id.clone());
            control_button(SharedString::from(format!("poll-option-{}", option.id)))
                .outline()
                .p_0()
                .selected(mine)
                .toggled(mine)
                .tab_stop(true)
                .disabled(chat.action_pending || !chat.can_send())
                .accessibility_label(format!("{}, {}, {percent}%", option.label, votes(count),))
                .child(
                    div()
                        .relative()
                        .size_full()
                        .min_w_0()
                        .rounded_md()
                        .overflow_hidden()
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left_0()
                                .w(relative(percent as f32 / 100.))
                                .bg(if mine {
                                    theme.primary.opacity(0.2)
                                } else {
                                    theme.muted
                                }),
                        )
                        .child(
                            control_content()
                                .relative()
                                .size_full()
                                .px_2()
                                .justify_between()
                                .child(
                                    control_content()
                                        .when(mine, |label| {
                                            label.child(
                                                Icon::new(IconName::Check)
                                                    .size(ACTION_ICON_SIZE)
                                                    .text_color(theme.primary),
                                            )
                                        })
                                        .child(
                                            div().min_w_0().truncate().child(option.label.clone()),
                                        ),
                                )
                                .child(
                                    Label::new(format!("{count} · {percent}%"))
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .flex_shrink_0(),
                                ),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.send(
                        Command::MutateChatMessage {
                            channel_id: channel_id.clone(),
                            message_id: message_id.clone(),
                            action: MessageAction::Vote(choice_id.clone()),
                        },
                        None,
                        cx,
                    );
                }))
        }))
        .child(
            Label::new(votes(total))
                .text_xs()
                .text_color(theme.muted_foreground),
        )
}

fn votes(count: usize) -> String {
    let noun = if count == 1 { "vote" } else { "votes" };
    format!("{count} {noun}")
}
