use gpui_kit::component::Disableable;

use super::*;
use crate::{
    api::SoundboardClip,
    session::{SoundboardAdminAction as Action, SoundboardCrop},
    ui::{
        icon_picker::{IconPicker, bundled_icon_names, icon_picker, icon_picker_field},
        soundboard::clip_icon,
    },
};

struct Metadata {
    name: Entity<InputState>,
    icon: IconPicker,
}

impl Metadata {
    fn new(icons: &[SharedString], window: &mut Window, cx: &mut App) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx)),
            icon: icon_picker(icons, "audio-lines", window, cx),
        }
    }

    fn values(&self, cx: &App) -> (String, String) {
        (
            self.name.read(cx).value().to_string(),
            self.icon
                .read(cx)
                .selected_value()
                .map_or_else(String::new, ToString::to_string),
        )
    }

    fn render(&self, original_icon: Option<&str>, disabled: bool, cx: &App) -> impl IntoElement {
        div()
            .flex()
            .gap_3()
            .child(field("Name", &self.name, disabled))
            .child(icon_picker_field(
                &self.icon,
                original_icon,
                clip_icon,
                disabled,
                cx,
            ))
    }
}

#[derive(Clone)]
enum ClipAction {
    Edit(SoundboardClip),
    Delete(SoundboardClip),
}

pub(super) struct SoundboardForm {
    upload: Metadata,
    edit: Metadata,
    start: Entity<InputState>,
    end: Entity<InputState>,
    gain: Entity<InputState>,
    picker_open: bool,
    picker_error: Option<&'static str>,
    clip_action: Option<ClipAction>,
    revision: u64,
}

impl SoundboardForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Settings>) -> Self {
        let icons = bundled_icon_names();
        let upload = Metadata::new(&icons, window, cx);
        let edit = Metadata::new(&icons, window, cx);
        for state in [&upload.icon, &edit.icon] {
            cx.observe(state, |_, _, cx| cx.notify()).detach();
        }
        Self {
            upload,
            edit,
            start: cx.new(|cx| InputState::new(window, cx).default_value("0")),
            end: cx.new(|cx| InputState::new(window, cx).placeholder("End of selection")),
            gain: cx.new(|cx| InputState::new(window, cx).default_value("100")),
            picker_open: false,
            picker_error: None,
            clip_action: None,
            revision: 0,
        }
    }

    fn crop(&self, duration: f64, cx: &App) -> SoundboardCrop {
        let start = self
            .start
            .read(cx)
            .value()
            .trim()
            .parse::<f64>()
            .unwrap_or(f64::NAN);
        let end = self.end.read(cx).value();
        let end = if end.trim().is_empty() {
            (start + 10.0).min(duration)
        } else {
            end.trim().parse().unwrap_or(f64::NAN)
        };
        let gain = self
            .gain
            .read(cx)
            .value()
            .trim()
            .parse()
            .unwrap_or(u16::MAX);
        SoundboardCrop { start, end, gain }
    }
}

impl Settings {
    pub(super) fn soundboard_action(&self, action: Action, cx: &mut Context<Self>) {
        self.spk.update(cx, |app, cx| {
            app.send(Command::AdminSoundboard(action), None, cx)
        });
    }

    pub(super) fn sync_soundboard_form(&mut self, cx: &mut Context<Self>) {
        if let View::SignedIn(sidebar) = &self.spk.read(cx).view {
            let board = &sidebar.admin_soundboard;
            if self.soundboard.revision != board.revision && board.notice.is_some() {
                self.soundboard.clip_action = None;
            }
            self.soundboard.revision = board.revision;
        }
    }

    fn choose_sound_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.soundboard.picker_open = true;
        self.soundboard.picker_error = None;
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose Soundboard audio".into()),
        });
        cx.spawn_in(window, async move |settings, cx| {
            let result = selection.await;
            let _ = settings.update_in(cx, |settings, window, cx| {
                let form = &mut settings.soundboard;
                form.picker_open = false;
                if settings.tab != Tab::Soundboard {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            let name: String = path
                                .file_stem()
                                .and_then(|name| name.to_str())
                                .unwrap_or("")
                                .chars()
                                .scan(0, |len, ch| {
                                    *len += ch.len_utf16();
                                    (*len <= 32).then_some(ch)
                                })
                                .collect();
                            form.upload
                                .name
                                .update(cx, |input, cx| input.set_value(name, window, cx));
                            form.start
                                .update(cx, |input, cx| input.set_value("0", window, cx));
                            form.end
                                .update(cx, |input, cx| input.set_value("", window, cx));
                            form.gain
                                .update(cx, |input, cx| input.set_value("100", window, cx));
                            settings.soundboard_action(Action::ChooseFile(path), cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => form.picker_error = Some("Could not open the file picker."),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn soundboard_admin(
        &self,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let board = &sidebar.admin_soundboard;
        let form = &self.soundboard;
        let busy = board.saving || board.decoding || board.loading || form.picker_open;
        let disabled = busy || board.previewing;
        let theme = cx.theme();
        let error = form
            .picker_error
            .or(board.error)
            .or(self.spk.read(cx).form_error);
        let status = if board.loading {
            Some("Loading sounds…")
        } else if board.saving {
            Some("Saving…")
        } else {
            board.notice
        };

        div().flex().flex_col().gap_4()
            .children(error.or(status).map(|message| div().id("soundboard-admin-status").role(Role::Status)
                .child(Label::new(message).text_color(if error.is_some() { theme.danger } else { theme.muted_foreground }))))
            .child(card("Add a sound", div().flex().flex_col().gap_3().items_start()
                .child(icon_label_button(
                    control_button("choose-sound-file").outline(), IconName::Upload,
                    if board.decoding { "Reading audio…" } else { "Choose audio file" },
                ).tab_stop(true).disabled(disabled)
                    .on_click(cx.listener(|settings, _, window, cx| settings.choose_sound_file(window, cx))))
                .child(Label::new("Choose a mono or stereo WAV, MP3, M4A, Ogg Vorbis, or FLAC file up to 20 MB. SPK receives only the selected audio.").text_color(theme.muted_foreground))
                .children(board.duration.map(|duration| {
                    div().w_full().flex().flex_col().gap_3()
                        .child(Label::new(format!("Source duration {:.3} s. Select up to 10 seconds. Leave End empty for the longest selection.", duration)))
                        .child(div().flex().gap_3()
                            .child(field("Start (seconds)", &form.start, disabled))
                            .child(field("End (seconds)", &form.end, disabled))
                            .child(field("Upload gain (%)", &form.gain, disabled)))
                        .child(form.upload.render(None, disabled, cx))
                        .child(div().flex().gap_2()
                            .child(control_button("preview-crop").label("Play selection").tab_stop(true).disabled(disabled)
                                .on_click(cx.listener(move |settings, _, _, cx| {
                                    let crop = settings.soundboard.crop(duration, cx);
                                    settings.soundboard_action(Action::Preview(crop), cx);
                                })))
                            .child(control_button("save-sound").label("Save").primary().tab_stop(true).disabled(disabled)
                                .on_click(cx.listener(move |settings, _, _, cx| {
                                    let crop = settings.soundboard.crop(duration, cx);
                                    let (name, icon) = settings.soundboard.upload.values(cx);
                                    settings.soundboard_action(Action::Add { crop, name, icon }, cx);
                                })))
                            .child(control_button("discard-sound").label("Discard").ghost().tab_stop(true).disabled(disabled)
                                .on_click(cx.listener(|settings, _, _, cx| settings.soundboard_action(Action::Discard, cx)))))
                })), cx))
            .children(board.previewing.then(|| control_button("stop-sound-preview").label("Stop preview").tab_stop(true)
                .on_click(cx.listener(|settings, _, _, cx| settings.soundboard_action(Action::StopPreview, cx)))))
            .child(card("Sounds", div().flex().flex_col().gap_3()
                .when(!board.loading && (board.clips.is_none() || board.error.is_some()), |body| body.child(
                    control_button("reload-sounds").label("Reload sounds").tab_stop(true).disabled(disabled)
                        .on_click(cx.listener(|settings, _, _, cx| settings.soundboard_action(Action::Show, cx)))))
                .when(board.clips.as_ref().is_some_and(|clips| clips.is_empty()), |body| body.child(Label::new("No sounds yet.")))
                .children(form.clip_action.as_ref().map(|action| self.sound_clip_editor(action, disabled, cx)))
                .children(board.clips.iter().flat_map(|clips| clips.iter()).map(|clip| self.admin_clip_row(clip, disabled, cx))), cx))
    }

    fn admin_clip_row(
        &self,
        clip: &SoundboardClip,
        disabled: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let id = clip.id.clone();
        let edit = clip.clone();
        let delete = clip.clone();
        div()
            .id(SharedString::from(clip.id.clone()))
            .flex()
            .items_center()
            .gap_2()
            .child(clip_icon(&clip.icon))
            .child(Label::new(clip.name.clone()).truncate().flex_1().min_w_0())
            .child(Label::new(format!("{:.1} s", f64::from(clip.duration_ms) / 1000.0)).text_xs())
            .child(
                tool_button(
                    "preview",
                    IconName::Headphones,
                    format!("Preview {} privately", clip.name),
                )
                .disabled(disabled)
                .on_click(cx.listener(move |settings, _, _, cx| {
                    settings.soundboard_action(Action::PreviewSaved(id.clone()), cx)
                })),
            )
            .child(
                tool_button("edit", IconName::Pencil, format!("Edit {}", clip.name))
                    .disabled(disabled)
                    .on_click(cx.listener(move |settings, _, window, cx| {
                        settings.soundboard.edit.name.update(cx, |input, cx| {
                            input.set_value(edit.name.clone(), window, cx)
                        });
                        settings.soundboard.edit.icon.update(cx, |select, cx| {
                            select.set_selected_value(&edit.icon.clone().into(), window, cx);
                        });
                        settings.soundboard.clip_action = Some(ClipAction::Edit(edit.clone()));
                        cx.notify();
                    })),
            )
            .child(
                tool_button("delete", IconName::Trash, format!("Delete {}", clip.name))
                    .disabled(disabled)
                    .on_click(cx.listener(move |settings, _, _, cx| {
                        settings.soundboard.clip_action = Some(ClipAction::Delete(delete.clone()));
                        cx.notify();
                    })),
            )
    }

    fn sound_clip_editor(
        &self,
        action: &ClipAction,
        disabled: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (label, input, button) = match action {
            ClipAction::Edit(clip) => {
                let id = clip.id.clone();
                let original_icon = clip.icon.clone();
                (
                    format!("Edit {}", clip.name),
                    Some(self.soundboard.edit.render(Some(&clip.icon), disabled, cx)),
                    control_button("apply-sound-edit")
                        .primary()
                        .label("Save")
                        .on_click(cx.listener(move |settings, _, _, cx| {
                            let (name, icon) = settings.soundboard.edit.values(cx);
                            settings.soundboard_action(
                                Action::Edit {
                                    id: id.clone(),
                                    name,
                                    icon: if icon.is_empty() {
                                        original_icon.clone()
                                    } else {
                                        icon
                                    },
                                },
                                cx,
                            );
                        })),
                )
            }
            ClipAction::Delete(clip) => {
                let id = clip.id.clone();
                (
                    format!("Delete “{}”? This can’t be undone.", clip.name),
                    None,
                    control_button("confirm-sound-delete")
                        .danger()
                        .label("Delete")
                        .on_click(cx.listener(move |settings, _, _, cx| {
                            settings.soundboard_action(Action::Delete(id.clone()), cx)
                        })),
                )
            }
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(Label::new(label).font_semibold())
            .children(input)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button.disabled(disabled).tab_stop(true))
                    .child(
                        control_button("cancel-sound-change")
                            .label("Cancel")
                            .ghost()
                            .tab_stop(true)
                            .disabled(disabled)
                            .on_click(cx.listener(|settings, _, _, cx| {
                                settings.soundboard.clip_action = None;
                                cx.notify();
                            })),
                    ),
            )
    }
}

fn field(label: &'static str, state: &Entity<InputState>, disabled: bool) -> impl IntoElement {
    div()
        .id(label)
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(Label::new(label))
        .child(
            Input::new(state)
                .w_full()
                .aria_label(label)
                .disabled(disabled),
        )
}
