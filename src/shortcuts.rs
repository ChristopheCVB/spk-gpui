use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers as NativeModifiers},
};
use gpui_kit::{Keystroke, Modifiers, Window};
use serde::{Deserialize, Serialize};

use crate::sounds::SoundSettings;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Mute,
    Deafen,
}

impl Action {
    pub const ALL: [Self; 2] = [Self::Mute, Self::Deafen];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mute => "Toggle mute",
            Self::Deafen => "Toggle deafen",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Mute => 0,
            Self::Deafen => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    InSpk,
    Global,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::InSpk => "In SPK",
            Self::Global => "Global",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Chord {
    key: String,
    modifiers: Modifiers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mac_key: Option<String>,
}

impl Chord {
    pub fn capture(keystroke: &Keystroke, window: &Window) -> Option<Self> {
        let chord = Self::from_keystroke(keystroke, window);
        chord.valid().then_some(chord)
    }

    fn from_keystroke(keystroke: &Keystroke, window: &Window) -> Self {
        let mut modifiers = keystroke.modifiers;
        // GPUI can fold Shift into a digit's resulting symbol.
        modifiers.shift |= window.modifiers().shift;
        modifiers.function = false;
        let key = keystroke.key.to_ascii_lowercase();
        let mac_key = captured_mac_key();
        let key = if modifiers.shift {
            char_from_single_ascii(&key)
                .and_then(|key| ")!@#$%^&*(".find(key))
                .filter(|_| mac_key.is_none())
                .map_or(key, |digit| digit.to_string())
        } else {
            key
        };
        let key = mac_key
            .as_deref()
            .and_then(|key_code| key_code.strip_prefix("Digit"))
            .map_or(key, str::to_owned);
        Self {
            key,
            modifiers,
            mac_key,
        }
    }

    fn valid(&self) -> bool {
        key_code(&self.key).is_some()
            && !self.modifiers.function
            && self.mac_key.as_deref().is_none_or(|mac_key| {
                if let Some(letter) = mac_key.strip_prefix("Key") {
                    char_from_single_ascii(letter).is_some_and(|key| key.is_ascii_uppercase())
                } else if let Some(digit) = mac_key.strip_prefix("Digit") {
                    char_from_single_ascii(digit).is_some_and(|key| key.is_ascii_digit())
                } else {
                    false
                }
            })
            && (self.key.starts_with('f') && self.key.len() > 1
                || self.modifiers.control
                || self.modifiers.alt
                || self.modifiers.platform)
    }

    fn same(&self, other: &Self) -> bool {
        self.modifiers == other.modifiers && self.same_key(other)
    }

    fn same_key(&self, other: &Self) -> bool {
        self.key == other.key
            || matches!((&self.mac_key, &other.mac_key), (Some(a), Some(b)) if a == b)
    }

    fn hotkey(&self) -> Option<HotKey> {
        if cfg!(target_os = "macos") && matches!(self.key.as_str(), "f21" | "f22" | "f23" | "f24") {
            return None;
        }
        let m = self.modifiers;
        let mut native = NativeModifiers::empty();
        native.set(NativeModifiers::CONTROL, m.control);
        native.set(NativeModifiers::ALT, m.alt);
        native.set(NativeModifiers::SHIFT, m.shift);
        native.set(NativeModifiers::META, m.platform);

        let code = if cfg!(target_os = "macos") && self.key.len() == 1 {
            self.mac_key.as_deref()?.parse().ok()?
        } else {
            key_code(&self.key)?
        };
        Some(HotKey::new(Some(native), code))
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (alt, platform) = if cfg!(target_os = "macos") {
            ("Option", "Command")
        } else if cfg!(windows) {
            ("Alt", "Win")
        } else {
            ("Alt", "Super")
        };
        for (active, label) in [
            (self.modifiers.control, "Ctrl"),
            (self.modifiers.alt, alt),
            (self.modifiers.shift, "Shift"),
            (self.modifiers.platform, platform),
        ] {
            if active {
                write!(f, "{label} + ")?;
            }
        }
        f.write_str(&self.key.to_ascii_uppercase())
    }
}

fn char_from_single_ascii(key: &str) -> Option<char> {
    (key.len() == 1).then(|| char::from(key.as_bytes()[0]))
}

fn key_code(key: &str) -> Option<Code> {
    let name = if let Some(key) = char_from_single_ascii(key) {
        if key.is_ascii_alphabetic() {
            format!("Key{}", key.to_ascii_uppercase())
        } else if key.is_ascii_digit() {
            format!("Digit{key}")
        } else {
            return None;
        }
    } else {
        let digits = key.strip_prefix('f')?;
        let number = digits.parse::<u8>().ok()?;
        if !(1..=24).contains(&number) || number.to_string() != digits {
            return None;
        }
        format!("F{number}")
    };
    name.parse().ok()
}

#[cfg(not(target_os = "macos"))]
fn captured_mac_key() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn captured_mac_key() -> Option<String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEventType};

    let event = NSApplication::sharedApplication(MainThreadMarker::new()?).currentEvent()?;
    if event.r#type() != NSEventType::KeyDown {
        return None;
    }

    // Carbon uses positions on the ANSI keyboard even when AppKit reports a different letter.
    let key = event.keyCode();
    let letter_codes = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f,
        0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
    ];
    let digit_codes = [0x1d, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19];
    if let Some(letter) = letter_codes.iter().position(|code| *code == key) {
        return Some(format!("Key{}", char::from(b'A' + letter as u8)));
    }
    let digit = digit_codes.iter().position(|code| *code == key)?;
    Some(format!("Digit{digit}"))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Binding {
    pub chord: Chord,
    pub scope: Scope,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Preferences {
    mute: Option<Binding>,
    deafen: Option<Binding>,
    sounds: SoundSettings,
}

impl Preferences {
    fn binding(&self, action: Action) -> &Option<Binding> {
        match action {
            Action::Mute => &self.mute,
            Action::Deafen => &self.deafen,
        }
    }

    fn binding_mut(&mut self, action: Action) -> &mut Option<Binding> {
        match action {
            Action::Mute => &mut self.mute,
            Action::Deafen => &mut self.deafen,
        }
    }

    fn load(path: Option<&Path>) -> io::Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let mut preferences: Self = serde_json::from_reader(io::BufReader::new(file))?;
        for action in Action::ALL {
            let binding = preferences.binding_mut(action);
            if binding
                .as_ref()
                .is_some_and(|binding| !binding.chord.valid())
            {
                *binding = None;
            }
        }
        if matches!((&preferences.mute, &preferences.deafen), (Some(a), Some(b)) if a.chord.same(&b.chord))
        {
            preferences.deafen = None;
        }
        for sound in crate::sounds::Sound::ALL {
            let setting = preferences.sounds.get_mut(sound);
            setting.volume = setting.volume.min(100);
        }
        Ok(preferences)
    }

    fn save(&self, path: Option<&Path>) -> io::Result<()> {
        let path = path.ok_or_else(|| io::Error::other("Missing config directory"))?;
        let directory = path.parent().expect("preferences path has a parent");
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(file.as_file_mut(), self)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeError {
    Duplicate,
    UnsupportedKey,
    Platform,
    Conflict,
    Registration,
    Save,
}

impl ChangeError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Duplicate => "This key combination is assigned to another SPK shortcut.",
            Self::UnsupportedKey => "This key cannot be registered as a global shortcut.",
            Self::Platform => "Global shortcuts are unavailable in this desktop session.",
            Self::Conflict => "The operating system or another app already uses this shortcut.",
            Self::Registration => "Could not register this global shortcut.",
            Self::Save => "Could not save voice preferences.",
        }
    }

    pub fn can_use_focused(self) -> bool {
        matches!(
            self,
            Self::UnsupportedKey | Self::Platform | Self::Conflict | Self::Registration
        )
    }
}

pub struct Editor {
    pub action: Action,
    pub scope: Scope,
    pub chord: Option<Chord>,
    pub recording: bool,
    pub error: Option<&'static str>,
    pub allow_focused: bool,
}

pub struct Shortcuts {
    preferences: Preferences,
    path: Option<PathBuf>,
    pub settings_error: Option<&'static str>,
    pub editor: Option<Editor>,
    platform_error: Option<&'static str>,
    manager: Option<GlobalHotKeyManager>,
    active: [Option<HotKey>; 2],
    errors: [Option<ChangeError>; 2],
    signed_in: bool,
    generation: Arc<AtomicU64>,
    registered_since: [u64; 2],
    x11_key_repeat: bool,
    focused_held: [bool; 2],
    #[cfg(target_os = "macos")]
    held: [bool; 2],
}

impl Shortcuts {
    pub fn new(window: &Window) -> Self {
        let path =
            dirs::config_dir().map(|dir| dir.join("spk-gpui").join("voice-preferences.json"));
        let mut shortcuts = Self::load(path, global_platform_error(window));
        shortcuts.x11_key_repeat = configure_x11_repeat(window).is_some();
        shortcuts
    }

    fn load(path: Option<PathBuf>, platform_error: Option<&'static str>) -> Self {
        let result = Preferences::load(path.as_deref());
        let settings_error = result
            .is_err()
            .then_some("Could not read saved shortcuts and sounds. Using defaults.");
        Self {
            preferences: result.unwrap_or_default(),
            path,
            settings_error,
            editor: None,
            platform_error,
            manager: None,
            active: [None; 2],
            errors: [None; 2],
            signed_in: false,
            generation: Arc::default(),
            registered_since: [0; 2],
            x11_key_repeat: false,
            focused_held: [false; 2],
            #[cfg(target_os = "macos")]
            held: [false; 2],
        }
    }

    pub fn watch_events(&self) -> async_channel::Receiver<(u64, GlobalHotKeyEvent)> {
        let (sender, events) = async_channel::unbounded();
        let generation = self.generation.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event| {
            let _ = sender.try_send((generation.load(Ordering::Relaxed), event));
        }));
        events
    }

    pub fn signed_in(&mut self, signed_in: bool) {
        if self.signed_in == signed_in {
            return;
        }
        self.signed_in = signed_in;
        self.editor = None;
        self.clear_focused();
        self.reset_global();
    }

    fn reset_global(&mut self) {
        for hotkey in self.active.into_iter().flatten() {
            if let Some(manager) = &self.manager {
                let _ = manager.unregister(hotkey);
            }
        }
        self.manager = None;
        self.active = [None; 2];
        self.errors = [None; 2];
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.registered_since.fill(generation);
        #[cfg(target_os = "macos")]
        self.held.fill(false);
        if self.signed_in {
            for action in Action::ALL {
                self.retry(action);
            }
        }
    }

    pub fn platform_error(&self) -> Option<&'static str> {
        self.platform_error
    }

    pub fn binding(&self, action: Action) -> Option<&Binding> {
        self.preferences.binding(action).as_ref()
    }

    pub fn error(&self, action: Action) -> Option<ChangeError> {
        self.errors[action.index()]
    }

    pub fn sound_settings(&self) -> SoundSettings {
        self.preferences.sounds
    }

    pub fn set_sound_settings(&mut self, sounds: SoundSettings) {
        let next = Preferences {
            sounds,
            ..self.preferences.clone()
        };
        self.settings_error = next
            .save(self.path.as_deref())
            .err()
            .map(|_| ChangeError::Save.message());
        self.preferences = next;
    }

    fn register(&mut self, binding: &Binding) -> Result<Option<HotKey>, ChangeError> {
        if binding.scope == Scope::InSpk || !self.signed_in {
            return Ok(None);
        }
        if self.platform_error.is_some() {
            return Err(ChangeError::Platform);
        }
        let hotkey = binding.chord.hotkey().ok_or(ChangeError::UnsupportedKey)?;
        if self.manager.is_none() {
            self.manager = Some(GlobalHotKeyManager::new().map_err(|_| ChangeError::Registration)?);
        }
        self.manager
            .as_ref()
            .unwrap()
            .register(hotkey)
            .map_err(|error| {
                if matches!(error, global_hotkey::Error::AlreadyRegistered(_)) {
                    ChangeError::Conflict
                } else {
                    ChangeError::Registration
                }
            })?;
        Ok(Some(hotkey))
    }

    pub fn retry(&mut self, action: Action) {
        let index = action.index();
        if self.active[index].is_none()
            && let Some(binding) = self.binding(action).cloned()
        {
            match self.register(&binding) {
                Ok(hotkey) => {
                    self.active[index] = hotkey;
                    self.errors[index] = None;
                    if hotkey.is_some() {
                        self.mark_registration(index);
                    }
                }
                Err(error) => self.errors[index] = Some(error),
            }
        }
    }

    pub fn set_binding(
        &mut self,
        action: Action,
        binding: Option<Binding>,
    ) -> Result<(), ChangeError> {
        if let Some(binding) = &binding {
            if !binding.chord.valid() {
                return Err(ChangeError::UnsupportedKey);
            }
            if Action::ALL.into_iter().any(|other| {
                other != action
                    && self
                        .binding(other)
                        .is_some_and(|old| old.chord.same(&binding.chord))
            }) {
                return Err(ChangeError::Duplicate);
            }
        }
        if self.binding(action) == binding.as_ref() {
            self.retry(action);
            return self.error(action).map_or(Ok(()), Err);
        }

        let index = action.index();
        let old = self.active[index];
        let requested = binding
            .as_ref()
            .filter(|b| b.scope == Scope::Global)
            .and_then(|b| b.chord.hotkey());
        let registered = if old.is_some() && old == requested {
            old
        } else {
            binding
                .as_ref()
                .map(|b| self.register(b))
                .transpose()?
                .flatten()
        };
        let mut next = self.preferences.clone();
        *next.binding_mut(action) = binding;
        if next.save(self.path.as_deref()).is_err() {
            if registered != old
                && let Some(hotkey) = registered
            {
                self.unregister(hotkey);
            }
            return Err(ChangeError::Save);
        }
        self.preferences = next;
        self.settings_error = None;
        self.active[index] = registered;
        self.errors[index] = None;
        self.focused_held[index] = false;
        if old != registered {
            self.mark_registration(index);
            if let Some(hotkey) = old {
                self.unregister(hotkey);
            }
        }
        Ok(())
    }

    fn mark_registration(&mut self, index: usize) {
        self.registered_since[index] = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        #[cfg(target_os = "macos")]
        {
            self.held[index] = false;
        }
    }

    fn unregister(&mut self, hotkey: HotKey) {
        if let Some(manager) = &self.manager
            && manager.unregister(hotkey).is_err()
        {
            self.reset_global();
        }
    }

    pub fn focused_action(&self, chord: &Chord) -> Option<Action> {
        if !self.signed_in || self.editor.is_some() {
            return None;
        }
        Action::ALL.into_iter().find(|action| {
            self.binding(*action)
                .is_some_and(|binding| binding.scope == Scope::InSpk && binding.chord.same(chord))
        })
    }

    pub fn focused_press(&mut self, action: Action, repeated: bool) -> bool {
        !repeated
            && (!self.x11_key_repeat
                || !std::mem::replace(&mut self.focused_held[action.index()], true))
    }

    pub fn focused_release(&mut self, keystroke: &Keystroke, window: &Window) {
        if self.focused_held.contains(&true) {
            self.release_key(&Chord::from_keystroke(keystroke, window));
        }
    }

    fn release_key(&mut self, key: &Chord) {
        for action in Action::ALL {
            if self.focused_held[action.index()]
                && self
                    .binding(action)
                    .is_some_and(|binding| binding.chord.same_key(key))
            {
                self.focused_held[action.index()] = false;
            }
        }
    }

    pub fn clear_focused(&mut self) {
        self.focused_held.fill(false);
    }

    pub fn global_action(&mut self, generation: u64, event: GlobalHotKeyEvent) -> Option<Action> {
        if !self.signed_in {
            return None;
        }
        let action = Action::ALL.into_iter().find(|action| {
            generation >= self.registered_since[action.index()]
                && self.active[action.index()].is_some_and(|hotkey| hotkey.id() == event.id)
        })?;
        #[cfg(target_os = "macos")]
        {
            let held = &mut self.held[action.index()];
            let pressed = event.state == HotKeyState::Pressed;
            if std::mem::replace(held, pressed) {
                return None;
            }
        }
        (event.state == HotKeyState::Pressed).then_some(action)
    }
}

#[cfg(not(target_os = "linux"))]
fn global_platform_error(_: &Window) -> Option<&'static str> {
    None
}

#[cfg(not(target_os = "linux"))]
fn configure_x11_repeat(_: &Window) -> Option<()> {
    None
}

#[cfg(target_os = "linux")]
fn configure_x11_repeat(window: &Window) -> Option<()> {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    use x11rb::{
        protocol::xkb::{ConnectionExt as _, ID, PerClientFlag},
        xcb_ffi::XCBConnection,
    };

    let display = window.display_handle().ok()?;
    let RawDisplayHandle::Xcb(display) = display.as_raw() else {
        return None;
    };
    let connection = display.connection?;
    // GPUI owns this connection. Use it only for this synchronous request on the UI thread.
    let connection =
        unsafe { XCBConnection::from_raw_xcb_connection(connection.as_ptr(), false) }.ok()?;
    // X11 otherwise sends synthetic KeyUp events between repeats, which GPUI exposes as fresh presses.
    let flag = PerClientFlag::DETECTABLE_AUTO_REPEAT;
    let reply = connection
        .xkb_per_client_flags(
            ID::USE_CORE_KBD.into(),
            flag,
            flag,
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .ok()?
        .reply()
        .ok()?;
    (reply.value & flag == flag).then_some(())
}

#[cfg(target_os = "linux")]
fn global_platform_error(window: &Window) -> Option<&'static str> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland");
    match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Wayland(_)) => Some("Wayland supports shortcuts while SPK is focused."),
        Ok(RawWindowHandle::Xcb(_) | RawWindowHandle::Xlib(_)) if !wayland => None,
        _ if wayland => Some("Wayland supports shortcuts while SPK is focused."),
        _ => Some("Global shortcuts require an X11 desktop session on Linux."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(key: &str, modifiers: Modifiers) -> Chord {
        Chord {
            key: key.to_owned(),
            modifiers,
            mac_key: None,
        }
    }

    fn binding(key: &str, scope: Scope) -> Binding {
        Binding {
            chord: chord(key, Modifiers::default()),
            scope,
        }
    }

    #[test]
    fn accepts_function_keys_and_modified_letters_and_digits() {
        let plain = Modifiers::default();
        let ctrl = Modifiers {
            control: true,
            ..plain
        };
        assert!(chord("f1", plain).valid());
        assert!(chord("f24", plain).valid());
        assert!(!chord("f25", plain).valid());
        assert!(!chord("f01", plain).valid());
        assert!(!chord("m", plain).valid());
        assert!(chord("m", ctrl).valid());
        assert!(chord("9", ctrl).valid());
        assert!(!chord("escape", ctrl).valid());
    }

    #[test]
    fn failed_assignment_preserves_previous_binding_and_preferences_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice-preferences.json");
        let mut shortcuts = Shortcuts::load(Some(path.clone()), Some("Unavailable"));
        shortcuts.signed_in(true);
        assert!(shortcuts.binding(Action::Mute).is_none());
        let original = binding("f1", Scope::InSpk);
        shortcuts
            .set_binding(Action::Mute, Some(original.clone()))
            .unwrap();

        assert_eq!(
            shortcuts.set_binding(Action::Deafen, Some(original.clone())),
            Err(ChangeError::Duplicate)
        );
        assert_eq!(
            shortcuts.set_binding(Action::Mute, Some(binding("f2", Scope::Global))),
            Err(ChangeError::Platform)
        );
        assert_eq!(
            shortcuts.focused_action(&original.chord),
            Some(Action::Mute)
        );
        assert_eq!(shortcuts.binding(Action::Mute), Some(&original));
        assert!(!shortcuts.focused_press(Action::Mute, true));
        shortcuts.x11_key_repeat = true;
        assert!(shortcuts.focused_press(Action::Mute, false));
        assert!(!shortcuts.focused_press(Action::Mute, false));
        shortcuts.release_key(&original.chord);
        assert!(shortcuts.focused_press(Action::Mute, false));

        let mut sounds = SoundSettings::default();
        sounds.leave.enabled = false;
        sounds.deafen.volume = 23;
        shortcuts.set_sound_settings(sounds);
        assert!(shortcuts.settings_error.is_none());
        let mut reopened = Shortcuts::load(Some(path), None);
        assert_eq!(reopened.binding(Action::Mute), Some(&original));
        assert_eq!(reopened.sound_settings(), sounds);
        assert_eq!(reopened.focused_action(&original.chord), None);
        reopened.set_binding(Action::Mute, None).unwrap();
        assert!(reopened.binding(Action::Mute).is_none());
    }

    #[test]
    fn global_dispatch_uses_current_registration_and_pressed_events() {
        let mut shortcuts = Shortcuts::load(None, None);
        shortcuts.signed_in(true);
        let hotkey = HotKey::new(None, Code::F11);
        let index = Action::Mute.index();
        shortcuts.active[index] = Some(hotkey);
        shortcuts.mark_registration(index);
        let generation = shortcuts.generation.load(Ordering::Relaxed);
        let event = |state| GlobalHotKeyEvent {
            id: hotkey.id(),
            state,
        };
        assert_eq!(
            shortcuts.global_action(generation - 1, event(HotKeyState::Pressed)),
            None
        );
        assert_eq!(
            shortcuts.global_action(generation, event(HotKeyState::Pressed)),
            Some(Action::Mute)
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            shortcuts.global_action(generation, event(HotKeyState::Pressed)),
            None
        );
        assert_eq!(
            shortcuts.global_action(generation, event(HotKeyState::Released)),
            None
        );
        assert_eq!(
            shortcuts.global_action(generation, event(HotKeyState::Pressed)),
            Some(Action::Mute)
        );
        shortcuts.signed_in(false);
        assert_eq!(
            shortcuts.global_action(generation, event(HotKeyState::Pressed)),
            None
        );
    }
}
