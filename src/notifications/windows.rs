use std::{
    collections::hash_map::DefaultHasher,
    ffi::c_void,
    hash::{Hash, Hasher},
};

use gpui_kit::{Context, Global, SystemNotification, SystemNotificationResponse};
use windows::{
    Data::Xml::Dom::XmlDocument,
    UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier},
    Win32::{
        Foundation::CLASS_E_NOAGGREGATION,
        System::Com::{
            CLSCTX_LOCAL_SERVER, CoRegisterClassObject, CoRevokeClassObject, IClassFactory,
            IClassFactory_Impl, REGCLS_MULTIPLEUSE,
        },
        UI::Notifications::{
            INotificationActivationCallback, INotificationActivationCallback_Impl,
            NOTIFICATION_USER_INPUT_DATA,
        },
    },
    core::{BOOL, GUID, HSTRING, IUnknown, Interface as _, PCWSTR, Ref, Result, h, implement},
};
use windows_registry::CURRENT_USER;

use super::NativeActivation;
use crate::{APP_ID, ui::Spk};

const ACTIVATOR_ID: GUID = GUID::from_u128(0x80e34428_a720_46e0_846c_12bdd0c5b4af);

pub struct Notifications {
    notifier: ToastNotifier,
    registration: u32,
}

impl Global for Notifications {}

impl Notifications {
    pub fn register(activated: async_channel::Sender<NativeActivation>) -> Option<Self> {
        let executable = std::env::current_exe().ok()?;
        let clsid = format!("{{{ACTIVATOR_ID:?}}}");
        let server = CURRENT_USER
            .create(format!(r"Software\Classes\CLSID\{clsid}\LocalServer32"))
            .ok()?;
        server
            .set_string("", format!("\"{}\"", executable.to_str()?))
            .ok()?;
        let app = CURRENT_USER
            .create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"))
            .ok()?;
        app.set_string("DisplayName", "SPK").ok()?;
        app.set_string("CustomActivator", clsid).ok()?;
        let notifier =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID)).ok()?;
        let factory: IClassFactory = Factory { activated }.into();
        // GPUI initializes COM for its UI thread and pumps the STA message queue.
        let registration = unsafe {
            CoRegisterClassObject(
                &ACTIVATOR_ID,
                &factory,
                CLSCTX_LOCAL_SERVER,
                REGCLS_MULTIPLEUSE,
            )
        }
        .ok()?;
        Some(Self {
            notifier,
            registration,
        })
    }

    pub fn show(notification: &SystemNotification, cx: &Context<Spk>) -> bool {
        let Some(state) = cx.try_global::<Self>() else {
            return false;
        };
        let _ = state.post(notification);
        true
    }

    fn post(&self, notification: &SystemNotification) -> Result<()> {
        let toast = ToastNotification::CreateToastNotification(&toast_document(notification)?)?;
        let mut hasher = DefaultHasher::new();
        notification.tag.hash(&mut hasher);
        toast.SetTag(&HSTRING::from(format!("{:016x}", hasher.finish())))?;
        self.notifier.Show(&toast)
    }
}

impl Drop for Notifications {
    fn drop(&mut self) {
        let _ = unsafe { CoRevokeClassObject(self.registration) };
    }
}

fn toast_document(notification: &SystemNotification) -> Result<XmlDocument> {
    let document = XmlDocument::new()?;
    document.LoadXml(h!(r#"<toast activationType="foreground"><visual><binding template="ToastGeneric"><text/><text/></binding></visual><actions><action content="Open" activationType="foreground"/></actions><audio silent="true"/></toast>"#))?;
    let tag = HSTRING::from(notification.tag.as_ref());
    document
        .DocumentElement()?
        .SetAttribute(h!("launch"), &tag)?;
    document
        .SelectSingleNode(h!("//action"))?
        .cast::<windows::Data::Xml::Dom::XmlElement>()?
        .SetAttribute(h!("arguments"), &tag)?;
    let texts = document.GetElementsByTagName(h!("text"))?;
    texts
        .Item(0)?
        .SetInnerText(&HSTRING::from(notification.title.as_ref()))?;
    texts
        .Item(1)?
        .SetInnerText(&HSTRING::from(notification.body.as_ref()))?;
    Ok(document)
}

#[implement(IClassFactory)]
struct Factory {
    activated: async_channel::Sender<NativeActivation>,
}

#[allow(non_snake_case)]
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut c_void,
    ) -> Result<()> {
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let callback: INotificationActivationCallback = Callback {
            activated: self.activated.clone(),
        }
        .into();
        unsafe { callback.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

#[implement(INotificationActivationCallback)]
struct Callback {
    activated: async_channel::Sender<NativeActivation>,
}

#[allow(non_snake_case)]
impl INotificationActivationCallback_Impl for Callback_Impl {
    fn Activate(
        &self,
        app_id: &PCWSTR,
        args: &PCWSTR,
        _data: *const NOTIFICATION_USER_INPUT_DATA,
        _count: u32,
    ) -> Result<()> {
        if !app_id.is_null() && !args.is_null() && unsafe { app_id.to_string()? } == APP_ID {
            let _ = self
                .activated
                .try_send(NativeActivation::from(SystemNotificationResponse {
                    tag: unsafe { args.to_string()? }.into(),
                    action_id: None,
                }));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_body_and_open_action_use_the_same_exact_target() {
        use windows::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
        };

        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.unwrap();
        let notification = SystemNotification {
            tag: "spk-alert:https://spk.example/c/channel/m/message".into(),
            title: "<text>&".into(),
            body: "Example message".into(),
            actions: Vec::new(),
        };
        let document = toast_document(&notification).unwrap();
        let tag = notification.tag.as_ref();
        assert_eq!(
            document
                .DocumentElement()
                .unwrap()
                .GetAttribute(h!("launch"))
                .unwrap(),
            tag
        );
        let action = document
            .SelectSingleNode(h!("//action"))
            .unwrap()
            .cast::<windows::Data::Xml::Dom::XmlElement>()
            .unwrap();
        assert_eq!(action.GetAttribute(h!("arguments")).unwrap(), tag);
        let title = document
            .GetElementsByTagName(h!("text"))
            .unwrap()
            .Item(0)
            .unwrap();
        assert_eq!(title.InnerText().unwrap(), notification.title.as_ref());
        ToastNotification::CreateToastNotification(&document).unwrap();
        drop((document, action, title));
        unsafe { CoUninitialize() };
    }

    #[test]
    fn retained_toast_dispatches_the_exact_target_without_a_live_toast_object() {
        let (activated, activations) = async_channel::unbounded();
        let factory: IClassFactory = Factory { activated }.into();
        let callback: INotificationActivationCallback =
            unsafe { factory.CreateInstance(None) }.unwrap();
        let base = url::Url::parse("https://spk.example").unwrap();
        let target = crate::api::MessageTarget::new("channel", "message").unwrap();
        let tag = crate::notifications::activation_tag(&base, &target);
        let tag_text = HSTRING::from(tag);
        let identity = windows::core::HSTRING::from(APP_ID);
        let tag = PCWSTR(tag_text.as_ptr());
        let app_id = PCWSTR(identity.as_ptr());

        unsafe {
            callback
                .Activate(windows::core::w!("another.app"), tag, &[])
                .unwrap();
        }
        assert!(activations.try_recv().is_err());
        unsafe { callback.Activate(app_id, tag, &[]).unwrap() };
        let response = activations.try_recv().unwrap().response.unwrap();
        assert_eq!(
            crate::notifications::activation_target(&base, &response.tag),
            Some(target)
        );
    }
}
