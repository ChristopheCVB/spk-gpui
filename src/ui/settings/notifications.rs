use gpui_kit::component::{slider::Slider, switch::Switch};

use super::*;

impl Settings {
    pub(super) fn notifications(&self, cx: &Context<Self>) -> impl IntoElement {
        let app = self.spk.read(cx);
        let settings = &app.notification_preferences;
        let sound = app.shortcuts.sound_settings().get(Sound::Message);
        let SliderValue::Single(volume) = self.message_volume.read(cx).value() else {
            unreachable!("single message volume slider");
        };

        let dms = Switch::new("notify-dms")
            .label("Direct messages")
            .checked(settings.dms)
            .tab_stop(true)
            .on_click(cx.listener(|this, enabled, _, cx| {
                this.spk.update(cx, |app, cx| {
                    app.change_notifications(|preferences| preferences.dms = *enabled, cx);
                });
            }));
        let pings = Switch::new("notify-pings")
            .label("Mentions and replies")
            .checked(settings.pings)
            .tab_stop(true)
            .on_click(cx.listener(|this, enabled, _, cx| {
                this.spk.update(cx, |app, cx| {
                    app.change_notifications(|preferences| preferences.pings = *enabled, cx);
                });
            }));
        let previews = Switch::new("notify-previews")
            .label("Show system notification previews")
            .checked(settings.system_previews)
            .tab_stop(true)
            .on_click(cx.listener(|this, enabled, _, cx| {
                this.spk.update(cx, |app, cx| {
                    app.change_notifications(
                        |preferences| preferences.system_previews = *enabled,
                        cx,
                    );
                });
            }));
        let sound_switch = Switch::new("notify-sound")
            .label("Message sound")
            .checked(sound.enabled)
            .tab_stop(true)
            .on_click(cx.listener(|this, enabled, _, cx| {
                this.spk.update(cx, |app, cx| {
                    app.change_sound(Sound::Message, |sound| sound.enabled = *enabled, cx);
                });
            }));

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(card(
                "What notifies you",
                div().flex().flex_col().gap_3().child(dms).child(pings),
                cx,
            ))
            .child(card(
                "System notifications",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(previews)
                    .child(Label::new(
                        "Manage desktop notification permissions in your system settings.",
                    )),
                cx,
            ))
            .child(card(
                "Sound",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(sound_switch)
                    .child(Label::new(format!("Volume {volume:.0}%")))
                    .child(
                        Slider::new(&self.message_volume)
                            .disabled(!sound.enabled)
                            .tab_stop(true)
                            .accessibility_label("Message sound volume"),
                    )
                    .child(
                        control_button("test-message-sound")
                            .ghost()
                            .small()
                            .tab_stop(true)
                            .label("Test sound")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.spk.read(cx).cues.play_setting(
                                    Sound::Message,
                                    true,
                                    SoundSetting {
                                        enabled: true,
                                        volume: volume as u8,
                                    },
                                );
                            })),
                    ),
                cx,
            ))
            .children(app.shortcuts.settings_error.map(Label::new))
    }
}
