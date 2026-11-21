use gpui_kit::base::Disableable;

use super::*;
use crate::{
    ui::video::camera_options,
    voice::video::{CameraDevice, CameraIntent, CameraStatus, camera_devices, preview_camera},
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

    pub(super) fn release_unused_image(&mut self, window: &mut Window) {
        if self.rendered != self.image {
            if let Some(previous) = std::mem::replace(&mut self.rendered, self.image.clone()) {
                let _ = window.drop_image(previous);
            }
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
                    if !camera_publishing(sidebar) && sidebar.voice_intent.camera == *active))
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
        if camera_publishing(sidebar) {
            return;
        }
        let choice = sidebar.voice_intent.camera.clone();
        let frames = preview_camera(choice.clone());
        let task = cx.spawn(async move |settings, cx| {
            while let Ok(frame) = frames.recv().await {
                let failed = frame.is_err();
                if settings
                    .update(cx, |settings, cx| {
                        match frame {
                            Ok(image) => settings.camera.image = Some(image),
                            Err(error) => {
                                settings.camera.active = None;
                                settings.camera.image = None;
                                settings.camera.error = Some(error);
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                    || failed
                {
                    return;
                }
            }
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
                        .disabled(camera_publishing(sidebar))
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

fn camera_publishing(sidebar: &Sidebar) -> bool {
    sidebar.voice_intent.camera.enabled
        && !sidebar
            .voice
            .as_ref()
            .and_then(|call| call.video.as_ref())
            .is_some_and(|report| matches!(report.camera, CameraStatus::Failed(_)))
}
