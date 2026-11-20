use std::{fs, io, path::Path};

use livekit::{PlatformAudio, PlayoutDeviceId, RecordingDeviceId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DeviceKind {
    Microphone,
    Speakers,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct DevicePreferences {
    pub microphone: Option<Device>,
    pub speakers: Option<Device>,
}

impl DevicePreferences {
    pub fn get_mut(&mut self, kind: DeviceKind) -> &mut Option<Device> {
        match kind {
            DeviceKind::Microphone => &mut self.microphone,
            DeviceKind::Speakers => &mut self.speakers,
        }
    }

    pub fn path() -> Option<std::path::PathBuf> {
        Some(
            dirs::config_dir()?
                .join("spk-gpui")
                .join("voice-devices.json"),
        )
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::File::open(path) {
            Ok(file) => Ok(serde_json::from_reader(io::BufReader::new(file))?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let directory = path
            .parent()
            .expect("voice preferences have a parent directory");
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(file.as_file_mut(), self)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceList {
    pub choices: Vec<Device>,
    pub default: Option<Device>,
}

impl DeviceList {
    pub fn preferred(&self, saved: Option<&Device>) -> Option<&Device> {
        let saved = saved?;
        if !saved.id.starts_with("index:") {
            return self.choices.iter().find(|device| device.id == saved.id);
        }
        let mut matches = self
            .choices
            .iter()
            .filter(|device| device.name == saved.name);
        let device = matches.next()?;
        matches.next().is_none().then_some(device)
    }

    pub fn resolve(&self, saved: Option<&Device>) -> Option<&Device> {
        self.preferred(saved).or(self.default.as_ref())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Devices {
    pub microphones: DeviceList,
    pub speakers: DeviceList,
}

impl Devices {
    pub fn read(audio: &PlatformAudio) -> Self {
        Self {
            microphones: list(
                DeviceKind::Microphone,
                audio.recording_devices().map(|device| {
                    (
                        device.index,
                        Device {
                            id: device.id.as_str().into(),
                            name: device.name,
                        },
                    )
                }),
            ),
            speakers: list(
                DeviceKind::Speakers,
                audio.playout_devices().map(|device| {
                    (
                        device.index,
                        Device {
                            id: device.id.as_str().into(),
                            name: device.name,
                        },
                    )
                }),
            ),
        }
    }
}

fn list(kind: DeviceKind, devices: impl Iterator<Item = (usize, Device)>) -> DeviceList {
    let mut list = DeviceList::default();
    #[cfg(windows)]
    let default_id = super::windows_audio::default_id(kind);
    #[cfg(not(windows))]
    let _ = kind;

    for (index, device) in devices.filter(|(_, device)| !device.name.is_empty()) {
        #[cfg(windows)]
        let is_default = {
            let _ = index;
            default_id
                .as_ref()
                .is_some_and(|id| device.id.eq_ignore_ascii_case(id))
        };
        #[cfg(not(windows))]
        let is_default = index == 0;
        if is_default {
            list.default = Some(device.clone());
        }
        // Windows enumerates physical endpoints only. The other backends expose a default alias.
        if cfg!(windows) || !is_default {
            list.choices.push(device);
        }
    }
    list
}

pub(super) fn switch_speakers(audio: &PlatformAudio, device: &Device) -> bool {
    audio
        .switch_playout_device(&PlayoutDeviceId::from_unchecked_guid(&device.id))
        .is_ok()
}

pub(super) fn select_microphone(audio: &PlatformAudio, device: &Device) -> bool {
    audio
        .set_recording_device(&RecordingDeviceId::from_unchecked_guid(&device.id))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_devices_survive_restart_and_fall_back_until_they_return() {
        let config = tempfile::tempdir().unwrap();
        let path = config.path().join("spk-gpui/voice-devices.json");
        assert_eq!(
            DevicePreferences::load(&path).unwrap(),
            DevicePreferences::default()
        );
        let mic = Device {
            id: "index:1".into(),
            name: "USB mic".into(),
        };
        let speakers = Device {
            id: "speaker-guid".into(),
            name: "USB speakers".into(),
        };
        let saved = DevicePreferences {
            microphone: Some(mic.clone()),
            speakers: Some(speakers.clone()),
        };
        saved.save(&path).unwrap();
        assert_eq!(DevicePreferences::load(&path).unwrap(), saved);
        let default = Device {
            id: "index:0".into(),
            name: "Default".into(),
        };
        let mut list = DeviceList {
            default: Some(default.clone()),
            ..Default::default()
        };
        assert_eq!(list.resolve(Some(&mic)), Some(&default));
        list.choices.push(Device {
            id: "index:3".into(),
            ..mic.clone()
        });
        assert_eq!(list.resolve(Some(&mic)).unwrap().id, "index:3");
        list.choices.push(mic.clone());
        assert_eq!(list.resolve(Some(&mic)), Some(&default));
        list.choices.push(Device {
            id: "different-guid".into(),
            name: speakers.name.clone(),
        });
        assert_eq!(list.resolve(Some(&speakers)), Some(&default));
        list.choices.push(speakers.clone());
        assert_eq!(list.resolve(Some(&speakers)), Some(&speakers));
        DevicePreferences::default().save(&path).unwrap();
        assert_eq!(
            DevicePreferences::load(&path).unwrap(),
            DevicePreferences::default()
        );
    }
}
