use super::*;

const CONNECTING_AFTER: Duration = Duration::from_secs(4);
const REPAIR_AFTER: Duration = Duration::from_secs(8);

pub(super) type MicrophoneSamples = HashMap<String, MicrophoneSample>;

#[cfg_attr(test, derive(Clone))]
pub(super) enum MicrophoneSample {
    ParticipantMissing,
    PublicationMissing,
    Unsubscribed(TrackSid),
    Subscribed {
        sid: TrackSid,
        packets: Option<(String, u64)>,
    },
}

impl MicrophoneSample {
    fn stalled(&self, previous: Option<&Self>) -> bool {
        match self {
            Self::ParticipantMissing | Self::PublicationMissing | Self::Unsubscribed(_) => true,
            Self::Subscribed {
                sid,
                packets: Some((stream, packets)),
            } => matches!(
                previous,
                Some(Self::Subscribed { sid: before_sid, packets: Some((before_stream, before_packets)) })
                    if sid == before_sid && stream == before_stream && packets == before_packets
            ),
            Self::Subscribed { packets: None, .. } => false,
        }
    }

    fn sid(&self) -> Option<&TrackSid> {
        match self {
            Self::Unsubscribed(sid) | Self::Subscribed { sid, .. } => Some(sid),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(super) struct MicrophoneWatchdog {
    observations: HashMap<String, (MicrophoneSample, Option<Episode>)>,
}

struct Episode {
    since: Instant,
    repaired: bool,
}

impl MicrophoneWatchdog {
    pub(super) fn retain(&mut self, intent: &Intent, output_selected: bool) {
        self.observations
            .retain(|id, _| output_selected && intent.expects_remote_microphone(id));
    }

    pub(super) fn observe(
        &mut self,
        intent: &Intent,
        samples: MicrophoneSamples,
        now: Instant,
    ) -> Vec<(String, TrackSid)> {
        let mut repairs = Vec::new();
        for (id, sample) in samples {
            if !intent.expects_remote_microphone(&id) {
                continue;
            }
            let previous = self.observations.get_mut(&id);
            let episode = if sample.stalled(previous.as_deref().map(|(sample, _)| sample)) {
                let mut episode =
                    previous
                        .and_then(|(_, episode)| episode.take())
                        .unwrap_or(Episode {
                            since: now,
                            repaired: false,
                        });
                if now - episode.since >= REPAIR_AFTER
                    && !episode.repaired
                    && let Some(sid) = sample.sid()
                {
                    repairs.push((id.clone(), sid.clone()));
                    episode.repaired = true;
                }
                Some(episode)
            } else {
                None
            };
            self.observations.insert(id, (sample, episode));
        }
        repairs
    }

    pub(super) fn connecting(&self, now: Instant) -> HashSet<String> {
        self.observations
            .iter()
            .filter(|(_, (_, episode))| {
                episode
                    .as_ref()
                    .is_some_and(|episode| now - episode.since >= CONNECTING_AFTER)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}
