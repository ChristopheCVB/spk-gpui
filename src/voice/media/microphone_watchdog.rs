use super::*;

const CONNECTING_AFTER: Duration = Duration::from_secs(4);
const REPAIR_AFTER: Duration = Duration::from_secs(8);
const REJOIN_AFTER: Duration = Duration::from_secs(20);

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

enum Progress {
    Stalled,
    Advancing,
    Unknown,
}

impl MicrophoneSample {
    fn progress(&self, previous: Option<&Self>) -> Progress {
        match (self, previous) {
            (Self::ParticipantMissing | Self::PublicationMissing | Self::Unsubscribed(_), _) => {
                Progress::Stalled
            }
            (
                Self::Subscribed {
                    sid,
                    packets: Some((stream, packets)),
                },
                Some(Self::Subscribed {
                    sid: previous_sid,
                    packets: Some((previous_stream, previous_packets)),
                }),
            ) if sid == previous_sid && stream == previous_stream => {
                match packets.cmp(previous_packets) {
                    std::cmp::Ordering::Equal => Progress::Stalled,
                    std::cmp::Ordering::Greater => Progress::Advancing,
                    std::cmp::Ordering::Less => Progress::Unknown,
                }
            }
            _ => Progress::Unknown,
        }
    }

    fn participant_present(&self) -> bool {
        !matches!(self, Self::ParticipantMissing)
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
    confirmed_since: Option<Instant>,
    repaired: bool,
}

impl MicrophoneWatchdog {
    pub(super) fn retain(&mut self, intent: &Intent, output_selected: bool) {
        self.observations
            .retain(|id, _| output_selected && intent.expects_remote_microphone(id));
    }

    pub(super) fn participant_left(&mut self, id: &str) {
        self.observations.remove(id);
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
            let progress = sample.progress(previous.as_deref().map(|(sample, _)| sample));
            let mut episode = previous.and_then(|(_, episode)| episode.take());
            match progress {
                Progress::Advancing => episode = None,
                Progress::Unknown => {
                    if let Some(episode) = &mut episode {
                        episode.confirmed_since = None;
                    }
                }
                Progress::Stalled => {
                    let episode = episode.get_or_insert(Episode {
                        since: now,
                        confirmed_since: None,
                        repaired: false,
                    });
                    if sample.participant_present() {
                        episode.confirmed_since.get_or_insert(now);
                    } else {
                        episode.confirmed_since = None;
                    }
                    if now - episode.since >= REPAIR_AFTER
                        && !episode.repaired
                        && let Some(sid) = sample.sid()
                    {
                        repairs.push((id.clone(), sid.clone()));
                        episode.repaired = true;
                    }
                }
            }
            self.observations.insert(id, (sample, episode));
        }
        repairs
    }

    pub(super) fn widespread_stall(
        &self,
        intent: &Intent,
        now: Instant,
        output_since: Instant,
    ) -> bool {
        if now - output_since < REJOIN_AFTER {
            return false;
        }
        let mut count = 0;
        for id in intent
            .remote_microphone_mutes
            .keys()
            .filter(|id| intent.expects_remote_microphone(id))
        {
            let Some((sample, episode)) = self.observations.get(id) else {
                return false;
            };
            if !sample.participant_present() {
                continue;
            }
            if !episode
                .as_ref()
                .and_then(|episode| episode.confirmed_since)
                .is_some_and(|since| now - since >= REJOIN_AFTER)
            {
                return false;
            }
            count += 1;
        }
        count >= 2
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
