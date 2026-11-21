use gpui_kit::base::Disableable;

use super::*;
use crate::{
    ui::video::camera_options,
    voice::video::{CameraDevice, CameraIntent, camera_devices},
};

#[derive(Default)]
pub(super) struct Preview {
    devices: Vec<CameraDevice>,
    active: Option<(CameraIntent, Task<()>)>,
    image: Option<Arc<RenderImage>>,
    rendered: Option<Arc<RenderImage>>,
    error: Option<&'static str>,
}

impl Preview {
    fn stop(&mut self) {
        self.active = None;
        self.image = None;
    }

    pub(super) fn release(&mut self, window: &mut Window) {
        self.stop();
        self.release_unused_image(window);
    }

    pub(super) fn release_unused_image(&mut self, window: &mut Window) {
        if self.rendered != self.image
            && let Some(previous) = std::mem::replace(&mut self.rendered, self.image.clone())
        {
            let _ = window.drop_image(previous);
        }
    }
}

impl Settings {
    pub(super) fn refresh_cameras(&mut self, cx: &mut Context<Self>) {
        let devices = cx.background_executor().spawn(async { camera_devices() });
        cx.spawn(async move |settings, cx| {
            let devices = devices.await;
            let _ = settings.update(cx, |settings, cx| {
                settings.camera.devices = devices;
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn sync_camera_preview(&mut self, cx: &App) {
        if let Some((active, _)) = &self.camera.active
            && (self.tab != Tab::Voice
                || !matches!(&self.spk.read(cx).view, View::SignedIn(sidebar)
                    if !sidebar.camera_publishing() && sidebar.voice_intent.camera == *active))
        {
            self.camera.stop();
            self.camera.error = None;
        }
    }

    fn toggle_camera_preview(&mut self, cx: &mut Context<Self>) {
        self.camera.error = None;
        if self.camera.active.is_some() {
            self.camera.stop();
            cx.notify();
            return;
        }
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return;
        };
        if sidebar.camera_publishing() {
            return;
        }
        let choice = sidebar.voice_intent.camera.clone();
        let (send_frames, frames) = async_channel::bounded(1);
        self.spk.update(cx, |app, cx| {
            app.send(Command::PreviewCamera(send_frames), None, cx);
        });
        let task = cx.spawn(async move |settings, cx| {
            let error = loop {
                match frames.recv().await {
                    Ok(Ok(image)) => {
                        if settings
                            .update(cx, |settings, cx| {
                                settings.camera.image = Some(image);
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(Err(error)) => break Some(error),
                    Err(_) => break None,
                }
            };
            let _ = settings.update(cx, |settings, cx| {
                settings.camera.stop();
                settings.camera.error = error;
                cx.notify();
            });
        });
        self.camera.active = Some((choice, task));
        cx.notify();
    }

    pub(super) fn camera_settings(
        &self,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let active = self.camera.active.is_some();
        let label = if active {
            "Stop Camera preview"
        } else {
            "Preview Camera"
        };
        card(
            "Camera",
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_3()
                .child(camera_options(sidebar, &self.spk, &self.camera.devices))
                .child(
                    Button::new("settings-camera-preview")
                        .small()
                        .tab_stop(true)
                        .disabled(sidebar.camera_publishing())
                        .label(label)
                        .on_click(
                            cx.listener(|settings, _, _, cx| settings.toggle_camera_preview(cx)),
                        ),
                )
                .when(active && self.camera.image.is_none(), |panel| {
                    panel.child(Label::new("Starting Camera preview…"))
                })
                .when_some(self.camera.image.clone(), |panel, frame| {
                    panel.child(
                        img(frame)
                            .w(px(320.0))
                            .max_w_full()
                            .h(px(180.0))
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .when_some(self.camera.error, |panel, error| {
                    panel.child(Label::new(error).text_color(cx.theme().danger))
                }),
            cx,
        )
    }
}
