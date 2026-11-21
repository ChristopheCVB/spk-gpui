use gpui_kit::base::Disableable;
use std::time::Duration;

use gpui_kit::component::{progress::Progress, slider::Slider, switch::Switch};

use super::*;
use crate::ui::voice::device_options;
use crate::voice::{
    Device, DeviceKind,
    audio_preview::{MicrophoneTest, test_playback},
};

impl Settings {
    fn preview_microphone(sidebar: &Sidebar) -> Option<Device> {
        sidebar
            .devices
            .microphones
            .preferred(sidebar.voice_intent.preferred.microphone.as_ref())
            .or_else(|| {
                sidebar
                    .devices
                    .microphones
                    .default
                    .as_ref()
                    .filter(|_| cfg!(windows))
            })
            .cloned()
    }

    pub(super) fn sync_mic_test(&mut self, cx: &App) {
        if let Some((device, _)) = &self.mic_test
            && (self.tab != Tab::Voice
                || !matches!(&self.spk.read(cx).view, View::SignedIn(sidebar)
                    if sidebar.voice.is_none() && Self::preview_microphone(sidebar) == *device))
        {
            self.mic_test = None;
            self.mic_level = 0;
        }
    }

    fn toggle_mic_test(&mut self, cx: &mut Context<Self>) {
        self.audio_test_error = None;
        self.mic_level = 0;
        if self.mic_test.take().is_some() {
            cx.notify();
            return;
        }
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return;
        };
        if sidebar.voice.is_some() {
            return;
        }
        let selected = Self::preview_microphone(sidebar);
        let device = selected.clone();
        let executor = cx.background_executor().clone();
        let task = cx.spawn(async move |settings, cx| {
            if let Some(test) = MicrophoneTest::start(device.as_ref()).await {
                while let Some(level) = test.level() {
                    if settings
                        .update(cx, |settings, cx| {
                            settings.mic_level = level;
                            cx.notify();
                        })
                        .is_err()
                    {
                        return;
                    }
                    executor.timer(Duration::from_millis(100)).await;
                }
            }
            let _ = settings.update(cx, |settings, cx| {
                settings.mic_test = None;
                settings.audio_test_error =
                    Some("Could not open the microphone. Check your audio device and permissions.");
                cx.notify();
            });
        });
        self.mic_test = Some((selected, task));
        cx.notify();
    }

    fn play_test_sound(&mut self, cx: &mut Context<Self>) {
        if self.playback_pending {
            return;
        }
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return;
        };
        let speakers = &sidebar.devices.speakers;
        let preferred = speakers.preferred(sidebar.voice_intent.preferred.speakers.as_ref());
        let active = sidebar
            .voice
            .as_ref()
            .and_then(|call| call.media.as_ref())
            .map_or_else(
                || preferred.or(speakers.default.as_ref()),
                |media| media.speakers.as_ref(),
            );
        let output = active
            .filter(|device| cfg!(windows) || Some(*device) != speakers.default.as_ref())
            .cloned();
        let volume = (sidebar.voice_intent.preferred.output_gain() * 100.0).round() as u8;
        self.playback_pending = true;
        self.audio_test_error = None;
        let (finished, result) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = finished.try_send(test_playback(output.as_ref(), volume).is_some());
        });
        cx.spawn(async move |settings, cx| {
            let played = result.recv().await == Ok(true);
            let _ = settings.update(cx, |settings, cx| {
                settings.playback_pending = false;
                settings.audio_test_error =
                    (!played).then_some("Could not play the test sound on the selected speakers.");
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn processing(&self, sidebar: &Sidebar, cx: &Context<Self>) -> impl IntoElement {
        let settings = sidebar.voice_intent.preferred.processing;
        let echo = settings.echo_cancellation;
        let SliderValue::Single(sensitivity) = self.sensitivity.read(cx).value() else {
            unreachable!("single microphone sensitivity slider");
        };
        let switch = |label: &'static str,
                      checked,
                      disabled,
                      setting: fn(bool) -> VoiceProcessingSetting| {
            let spk = self.spk.downgrade();
            Switch::new(label)
                .label(label)
                .accessibility_label(label)
                .checked(checked)
                .disabled(disabled)
                .tab_stop(true)
                .on_change(move |enabled, _, cx| {
                    let _ = spk.update(cx, |app, cx| {
                        app.send(Command::SetVoiceProcessing(setting(*enabled)), None, cx);
                    });
                })
        };
        card(
            "Voice processing",
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(switch("Echo cancellation", echo, false, VoiceProcessingSetting::EchoCancellation))
                .when(echo, |body| body.child(
                    Label::new("Turn off echo cancellation to adjust noise suppression, automatic gain, and sensitivity.")
                        .text_color(cx.theme().muted_foreground),
                ))
                .child(switch("Noise suppression (Standard)", echo || settings.noise_suppression, echo, VoiceProcessingSetting::NoiseSuppression))
                .child(switch("Automatic gain control", echo || settings.auto_gain, echo, VoiceProcessingSetting::AutoGain))
                .child(Label::new(format!("Sensitivity {sensitivity:.0}%")))
                .child(
                    Slider::new(&self.sensitivity)
                        .tab_stop(true)
                        .accessibility_label("Microphone sensitivity")
                        .disabled(echo),
                ),
            cx,
        )
    }

    pub(super) fn audio(&self, sidebar: &Sidebar, cx: &Context<Self>) -> impl IntoElement {
        let testing = self.mic_test.is_some();
        let volume = self.output_volume.read(cx).value();
        let SliderValue::Single(volume) = volume else {
            unreachable!("single output volume slider")
        };
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(card(
                "Microphone",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(device_options(
                        sidebar,
                        DeviceKind::Microphone,
                        &self.spk,
                        cx,
                    ))
                    .child(
                        Button::new("test-microphone")
                            .small()
                            .tab_stop(true)
                            .disabled(sidebar.voice.is_some())
                            .label(if testing {
                                "Stop microphone test"
                            } else {
                                "Test microphone"
                            })
                            .on_click(
                                cx.listener(|settings, _, _, cx| settings.toggle_mic_test(cx)),
                            ),
                    )
                    .when(sidebar.voice.is_some(), |body| {
                        body.child(
                            Label::new("Leave voice to test the microphone.")
                                .text_color(cx.theme().muted_foreground),
                        )
                    })
                    .when(testing, |body| {
                        body.child(
                            Progress::new("microphone-level")
                                .value(f32::from(self.mic_level))
                                .accessibility_label("Microphone input level"),
                        )
                    }),
                cx,
            ))
            .child(card(
                "Speakers",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(device_options(sidebar, DeviceKind::Speakers, &self.spk, cx))
                    .child(Label::new(format!("Test sound volume {volume:.0}%")))
                    .child(
                        Slider::new(&self.output_volume)
                            .tab_stop(true)
                            .accessibility_label("Test sound volume"),
                    )
                    .child(
                        Button::new("test-sound")
                            .small()
                            .tab_stop(true)
                            .disabled(self.playback_pending)
                            .label(if self.playback_pending {
                                "Playing test sound…"
                            } else {
                                "Test sound"
                            })
                            .on_click(
                                cx.listener(|settings, _, _, cx| settings.play_test_sound(cx)),
                            ),
                    ),
                cx,
            ))
            .child(self.processing(sidebar, cx))
            .children(
                self.audio_test_error
                    .map(|error| Label::new(error).text_color(cx.theme().danger)),
            )
            .child(self.voice_sounds(cx))
            .children(
                sidebar
                    .device_notice
                    .map(|notice| Label::new(notice).text_color(cx.theme().danger)),
            )
    }
}
