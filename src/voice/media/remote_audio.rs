use std::{
    collections::HashMap,
    num::NonZero,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    time::Duration,
};

use futures::StreamExt;
use livekit::{prelude::*, webrtc::audio_stream::native::NativeAudioStream};
use rodio::{MixerDeviceSink, Player, buffer::SamplesBuffer, mixer::Mixer};
use tokio::{sync::broadcast, task::JoinHandle, time::Instant};

use crate::voice::{
    Device, DeviceKind, Devices, Intent, audio_preview::cpal_device, devices::open_media_output,
};

use super::{processing::RenderBlock, voice_output::VoiceOutput};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Route {
    Silent,
    Voice(f32),
    Media(f32),
}

fn route(intent: &Intent, id: &str, source: TrackSource, muted: bool) -> Option<Route> {
    let volume = intent.preferred.participant_volume(id);
    let gain = match source {
        TrackSource::Microphone => volume.mic,
        TrackSource::ScreenshareAudio if intent.watching.contains(id) => volume.screen,
        _ => return None,
    };
    let gain = f32::from(gain) / 100.0 * intent.preferred.output_gain();
    Some(
        if muted || volume.muted || intent.mute.deafened || gain == 0.0 {
            Route::Silent
        } else if source == TrackSource::ScreenshareAudio {
            Route::Media(gain)
        } else {
            Route::Voice(gain)
        },
    )
}

pub(super) struct RemoteAudio {
    tracks: HashMap<TrackSid, Playback>,
    voice: Output<VoiceSink>,
    media: Output<MixerDeviceSink>,
    render: broadcast::Sender<Arc<RenderBlock>>,
    pub(super) native_playout: bool,
}

impl Default for RemoteAudio {
    fn default() -> Self {
        Self {
            tracks: HashMap::new(),
            voice: Output::default(),
            media: Output::default(),
            render: broadcast::channel(16).0,
            native_playout: false,
        }
    }
}

impl RemoteAudio {
    pub(super) fn suspend(&mut self) {
        self.tracks.clear();
        self.voice = Output::default();
        self.media = Output::default();
        self.native_playout = false;
    }

    pub(super) fn render_reference(&self) -> broadcast::Receiver<Arc<RenderBlock>> {
        self.render.subscribe()
    }

    pub(super) fn forget(&mut self, sid: &TrackSid) {
        self.tracks.remove(sid);
    }

    pub(super) fn sync(
        &mut self,
        room: &Room,
        intent: &Intent,
        devices: &Devices,
        speakers: Option<&Device>,
    ) {
        let routes = sync_subscriptions(room, intent);
        let voice = speakers.filter(|_| {
            routes
                .values()
                .any(|(_, route)| matches!(route, Route::Voice(_)))
        });
        let media = routes
            .values()
            .any(|(_, route)| matches!(route, Route::Media(_)));
        let media_device = devices
            .media_outputs
            .resolve(intent.preferred.media_output.as_ref());
        // Windows voice defaults resolve to a communications endpoint, while CPAL defaults to eConsole.
        let system_speakers = !cfg!(windows)
            && (intent.preferred.speakers.is_none()
                || devices.speakers.default.as_ref() == speakers);
        let voice_device = if system_speakers {
            devices.media_outputs.default.as_ref().or(voice)
        } else {
            voice
        };
        let voice_changed = self.voice.update(voice.is_some(), voice_device, |failed| {
            let selected = voice.filter(|_| !system_speakers);
            let device = cpal_device(DeviceKind::Speakers, selected)?;
            let reference = VoiceOutput::open(device.clone(), self.render.clone(), failed.clone());
            VoiceSink::open(device, reference, failed)
        });
        let media_changed = self.media.update(media, media_device, |failed| {
            open_media_output(media_device, move |_| failed.store(true, Relaxed)).ok()
        });
        if voice_changed || media_changed {
            self.tracks.retain(|_, playback| {
                if playback.media {
                    !media_changed
                } else {
                    !voice_changed
                }
            });
        }
        let native_playout = sync_playback(
            &mut self.tracks,
            routes,
            self.voice.sink.as_ref().map(VoiceSink::mixer),
            self.media.sink.as_ref().map(MixerDeviceSink::mixer),
        );
        self.native_playout = voice.is_some() && native_playout;
    }
}

fn sync_subscriptions(
    room: &Room,
    intent: &Intent,
) -> HashMap<TrackSid, (RemoteAudioTrack, Route)> {
    let mut routes = HashMap::new();
    for participant in room.remote_participants().into_values() {
        let id = participant.identity();
        for publication in participant.track_publications().into_values() {
            if publication.kind() != TrackKind::Audio {
                continue;
            }
            let route = route(intent, &id.0, publication.source(), publication.is_muted());
            if let Some(RemoteTrack::Audio(track)) = publication.track() {
                if route.is_none() && track.is_enabled() {
                    track.disable();
                }
                if let Some(route) = route {
                    routes.insert(publication.sid(), (track, route));
                }
            }
            if publication.is_desired() != route.is_some() {
                publication.set_subscribed(route.is_some());
            }
        }
    }
    routes
}

fn sync_playback(
    tracks: &mut HashMap<TrackSid, Playback>,
    routes: HashMap<TrackSid, (RemoteAudioTrack, Route)>,
    voice: Option<&Mixer>,
    media: Option<&Mixer>,
) -> bool {
    tracks.retain(|sid, playback| {
        !playback.task.is_finished()
            && routes.get(sid).is_some_and(|(_, route)| match route {
                Route::Voice(_) => voice.is_some(),
                Route::Media(_) => media.is_some(),
                _ => false,
            })
    });
    let mut native_playout = false;
    for (sid, (track, route)) in routes {
        // shortcut: LiveKit fallback has no render reference, remove when all supported routes can provide PCM.
        let native = matches!(route, Route::Voice(1.0)) && voice.is_none();
        native_playout |= native;
        if track.is_enabled() != native {
            if native {
                track.enable();
            } else {
                track.disable();
            }
        }
        let (mixer, gain, media) = match route {
            Route::Voice(gain) => (voice, gain, false),
            Route::Media(gain) => (media, gain, true),
            Route::Silent => continue,
        };
        if let Some(mixer) = mixer {
            let playback = tracks
                .entry(sid)
                .or_insert_with(|| Playback::start(track, mixer, gain, media));
            playback.player.set_volume(gain);
        }
    }
    native_playout
}

enum VoiceSink {
    Reference(VoiceOutput),
    Playback(MixerDeviceSink),
}

impl VoiceSink {
    fn open(
        device: rodio::cpal::Device,
        reference: Option<VoiceOutput>,
        failed: Arc<AtomicBool>,
    ) -> Option<Self> {
        reference.map(Self::Reference).or_else(|| {
            failed.store(false, Relaxed);
            let mut sink = rodio::DeviceSinkBuilder::from_device(device)
                .ok()?
                .with_error_callback(move |_| failed.store(true, Relaxed))
                .open_sink_or_fallback()
                .ok()?;
            sink.log_on_drop(false);
            Some(Self::Playback(sink))
        })
    }

    fn mixer(&self) -> &Mixer {
        match self {
            Self::Reference(output) => output.mixer(),
            Self::Playback(output) => output.mixer(),
        }
    }
}

struct Output<T> {
    device: Option<Device>,
    sink: Option<T>,
    failed: Arc<AtomicBool>,
    retry_at: Option<Instant>,
}

impl<T> Default for Output<T> {
    fn default() -> Self {
        Self {
            device: None,
            sink: None,
            failed: Arc::default(),
            retry_at: None,
        }
    }
}

impl<T> Output<T> {
    fn update(
        &mut self,
        wanted: bool,
        device: Option<&Device>,
        open: impl FnOnce(Arc<AtomicBool>) -> Option<T>,
    ) -> bool {
        if !wanted {
            return self.sink.take().is_some();
        }
        let now = Instant::now();
        let changed = self.device.as_ref() != device || self.failed.load(Relaxed);
        if !changed && (self.sink.is_some() || self.retry_at.is_some_and(|at| now < at)) {
            return false;
        }
        self.sink = None;
        self.device = device.cloned();
        self.failed = Arc::default();
        self.sink = open(self.failed.clone());
        self.retry_at = self.sink.is_none().then_some(now + Duration::from_secs(1));
        true
    }
}

struct Playback {
    player: Arc<Player>,
    task: JoinHandle<()>,
    media: bool,
}

impl Playback {
    fn start(track: RemoteAudioTrack, mixer: &Mixer, gain: f32, media: bool) -> Self {
        const RATE: NonZero<u32> = NonZero::new(48_000).unwrap();
        const CHANNELS: NonZero<u16> = NonZero::new(2).unwrap();
        let mut stream =
            NativeAudioStream::new(track.rtc_track(), RATE.get() as i32, CHANNELS.get().into());
        let player = Arc::new(Player::connect_new(mixer));
        player.set_volume(gain);
        let task = tokio::spawn({
            let player = player.clone();
            async move {
                while let Some(frame) = stream.next().await {
                    // Keep the device queue short even when its clock falls behind the network.
                    if player.len() >= 4 {
                        continue;
                    }
                    let samples = frame
                        .data
                        .iter()
                        .map(|sample| f32::from(*sample) / 32768.0)
                        .collect::<Vec<_>>();
                    player.append(SamplesBuffer::new(CHANNELS, RATE, samples));
                }
            }
        });
        Self {
            player,
            task,
            media,
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.player.stop();
        self.task.abort();
    }
}
