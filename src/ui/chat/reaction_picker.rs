use super::{actions::ReactionSelection, *};

use gpui_kit::{
    base::{Align, Positioner},
    component::{Placement, ThemeStyled},
};
use serde::Deserialize;

const ROW_HEIGHT: Pixels = CONTROL_HEIGHT;
const POPOVER_MARGIN: Pixels = px(8.);
const POPOVER_GAP: Pixels = px(4.);

#[derive(Deserialize, PartialEq, Eq)]
pub(super) struct Emoji {
    pub(super) emoji: String,
    pub(super) annotation: String,
    pub(super) shortcodes: Vec<String>,
    pub(super) tags: Vec<String>,
}

pub(super) static EMOJI: LazyLock<Vec<Emoji>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../assets/emoji/emojibase-en.json"))
        .expect("bundled Emojibase catalog must be valid")
});

fn emoji_results(query: &str) -> Vec<&'static Emoji> {
    let query = query.trim().trim_matches(':').to_lowercase();
    if query.is_empty() {
        return EMOJI.iter().collect();
    }
    let shortcode = query.replace(' ', "_");
    let mut matches: Vec<_> = EMOJI
        .iter()
        .filter_map(|emoji| {
            let name = emoji.annotation.to_lowercase();
            let rank = std::iter::once((name.as_str(), query.as_str(), 0))
                .chain(
                    emoji
                        .shortcodes
                        .iter()
                        .map(|name| (name.as_str(), shortcode.as_str(), 1)),
                )
                .chain(
                    emoji
                        .tags
                        .iter()
                        .map(|tag| (tag.as_str(), query.as_str(), 2)),
                )
                .filter_map(|(candidate, query, field)| {
                    let offset = candidate.find(query)?;
                    Some((candidate != query, offset != 0, field))
                })
                .min()?;
            Some((rank, emoji))
        })
        .collect();
    matches.sort_by_key(|(rank, _)| *rank);
    matches.into_iter().map(|(_, emoji)| emoji).collect()
}

pub(in crate::ui) struct ReactionPicker {
    pub(super) channel_id: String,
    pub(super) message_id: String,
    return_focus: FocusHandle,
    results: Rc<Vec<&'static Emoji>>,
    scroll: ListState,
    active: usize,
}

impl ReactionPicker {
    pub(super) fn belongs_to(&self, channel_id: &str, message_id: &str) -> bool {
        self.channel_id == channel_id && self.message_id == message_id
    }
}

impl Spk {
    pub(super) fn open_reaction_picker(
        &mut self,
        channel_id: &str,
        message_id: &str,
        return_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !active_chat_message(chat(&self.view), channel_id, message_id)
            .is_some_and(|(chat, _)| !chat.action_pending)
        {
            return;
        }
        self.reaction_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        let results = Rc::new(emoji_results(""));
        self.reaction_picker = Some(ReactionPicker {
            channel_id: channel_id.to_owned(),
            message_id: message_id.to_owned(),
            return_focus,
            scroll: ListState::new(results.len(), ListAlignment::Top, ROW_HEIGHT)
                .with_uniform_item_height(ROW_HEIGHT),
            results,
            active: 0,
        });
        cx.notify();
    }

    pub(in crate::ui) fn update_reaction_search(&mut self, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.reaction_picker {
            picker.results = Rc::new(emoji_results(&self.reaction_input.read(cx).value()));
            picker
                .scroll
                .reset_with_uniform_height(picker.results.len(), ROW_HEIGHT);
            picker.active = 0;
            cx.notify();
        }
    }

    pub(super) fn close_reaction_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(picker) = self.reaction_picker.take() else {
            return false;
        };
        picker.return_focus.focus(window, cx);
        cx.notify();
        true
    }

    fn pick_reaction(&mut self, emoji: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.reaction_picker.take() {
            picker.return_focus.focus(window, cx);
            self.toggle_reaction(
                &picker.channel_id,
                &picker.message_id,
                emoji,
                ReactionSelection::Choice,
                cx,
            );
            cx.notify();
        }
    }

    pub(in crate::ui) fn reaction_picker_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(picker) = &mut self.reaction_picker else {
            return false;
        };
        if !self.reaction_input.focus_handle(cx).is_focused(window) {
            return false;
        }
        match key {
            "up" | "down" => {
                picker.active = if key == "up" {
                    picker.active.saturating_sub(1)
                } else {
                    (picker.active + 1).min(picker.results.len().saturating_sub(1))
                };
                if !picker.results.is_empty() {
                    picker.scroll.scroll_to_reveal_item(picker.active);
                    cx.notify();
                }
            }
            "enter" => {
                if let Some(&result) = picker.results.get(picker.active) {
                    self.pick_reaction(&result.emoji, window, cx);
                }
            }
            _ => return false,
        }
        true
    }

    pub(super) fn reaction_picker_anchor(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let picker = self.reaction_picker.as_ref()?;
        let results = picker.results.clone();
        let active = picker.active;
        let scroll = picker.scroll.clone();
        let app = cx.weak_entity();
        let empty = results.is_empty();
        let surface = div()
            .id("reaction-picker")
            .role(Role::Group)
            .aria_label("Add reaction")
            .occlude()
            .popover_style(cx)
            .shadow(floating_shadow(cx))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                if this.close_reaction_picker(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                surface_input(&self.reaction_input, cx.theme().background, cx)
                    .aria_label("Search emoji"),
            )
            .when(empty, |panel| {
                panel.child(
                    div()
                        .id("reaction-search-status")
                        .role(Role::Status)
                        .p_2()
                        .child(
                            Label::new("No emoji found").text_color(cx.theme().muted_foreground),
                        ),
                )
            })
            .when(!empty, |panel| {
                panel.child(
                    div()
                        .relative()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .child(
                            list(scroll.clone(), move |index, _, _| {
                                let emoji = results[index];
                                let app = app.clone();
                                let item = control_button(index)
                                    .ghost()
                                    .w_full()
                                    .h(ROW_HEIGHT)
                                    .tab_stop(false)
                                    .accessibility_label(emoji.annotation.clone())
                                    .selected(index == active)
                                    .child(
                                        div()
                                            .w_full()
                                            .flex()
                                            .items_center()
                                            .gap(CONTROL_PADDING)
                                            .min_w_0()
                                            .child(reaction_glyph(&emoji.emoji, ACTION_ICON_SIZE))
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_ellipsis()
                                                    .child(emoji.annotation.clone()),
                                            ),
                                    )
                                    .on_click(move |_, window, cx| {
                                        let _ = app.update(cx, |this, cx| {
                                            this.pick_reaction(&emoji.emoji, window, cx);
                                        });
                                    });
                                div()
                                    .pr(OVERLAY_SCROLLBAR_WIDTH)
                                    .child(item)
                                    .into_any_element()
                            })
                            .w_full()
                            .h_full(),
                        )
                        .child(overlay_scrollbar(&scroll)),
                )
            });
        Some(
            canvas(
                move |trigger, window, cx| {
                    // Measure after the toolbar lays out so the popup follows its button without resizing the row.
                    let viewport = window.viewport_size();
                    let margin = POPOVER_MARGIN + window.client_inset().unwrap_or_default();
                    let available_height = trigger.top().max(viewport.height - trigger.bottom())
                        - margin
                        - POPOVER_GAP;
                    let mut popup = Positioner::side(trigger)
                        .placement(Placement::Top)
                        .align(Align::End)
                        .offset(POPOVER_GAP)
                        .margin(POPOVER_MARGIN)
                        .child(
                            surface
                                .w(px(320.).min(viewport.width - margin * 2.))
                                .h(px(384.).min(available_height)),
                        )
                        .into_any_element();
                    popup.layout_as_root(viewport.map(AvailableSpace::Definite), window, cx);
                    window.defer_draw(popup, Point::default(), 1, None);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::emoji_results;

    #[test]
    fn emoji_search_browses_and_ranks_matches_by_name_shortcode_and_keyword() {
        assert_eq!(emoji_results("  ")[0].annotation, "grinning face");
        assert_eq!(emoji_results(" Grinning Face ")[0].emoji, "😀");
        assert_eq!(emoji_results(" :THUMBSUP: ")[0].annotation, "thumbs up");
        assert_eq!(
            emoji_results(":stuck out tongue:")[0].annotation,
            "face with tongue"
        );
        assert_eq!(emoji_results("pepperoni")[0].annotation, "pizza");
        let party = emoji_results("party");
        let position = |name| {
            party
                .iter()
                .position(|emoji| emoji.annotation == name)
                .unwrap()
        };
        assert!(position("party popper") < position("face with tongue"));
        assert!(position("face with tongue") < position("partying face"));
        assert!(emoji_results("zzz no matching emoji").is_empty());
    }
}
