use std::{collections::HashMap, fs, io, io::Write as _, path::Path};

use gpui_kit::{
    AppContext, Context, Global, SystemNotification, SystemNotificationResponse, Window,
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::{
    Connection, Dispatch, Proxy as _, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_surface::WlSurface},
};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;
use zbus::{OwnedVariant, Proxy, Variant, connection::Builder, fdo::RequestNameFlags};

use super::NativeActivation;
use crate::{APP_ID, ui::Spk};

const OBJECT_PATH: &str = "/dev/meeehdi/spk/desktop";
const OPEN_ACTION: &str = "app.open";

pub struct Notifications {
    portal: Option<(Proxy<'static>, bool)>,
    // Keep the exported application object alive even if the portal is unavailable.
    _connection: zbus::Connection,
}

impl Global for Notifications {}

impl Notifications {
    pub async fn register(activated: async_channel::Sender<NativeActivation>) -> Option<Self> {
        let connection = Builder::session()
            .serve_at(OBJECT_PATH, Application { activated })
            .build()
            .await
            .ok()?;
        connection
            .request_name_with_flags(APP_ID, RequestNameFlags::DO_NOT_QUEUE)
            .await
            .ok()?;
        let portal = async {
            register_desktop(&dirs::data_dir()?, &std::env::current_exe().ok()?).ok()?;
            // shortcut: registration lasts for this portal process, renew it if the owner changes.
            ashpd::register_host_app_with_connection(connection.clone(), APP_ID.try_into().ok()?)
                .await
                .ok()?;
            let portal = Proxy::new_owned(
                connection.clone(),
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Notification",
            )
            .await
            .ok()?;
            let explicit_silence = portal.get_property::<u32>("version").await.ok()? >= 2;
            if !explicit_silence && !notifications_are_silent_without_hints(&connection).await {
                return None;
            }
            Some((portal, explicit_silence))
        }
        .await;
        Some(Self {
            portal,
            _connection: connection,
        })
    }

    pub fn show(notification: &SystemNotification, cx: &mut Context<Spk>) -> bool {
        let Some((portal, explicit_silence)) = cx
            .try_global::<Self>()
            .and_then(|state| state.portal.clone())
        else {
            return false;
        };
        let notification = notification.clone();
        cx.background_spawn(async move {
            let tag: &str = &notification.tag;
            let action = HashMap::from([
                ("label", Variant::from("Open")),
                ("action", Variant::from(OPEN_ACTION)),
                ("target", Variant::from(tag)),
            ]);
            let mut payload = HashMap::from([
                ("title", Variant::from(notification.title.as_ref())),
                ("body", Variant::from(notification.body.as_ref())),
                ("default-action", Variant::from(OPEN_ACTION)),
                ("default-action-target", Variant::from(tag)),
                ("buttons", Variant::from(vec![action])),
            ]);
            if explicit_silence {
                payload.insert("sound", Variant::from("silent"));
            }
            // A portal denial must not bypass the user's permission through the fallback.
            let _: zbus::Result<()> = portal.call("AddNotification", &(tag, payload)).await;
        })
        .detach();
        true
    }
}

async fn notifications_are_silent_without_hints(connection: &zbus::Connection) -> bool {
    let Ok(notifier) = Proxy::new(
        connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    else {
        return false;
    };
    let server: zbus::Result<(String, String, String, String)> =
        notifier.call("GetServerInformation", &()).await;
    // GNOME Shell only plays sounds when the notification carries a sound hint.
    server.is_ok_and(|(name, vendor, _, _)| name == "gnome-shell" && vendor == "GNOME")
}

struct Application {
    activated: async_channel::Sender<NativeActivation>,
}

#[zbus::interface(name = "org.freedesktop.Application")]
impl Application {
    fn activate(&self, platform_data: HashMap<String, OwnedVariant>) {
        self.dispatch(None, platform_data);
    }

    fn activate_action(
        &self,
        action_name: &str,
        parameter: Vec<OwnedVariant>,
        platform_data: HashMap<String, OwnedVariant>,
    ) {
        if let ("open", [tag]) = (action_name, parameter.as_slice())
            && let Ok(tag) = <&str>::try_from(tag)
        {
            self.dispatch(
                Some(SystemNotificationResponse {
                    tag: tag.to_owned().into(),
                    action_id: Some("open".into()),
                }),
                platform_data,
            );
        }
    }
}

impl Application {
    fn dispatch(
        &self,
        response: Option<SystemNotificationResponse>,
        platform_data: HashMap<String, OwnedVariant>,
    ) {
        let token = platform_data
            .get("activation-token")
            .or_else(|| platform_data.get("desktop-startup-id"))
            .and_then(|value| <&str>::try_from(value).ok())
            .map(ToOwned::to_owned);
        let _ = self
            .activated
            .try_send(NativeActivation { response, token });
    }
}

pub fn activate_window(window: &Window, token: Option<&str>) {
    if token
        .and_then(|token| activate_wayland(window, token))
        .is_none()
    {
        window.activate_window();
    }
}

struct ActivationQueue;

impl Dispatch<WlRegistry, GlobalListContents> for ActivationQueue {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(ActivationQueue: ignore XdgActivationV1);

fn activate_wayland(window: &Window, token: &str) -> Option<()> {
    let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
        return None;
    };
    let RawWindowHandle::Wayland(surface) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    // GPUI owns both handles and keeps them live until this call returns.
    let connection = Connection::from_backend(unsafe {
        Backend::from_foreign_display(display.display.as_ptr().cast())
    });
    let surface =
        unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast()) }
            .ok()?;
    let surface = WlSurface::from_id(&connection, surface).ok()?;
    let (globals, queue) = registry_queue_init::<ActivationQueue>(&connection).ok()?;
    let activation = globals.bind::<XdgActivationV1, _, _>(&queue.handle(), 1..=1, ());
    globals.destroy();
    let activation = activation.ok()?;
    activation.activate(token.into(), &surface);
    activation.destroy();
    connection.flush().ok()
}

fn register_desktop(root: &Path, executable: &Path) -> io::Result<()> {
    let executable = executable
        .to_str()
        .filter(|path| !path.chars().any(char::is_control))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid SPK launch path"))?;
    // Quote the D-Bus command argument and escape its backslashes for the service file.
    let executable = executable
        .replace('\\', "\\\\\\\\")
        .replace('"', "\\\\\"")
        .replace('`', "\\\\`")
        .replace('$', "\\\\$");
    for (directory, filename, content) in [
        (
            root.join("dbus-1/services"),
            format!("{APP_ID}.service"),
            format!("[D-BUS Service]\nName={APP_ID}\nExec=\"{executable}\"\n"),
        ),
        (
            root.join("applications"),
            format!("{APP_ID}.desktop"),
            "[Desktop Entry]\nType=Application\nName=SPK\nDBusActivatable=true\nX-GNOME-UsesNotifications=true\n".into(),
        ),
    ] {
        let path = directory.join(filename);
        if fs::read_to_string(&path).as_deref().ok() == Some(&content) {
            continue;
        }
        fs::create_dir_all(&directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(content.as_bytes())?;
        file.persist(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct GnomeNotificationServer;

    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl GnomeNotificationServer {
        fn get_server_information(&self) -> (&str, &str, &str, &str) {
            ("gnome-shell", "GNOME", "46", "1.2")
        }
    }

    #[test]
    fn desktop_registration_points_to_the_running_executable() {
        let root = tempfile::tempdir().unwrap();
        let executable = Path::new("/opt/SPK's 100%/spk-gpui");
        register_desktop(root.path(), executable).unwrap();
        let desktop = fs::read_to_string(
            root.path()
                .join("applications")
                .join(format!("{APP_ID}.desktop")),
        )
        .unwrap();
        let service = fs::read_to_string(
            root.path()
                .join("dbus-1/services")
                .join(format!("{APP_ID}.service")),
        )
        .unwrap();
        assert!(desktop.contains("DBusActivatable=true\n"));
        assert!(desktop.contains("X-GNOME-UsesNotifications=true\n"));
        assert!(service.contains("Exec=\"/opt/SPK's 100%/spk-gpui\"\n"));

        assert!(register_desktop(root.path(), Path::new("/opt/SPK\nName=other")).is_err());
        assert_eq!(
            fs::read_to_string(
                root.path()
                    .join("dbus-1/services")
                    .join(format!("{APP_ID}.service")),
            )
            .unwrap(),
            service
        );
    }

    #[tokio::test]
    #[ignore = "requires the isolated D-Bus session from the build workflow"]
    async fn retained_notification_starts_the_registered_application() {
        use std::{os::unix::fs::PermissionsExt, time::Duration};

        let root = std::env::var_os("SPK_TEST_NOTIFICATION_DATA").expect("isolated test data");
        let root = Path::new(&root);
        let launcher = root.join("SPK's \"demo\" `$ test");
        let test_executable = std::env::current_exe().unwrap();
        let command = test_executable.to_str().unwrap().replace('\'', "'\\''");
        fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nexec '{command}' --ignored --exact notifications::linux::tests::retained_activation_child\n",
            ),
        )
        .unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        register_desktop(root, &launcher).unwrap();

        let base = url::Url::parse(env!("SPK_URL")).unwrap();
        let target = crate::api::MessageTarget::new("channel", "message").unwrap();
        let tag = crate::notifications::activation_tag(&base, &target);
        let connection = zbus::Connection::session().await.unwrap();
        let _server = Builder::session()
            .name("org.freedesktop.Notifications")
            .serve_at("/org/freedesktop/Notifications", GnomeNotificationServer)
            .build()
            .await
            .unwrap();
        assert!(notifications_are_silent_without_hints(&connection).await);
        let application = Proxy::new(
            &connection,
            APP_ID,
            OBJECT_PATH,
            "org.freedesktop.Application",
        )
        .await
        .unwrap();
        let data = HashMap::from([("activation-token", Variant::from("portal-token"))]);
        let (): () = application
            .call(
                "ActivateAction",
                &("open", vec![Variant::from(tag.as_str())], data),
            )
            .await
            .unwrap();

        let delivered = root.join("delivered");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !delivered.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("D-Bus should launch SPK and deliver the target");
    }

    #[tokio::test]
    #[ignore = "invoked by the isolated D-Bus activation fixture"]
    async fn retained_activation_child() {
        let root = std::env::var_os("SPK_TEST_NOTIFICATION_DATA").expect("isolated test data");
        let (activated, activations) = async_channel::unbounded();
        let _registration = Notifications::register(activated).await.unwrap();
        let activation = activations.recv().await.unwrap();
        assert_eq!(activation.token.as_deref(), Some("portal-token"));
        let base = url::Url::parse(env!("SPK_URL")).unwrap();
        let target = crate::api::MessageTarget::new("channel", "message").unwrap();
        assert_eq!(
            crate::notifications::activation_target(&base, &activation.response.unwrap().tag),
            Some(target)
        );
        fs::write(Path::new(&root).join("delivered"), "").unwrap();
    }
}
