use std::{collections::HashMap, fs, io, path::Path};

use livekit::{PlatformAudio, PlayoutDeviceId, RecordingDeviceId};
use rodio::cpal::{
    self,
    traits::{DeviceTrait, HostTrait},
};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DeviceKind {
    Microphone,
    Speakers,
    MediaOutput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct DevicePreferences {
    pub microphone: Option<Device>,
    pub speakers: Option<Device>,
    pub media_output: Option<Device>,
    pub radio_volume: u8,
    pub soundboard_enabled: bool,
    pub soundboard_volume: u8,
    pub screen_audio: bool,
    pub output_volume: u8,
    pub processing: VoiceProcessing,
    pub participant_volumes: HashMap<String, ParticipantVolume>,
}

impl Default for DevicePreferences {
    fn default() -> Self {
        Self {
            microphone: None,
            speakers: None,
            media_output: None,
            radio_volume: 100,
            soundboard_enabled: true,
            soundboard_volume: 70,
            screen_audio: true,
            output_volume: 100,
            participant_volumes: HashMap::new(),
            processing: VoiceProcessing::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct VoiceProcessing {
    pub echo_cancellation: bool,
    #[serde(deserialize_with = "deserialize_noise_suppression")]
    pub noise_suppression: NoiseSuppression,
    pub auto_gain: bool,
    pub sensitivity: u8,
}

impl Default for VoiceProcessing {
    fn default() -> Self {
        Self {
            echo_cancellation: true,
            noise_suppression: NoiseSuppression::Standard,
            auto_gain: true,
            sensitivity: 50,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NoiseSuppression {
    Off,
    Standard,
    Enhanced,
}

fn deserialize_noise_suppression<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<NoiseSuppression, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SavedMode {
        Mode(NoiseSuppression),
        Legacy(bool),
    }

    Ok(match SavedMode::deserialize(deserializer)? {
        SavedMode::Mode(mode) => mode,
        SavedMode::Legacy(false) => NoiseSuppression::Off,
        SavedMode::Legacy(true) => NoiseSuppression::Standard,
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct ParticipantVolume {
    pub mic: u8,
    pub screen: u8,
    pub muted: bool,
}

impl Default for ParticipantVolume {
    fn default() -> Self {
        Self {
            mic: 100,
            screen: 100,
            muted: false,
        }
    }
}

pub enum ParticipantVolumeChange {
    Microphone(u8),
    Screen(u8),
    Muted(bool),
}

impl DevicePreferences {
    pub fn participant_volume(&self, id: &str) -> ParticipantVolume {
        let mut volume = self
            .participant_volumes
            .get(id)
            .copied()
            .unwrap_or_default();
        volume.mic = volume.mic.min(200);
        volume.screen = volume.screen.min(200);
        volume
    }

    pub fn output_gain(&self) -> f32 {
        f32::from(self.output_volume.min(200)) / 100.0
    }

    pub fn get_mut(&mut self, kind: DeviceKind) -> &mut Option<Device> {
        match kind {
            DeviceKind::Microphone => &mut self.microphone,
            DeviceKind::Speakers => &mut self.speakers,
            DeviceKind::MediaOutput => &mut self.media_output,
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
    pub media_outputs: DeviceList,
}

impl Devices {
    pub fn enumerate() -> Self {
        PlatformAudio::new().map_or_else(
            |_| Self {
                media_outputs: media_outputs(),
                ..Self::default()
            },
            |audio| Self::read(&audio),
        )
    }

    pub(super) fn read(audio: &PlatformAudio) -> Self {
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
            media_outputs: media_outputs(),
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

fn media_outputs() -> DeviceList {
    fn describe(device: cpal::Device) -> Option<Device> {
        Some(Device {
            id: device.id().ok()?.to_string(),
            name: device.description().ok()?.name().to_owned(),
        })
    }

    let host = cpal::default_host();
    DeviceList {
        default: host.default_output_device().and_then(describe),
        choices: host
            .output_devices()
            .ok()
            .into_iter()
            .flatten()
            .filter_map(describe)
            .collect(),
    }
}

#[derive(Clone, PartialEq)]
pub(crate) struct MediaOutput {
    pub device: Option<Device>,
    pub volume: f32,
}

pub(crate) fn open_media_output(
    device: Option<&Device>,
    on_error: impl FnMut(cpal::Error) + Clone + Send + 'static,
) -> Result<rodio::MixerDeviceSink, rodio::DeviceSinkError> {
    use rodio::DeviceSinkBuilder;

    let open = |device| {
        DeviceSinkBuilder::from_device(device)?
            .with_error_callback(on_error.clone())
            .open_stream()
    };
    let host = cpal::default_host();
    let sink = device
        .and_then(|device| device.id.parse().ok())
        .and_then(|id| host.device_by_id(&id))
        .and_then(|device| open(device).ok());
    let mut sink = match sink {
        Some(sink) => sink,
        None => DeviceSinkBuilder::from_default_device()?
            .with_error_callback(on_error)
            .open_stream()?,
    };
    sink.log_on_drop(false);
    Ok(sink)
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

pub(super) fn route_speakers(
    list: &DeviceList,
    preferred: Option<&Device>,
    previous: Option<&Device>,
    mut switch: impl FnMut(&Device) -> bool,
) -> Option<Device> {
    let preferred = list.resolve(preferred);
    let previous = previous.and_then(|device| {
        list.choices
            .iter()
            .chain(list.default.iter())
            .find(|choice| *choice == device)
    });
    preferred
        .into_iter()
        .chain(previous.filter(|device| Some(*device) != preferred))
        .chain(
            list.default
                .as_ref()
                .filter(|device| Some(*device) != preferred && Some(*device) != previous),
        )
        .find(|device| switch(device))
        .cloned()
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

    #[cfg(windows)]
    #[test]
    #[ignore = "requires active Windows communications input and output devices"]
    fn windows_system_default_survives_media_device_enumeration() {
        let devices = Devices::enumerate();
        assert!(
            devices.microphones.default.is_some(),
            "missing communications microphone"
        );
        assert!(
            devices.speakers.default.is_some(),
            "missing communications speakers"
        );
        assert!(
            !devices.media_outputs.choices.is_empty(),
            "missing media outputs"
        );
    }

    #[test]
    fn microphone_processing_preserves_old_defaults_and_round_trips_saved_settings() {
        let mut preferences: DevicePreferences =
            serde_json::from_str(r#"{"output_volume":75}"#).unwrap();
        assert_eq!(preferences.processing, VoiceProcessing::default());
        assert!(preferences.processing.echo_cancellation);

        for (legacy, mode) in [
            ("false", NoiseSuppression::Off),
            ("true", NoiseSuppression::Standard),
        ] {
            let processing: VoiceProcessing =
                serde_json::from_str(&format!(r#"{{"noise_suppression":{legacy}}}"#)).unwrap();
            assert_eq!(processing.noise_suppression, mode);
        }

        preferences.processing = VoiceProcessing {
            echo_cancellation: false,
            noise_suppression: NoiseSuppression::Enhanced,
            auto_gain: false,
            sensitivity: 80,
        };
        let config = tempfile::tempdir().unwrap();
        let path = config.path().join("voice-devices.json");
        preferences.save(&path).unwrap();
        assert_eq!(DevicePreferences::load(&path).unwrap(), preferences);
    }

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
            media_output: Some(Device {
                id: "pulse:headphones".into(),
                name: "Headphones".into(),
            }),
            radio_volume: 125,
            soundboard_enabled: false,
            soundboard_volume: 25,
            ..Default::default()
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

#[cfg(test)]
#[test]
fn output_volume_defaults_to_unity_for_saved_device_preferences() {
    let old: DevicePreferences =
        serde_json::from_str(r#"{"microphone":null,"speakers":null}"#).unwrap();
    assert_eq!(old.output_volume, 100);
    assert_eq!(old.output_gain(), 1.0);

    let saved: DevicePreferences = serde_json::from_str(r#"{"output_volume":175}"#).unwrap();
    assert_eq!(saved.output_gain(), 1.75);

    let invalid: DevicePreferences = serde_json::from_str(r#"{"output_volume":255}"#).unwrap();
    assert_eq!(invalid.output_gain(), 2.0);
}
