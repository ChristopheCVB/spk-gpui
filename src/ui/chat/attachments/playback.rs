use super::{
    audio::{AudioPlayback, BAD_OUTPUT},
    *,
};
use crate::voice::MediaOutput;

enum PlaybackTick {
    Continue,
    ReplaceOutput(MediaOutput, SamplesBuffer),
    Stop,
}

pub(super) struct Playback {
    pub(super) id: String,
    token: Uuid,
    audio: Option<AudioPlayback>,
    pub(super) started: Option<Instant>,
    ending: bool,
    elapsed: Duration,
}

impl Playback {
    pub(super) fn position(&self) -> Duration {
        self.elapsed + self.started.map_or(Duration::ZERO, |start| start.elapsed())
    }

    fn toggle(&mut self) {
        if let Some(start) = self.started.take() {
            self.elapsed += start.elapsed();
            if let Some(audio) = &self.audio {
                audio.player.pause();
            }
        } else {
            self.started = Some(Instant::now());
            if let Some(audio) = &self.audio {
                audio.player.play();
            }
        }
    }

    fn replace_output(
        &mut self,
        output: Result<AudioPlayback, &'static str>,
        samples: SamplesBuffer,
    ) {
        let position = self.position();
        self.audio = None;
        self.audio = output
            .inspect(|audio| audio.attach(samples, position, self.started.is_some()))
            .ok();
    }

    fn advance(&mut self, video_duration: Option<Duration>, volume: f32) -> (bool, bool) {
        if let Some(audio) = &self.audio {
            audio.player.set_volume(volume);
        }
        let failed = self.audio.as_ref().is_some_and(AudioPlayback::failed);
        if failed {
            self.audio = None;
        }
        let ended = match video_duration {
            Some(duration) => self.position() >= duration,
            None => self.audio.as_ref().is_none_or(|audio| audio.player.empty()),
        };
        // Keep the audio device open for one tick while it drains its last samples.
        let finished = ended && (self.ending || failed);
        self.ending = ended;
        (failed, finished)
    }
}

impl Spk {
    pub(super) fn toggle_attachment_media(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(playback) = self
            .chat_attachments
            .playback
            .as_mut()
            .filter(|playback| playback.id == id)
        {
            self.chat_attachments.media_start = None;
            playback.toggle();
            cx.notify();
            return;
        }
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let Some(card) = self
            .chat_attachments
            .cards
            .get_mut(&id)
            .filter(|card| card.pending.is_none())
        else {
            return;
        };
        let Some(preview) = card.preview.as_ref().filter(|preview| preview.is_media()) else {
            return;
        };
        let video_duration = match preview {
            Preview::Video(clip) => Some(clip.duration),
            _ => None,
        };
        let samples = preview.audio_samples();
        let output = sidebar.media_output();
        let operation = Uuid::new_v4();
        card.pending = Some(operation);
        card.error = None;
        self.chat_attachments.playback = None;
        self.chat_attachments.media_start = Some(operation);
        cx.spawn_in(window, async move |app, cx| {
            run_playback(app, cx, id, operation, output, samples, video_duration).await;
        })
        .detach();
        cx.notify();
    }
}

async fn run_playback(
    app: WeakEntity<Spk>,
    cx: &mut AsyncWindowContext,
    id: String,
    operation: Uuid,
    mut output: MediaOutput,
    samples: Option<SamplesBuffer>,
    video_duration: Option<Duration>,
) {
    let result = match samples {
        Some(samples) => open_playback(cx, output.clone()).await.map(|audio| {
            audio.attach(samples, Duration::ZERO, false);
            Some(audio)
        }),
        None => Ok(None),
    };
    let started = app
        .update_in(cx, |app, _, cx| {
            let Some(card) = app
                .chat_attachments
                .cards
                .get_mut(&id)
                .filter(|card| card.pending == Some(operation))
            else {
                return false;
            };
            card.pending = None;
            cx.notify();
            if app.chat_attachments.media_start != Some(operation) {
                return false;
            }
            app.chat_attachments.media_start = None;
            if !card.preview.as_ref().is_some_and(Preview::is_media) {
                return false;
            }
            let audio = match result {
                Ok(audio) => audio,
                Err(error) => {
                    card.error = Some(error);
                    if video_duration.is_none() {
                        return false;
                    }
                    None
                }
            };
            let mut playback = Playback {
                id: id.clone(),
                token: operation,
                audio,
                started: None,
                elapsed: Duration::ZERO,
                ending: false,
            };
            playback.toggle();
            app.chat_attachments.playback = Some(playback);
            true
        })
        .unwrap_or(false);
    if !started {
        return;
    }
    let interval = Duration::from_millis(if video_duration.is_some() { 40 } else { 120 });
    loop {
        cx.background_executor().timer(interval).await;
        let next_output = app
            .update_in(cx, |app, _, cx| {
                let View::SignedIn(sidebar) = &app.view else {
                    return PlaybackTick::Stop;
                };
                let selected = sidebar.media_output();
                let Some(playback) = app
                    .chat_attachments
                    .playback
                    .as_mut()
                    .filter(|playback| playback.token == operation)
                else {
                    return PlaybackTick::Stop;
                };
                let card = app.chat_attachments.cards.get_mut(&id);
                if output.device != selected.device
                    && let Some(samples) = card
                        .as_ref()
                        .and_then(|card| card.preview.as_ref())
                        .and_then(Preview::audio_samples)
                {
                    return PlaybackTick::ReplaceOutput(selected, samples);
                }
                let (failed, finished) = playback.advance(video_duration, selected.volume);
                if failed && let Some(card) = card {
                    card.error = Some(BAD_OUTPUT);
                }
                if finished {
                    app.chat_attachments.playback = None;
                    cx.notify();
                    return PlaybackTick::Stop;
                }
                if failed || (video_duration.is_some() && playback.started.is_some()) {
                    cx.notify();
                }
                PlaybackTick::Continue
            })
            .unwrap_or(PlaybackTick::Stop);
        let (selected, samples) = match next_output {
            PlaybackTick::Stop => break,
            PlaybackTick::Continue => continue,
            PlaybackTick::ReplaceOutput(output, samples) => (output, samples),
        };
        output = selected;
        let result = open_playback(cx, output.clone()).await;
        let _ = app.update_in(cx, |app, _, cx| {
            let Some(playback) = app
                .chat_attachments
                .playback
                .as_mut()
                .filter(|playback| playback.token == operation)
            else {
                return;
            };
            playback.replace_output(result, samples);
            if let Some(card) = app.chat_attachments.cards.get_mut(&id) {
                card.error = playback.audio.is_none().then_some(BAD_OUTPUT);
            }
            cx.notify();
        });
    }
}

async fn open_playback(
    cx: &AsyncWindowContext,
    output: MediaOutput,
) -> Result<AudioPlayback, &'static str> {
    cx.background_executor()
        .spawn(async move { AudioPlayback::open(output) })
        .await
}
