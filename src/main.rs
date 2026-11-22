#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod api;
mod chat_markdown;
mod notifications;
mod session;
mod shortcuts;
mod sounds;
mod ui;
mod voice;

use std::borrow::Cow;

use gpui_kit::{
    component::{Theme, ThemeRegistry},
    *,
};

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
    let (activated, activations) = async_channel::unbounded::<notifications::NativeActivation>();
    let startup = activated.clone();
    // The macOS notification delegate must be registered before launch finishes.
    platform.on_system_notification_response(Box::new(move |response| {
        let _ = startup.try_send(response.into());
    }));
    Application::with_platform(platform)
        .with_assets(assets::AllAssets)
        .run(move |cx| {
            init(cx);
            load_fonts(cx);
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
            notifications::register_window(cx, window, app, base, activated, activations);
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
    std::process::ExitCode::SUCCESS
}

fn load_fonts(cx: &mut App) {
    const GEIST: &[&[u8]] = &[
        include_bytes!("../assets/fonts/Geist-Regular.ttf"),
        include_bytes!("../assets/fonts/Geist-Medium.ttf"),
        include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
        include_bytes!("../assets/fonts/Geist-Bold.ttf"),
        include_bytes!("../assets/fonts/Geist-Italic.ttf"),
        include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    ];
    let text = cx.text_system();
    if let Err(error) = text.add_fonts(GEIST.iter().copied().map(Cow::Borrowed).collect()) {
        eprintln!("Could not load embedded Geist fonts: {error}");
    }
    let families = text.all_font_names();
    Theme::update(cx, |theme| {
        if families.iter().any(|family| family == "Geist") {
            theme.font_family = "Geist".into();
        }
        if families.iter().any(|family| family == "Geist Mono") {
            theme.mono_font_family = "Geist Mono".into();
        }
    });
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
