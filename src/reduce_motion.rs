use ashpd::desktop::settings::Settings;
use futures::StreamExt;
use gpui_kit::App;

pub fn follow_gtk_fallback(cx: &mut App) {
    cx.spawn(async move |cx| {
        let settings = Settings::new().await.ok()?;
        if settings.reduced_motion().await.is_ok() {
            return Some(());
        }

        // Older GTK portals expose this preference under GNOME's settings namespace.
        let namespace = "org.gnome.desktop.interface";
        let key = "enable-animations";
        let mut changes = settings
            .receive_setting_changed_with_args::<bool>(namespace, key)
            .await
            .ok()?;
        let enabled = settings.read::<bool>(namespace, key).await.ok()?;
        cx.update(|cx| cx.set_reduce_motion(!enabled));
        while let Some(enabled) = changes.next().await {
            if let Ok(enabled) = enabled {
                cx.update(|cx| cx.set_reduce_motion(!enabled));
            }
        }
        Some(())
    })
    .detach();
}
