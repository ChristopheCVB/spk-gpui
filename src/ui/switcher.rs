use std::cell::Cell;

use gpui_kit::component::command::{
    Command as CommandPalette, CommandGroup, CommandItem, CommandState,
};

use super::{
    workspace::{channel_name, voice_channels},
    *,
};
use crate::api::Channel;

#[derive(Clone, Copy)]
enum Group {
    Text,
    Direct,
    Voice,
    Webhooks,
    People,
    Actions,
}

impl Group {
    const ALL: [Self; 6] = [
        Self::Text,
        Self::Direct,
        Self::Voice,
        Self::Webhooks,
        Self::People,
        Self::Actions,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Text => "Text channels",
            Self::Direct => "Direct messages",
            Self::Voice => "Voice channels",
            Self::Webhooks => "Webhooks",
            Self::People => "People",
            Self::Actions => "Actions",
        }
    }

    fn icon(self, feed_icon: Option<&str>) -> Icon {
        Icon::new(match self {
            Self::Text => IconName::Hash,
            Self::Direct => IconName::MessageCircle,
            Self::Voice => IconName::Volume2,
            Self::Webhooks => return webhook_icon(feed_icon),
            Self::People => IconName::User,
            Self::Actions => IconName::Settings,
        })
    }
}

#[derive(Clone)]
enum Destination {
    Text(String),
    Direct(String),
    Voice(String),
    Webhook(String),
    Person(String),
    Settings,
    Shortcuts,
    Theme,
}

impl Destination {
    fn channel(channel: &Channel) -> Option<(Group, Self)> {
        let id = channel.id.clone();
        match channel.kind.as_str() {
            "text" => Some((Group::Text, Self::Text(id))),
            "dm" if channel.is_conversation() => Some((Group::Direct, Self::Direct(id))),
            "voice" => Some((Group::Voice, Self::Voice(id))),
            "webhook" => Some((Group::Webhooks, Self::Webhook(id))),
            _ => None,
        }
    }

    fn open(self, app: &mut Spk, window: &mut Window, cx: &mut Context<Spk>) {
        app.device_picker = None;
        match self {
            Self::Text(id) => app.send(Command::OpenTextChannel(id), None, cx),
            Self::Direct(id) => app.send(Command::OpenDirectMessage(id), None, cx),
            Self::Voice(id) => app.select_voice_channel(&id, cx),
            Self::Webhook(id) => app.send(Command::OpenWebhook(id), None, cx),
            Self::Person(id) => app.send(Command::StartDirectMessage(id), None, cx),
            Self::Settings => app.open_settings(window, cx),
            Self::Shortcuts => app.open_shortcut_reference(window, cx),
            Self::Theme => app.set_appearance(
                if cx.theme().mode.is_dark() {
                    Appearance::Light
                } else {
                    Appearance::Dark
                },
                window,
                cx,
            ),
        }
    }
}

struct Entry {
    label: String,
    destination: Destination,
    icon: Icon,
}

impl Spk {
    pub(super) fn open_quick_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let mut groups: [Vec<Entry>; Group::ALL.len()] = std::array::from_fn(|_| Vec::new());
        for channel in sidebar.channels.iter().flatten() {
            if let Some((group, destination)) = Destination::channel(channel) {
                let label = channel.other_user_id.as_deref().map_or_else(
                    || channel_name(channel),
                    |peer| sidebar.message_author(peer),
                );
                groups[group as usize].push(Entry {
                    label: label.to_owned(),
                    destination,
                    icon: group.icon(channel.icon.as_deref()),
                });
            }
        }
        groups[Group::People as usize].extend(sidebar.people().into_iter().map(|(id, name)| {
            Entry {
                label: name.to_owned(),
                destination: Destination::Person(id.to_owned()),
                icon: Group::People.icon(None),
            }
        }));
        groups[Group::Actions as usize].extend(
            [
                ("Settings", Destination::Settings),
                ("Keyboard shortcuts", Destination::Shortcuts),
                ("Toggle theme", Destination::Theme),
            ]
            .map(|(label, destination)| Entry {
                label: label.to_owned(),
                destination,
                icon: Group::Actions.icon(None),
            }),
        );
        let state = cx.new(|cx| CommandState::new(window, cx));
        let app = cx.weak_entity();
        let entries = std::rc::Rc::new(groups);
        let first_render = Cell::new(true);
        self.quick_switcher = Some(state.downgrade());
        window.open_dialog(cx, move |dialog, window, cx| {
            if first_render.replace(false) {
                state.read(cx).focus_handle(cx).focus(window, cx);
            }
            let entries_for_confirm = entries.clone();
            let app = app.clone();
            let mut palette = CommandPalette::new(&state)
                .searchable(true)
                .placeholder("Jump to…")
                .max_h(window.viewport_size().height * 0.48)
                .empty(|_, _, _| Label::new("No matching destinations.").p_2())
                .on_cancel({
                    let app = app.clone();
                    move |_, cx| {
                        let _ = app.update(cx, |app, _| app.quick_switcher = None);
                    }
                })
                .on_confirm(move |index, window, cx| {
                    let Some(entry) = entries_for_confirm
                        .get(index.section)
                        .and_then(|group| group.get(index.row))
                    else {
                        return;
                    };
                    window.close_dialog(cx);
                    let _ = app.update(cx, |app, cx| {
                        app.quick_switcher = None;
                        entry.destination.clone().open(app, window, cx);
                    });
                });
            for (group, entries) in Group::ALL.into_iter().zip(entries.iter()) {
                palette = palette.group(CommandGroup::new().label(group.label()).items(
                    entries.iter().map(|entry| {
                        let label = entry.label.clone();
                        let icon = entry.icon.clone();
                        CommandItem::new()
                            .label(label.clone())
                            .child(move |_, _| {
                                control_content()
                                    .child(icon.clone().size(ACTION_ICON_SIZE))
                                    .child(div().min_w_0().truncate().child(label.clone()))
                            })
                            .keywords([group.label()])
                    }),
                ));
            }
            dialog
                .centered()
                .width(px(460.0))
                .title("Quick Switcher")
                .child(palette)
        });
    }

    pub(super) fn navigate_channel(
        &mut self,
        backwards: bool,
        unread_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let current = sidebar
            .chat
            .as_ref()
            .map(|chat| chat.channel_id.as_str())
            .or(self.selected_voice_channel.as_deref())
            .or_else(|| {
                voice_channels(sidebar)
                    .next()
                    .map(|channel| channel.id.as_str())
            });
        let channels = ["dm", "text", "voice", "webhook"]
            .into_iter()
            .flat_map(|kind| {
                sidebar
                    .channels
                    .iter()
                    .flatten()
                    .filter(move |channel| channel.kind == kind)
            })
            .filter(|channel| channel.kind == "voice" || channel.is_conversation())
            .collect::<Vec<_>>();
        let ids: Vec<_> = channels.iter().map(|channel| channel.id.as_str()).collect();
        if let Some(index) = step_channel(&ids, current, backwards, |id| {
            !unread_only || (Some(id) != current && sidebar.unread(id).unread > 0)
        }) && let Some((_, target)) = Destination::channel(channels[index])
        {
            target.open(self, window, cx);
        }
    }
}

fn step_channel(
    ids: &[&str],
    current: Option<&str>,
    backwards: bool,
    eligible: impl Fn(&str) -> bool,
) -> Option<usize> {
    let len = ids.len();
    let current = current.and_then(|id| ids.iter().position(|&candidate| candidate == id));
    (0..len)
        .map(|step| {
            let direction = if backwards { len - step - 1 } else { step };
            current.map_or(direction, |index| {
                (index + direction + usize::from(!backwards)) % len
            })
        })
        .find(|&index| eligible(ids[index]))
}

#[cfg(test)]
mod tests {
    use super::step_channel;

    #[test]
    fn keyboard_navigation_wraps_and_can_skip_read_channels() {
        let channels = ["dm", "text", "voice", "webhook"];
        assert_eq!(step_channel(&channels, Some("dm"), true, |_| true), Some(3));
        assert_eq!(
            step_channel(&channels, Some("voice"), false, |_| true),
            Some(3)
        );
        assert_eq!(
            step_channel(&channels, Some("webhook"), false, |_| true),
            Some(0)
        );
        assert_eq!(step_channel(&channels, None, false, |_| true), Some(0));
        assert_eq!(step_channel(&channels, None, true, |_| true), Some(3));
        assert_eq!(
            step_channel(&channels, Some("dm"), false, |id| id == "voice"),
            Some(2)
        );
        assert_eq!(step_channel(&channels, Some("dm"), true, |_| false), None);
        assert_eq!(step_channel(&[], None, false, |_| true), None);
    }
}
