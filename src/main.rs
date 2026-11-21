#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod api;
mod session;
mod shortcuts;
mod sounds;
mod ui;
mod voice;

use gpui_kit::{component::ThemeRegistry, *};

use ui::Spk;

fn main() {
    application().with_assets(assets::AllAssets).run(|cx| {
        init(cx);
        gpui_kit::base::Root::register_plugin(cx, |_, _| ui::ShortcutKeys);
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(include_str!("../assets/themes.json"))
            .expect("SPK themes should be valid");

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1200.0), px(760.0)), cx)),
            window_min_size: Some(size(px(560.0), px(440.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("SPK".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        open_window(options, cx, |window, cx| cx.new(|cx| Spk::new(window, cx)))
            .expect("Failed to open the SPK window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
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
