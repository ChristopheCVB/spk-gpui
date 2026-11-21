#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod api;
mod notifications;
mod session;
mod shortcuts;
mod sounds;
mod ui;
mod voice;

use gpui_kit::{component::ThemeRegistry, *};

use ui::Spk;

const APP_ID: &str = "dev.meeehdi.spk.desktop";

fn main() -> std::process::ExitCode {
    if let Some(exit) = voice::video::run_camera_command() {
        return exit;
    }
    let base = url::Url::parse(env!("SPK_URL")).expect("validated SPK URL");
    let initial_alert = std::env::args_os()
        .nth(1)
        .and_then(|tag| notifications::activation_target(&base, tag.to_str()?));

    let platform = platform::current_platform(false);
    platform.set_app_identity(APP_ID, "SPK");
    let (activated, activations) = async_channel::unbounded::<SystemNotificationResponse>();
    let startup = activated.clone();
    // The macOS notification delegate must be registered before launch finishes.
    platform.on_system_notification_response(Box::new(move |response| {
        let _ = startup.try_send(response);
    }));
    Application::with_platform(platform)
        .with_assets(assets::AllAssets)
        .run(move |cx| {
            init(cx);
            gpui_kit::base::Root::register_plugin(cx, |_, _| ui::ShortcutKeys);
            ThemeRegistry::global_mut(cx)
                .load_themes_from_str(include_str!("../assets/themes.json"))
                .expect("SPK themes should be valid");

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1200.0), px(760.0)), cx)),
                window_min_size: Some(size(px(560.0), px(440.0))),
                app_id: Some(APP_ID.into()),
                titlebar: Some(TitlebarOptions {
                    title: Some("SPK".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let (window, app) = open_window(options, cx, |window, cx| {
                cx.new(|cx| Spk::new(initial_alert, window, cx))
            })
            .expect("Failed to open the SPK window");
            let app = app.downgrade();
            // GPUI Kit installs its own handler at init. SPK's system alerts use exact targets.
            cx.on_system_notification_response(move |response, _| {
                let _ = activated.try_send(response);
            });
            cx.spawn(async move |cx| {
                while let Ok(response) = activations.recv().await {
                    if response
                        .action_id
                        .as_deref()
                        .is_some_and(|action| action != "open")
                    {
                        continue;
                    }
                    let Some(target) = notifications::activation_target(&base, &response.tag)
                    else {
                        continue;
                    };
                    cx.update(|cx| {
                        cx.dismiss_system_notification(&response.tag);
                        cx.activate(true);
                        let _ = window.update(cx, |_, window, cx| {
                            window.activate_window();
                            let _ = app.update(cx, |this, cx| this.open_alert(target, window, cx));
                        });
                    });
                }
            })
            .detach();
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use gpui_kit::{
        AssetSource,
        assets::{AllAssets, IconName},
    };
    use std::borrow::Cow;

    #[test]
    fn desktop_icons_are_embedded() {
        for icon in [
            IconName::Hash,
            IconName::Bell,
            IconName::Settings,
            IconName::User,
            IconName::X,
            IconName::PhoneOff,
            IconName::Mic,
            IconName::MicOff,
            IconName::Headphones,
            IconName::HeadphoneOff,
        ] {
            let path = icon.path();
            let data = AllAssets.load(&path).unwrap();
            assert!(
                matches!(data, Some(Cow::Borrowed(svg)) if !svg.is_empty()),
                "{path} should be embedded"
            );
        }
    }
}
