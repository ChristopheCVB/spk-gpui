use std::collections::HashMap;

use livekit::{prelude::ConnectionQuality, rtc_engine::SessionStats, webrtc::stats::RtcStats};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Diagnostics {
    pub quality: Option<ConnectionQuality>,
    // Store tenths to compare reports without floating-point state.
    pub jitter_ms: Option<u32>,
    pub loss_percent: Option<u32>,
    pub round_trip_ms: Option<u32>,
}

#[derive(Default)]
pub(super) struct Sampler {
    previous: HashMap<String, (u64, i64)>,
}

impl Sampler {
    pub(super) fn room(&mut self, sample: Option<&SessionStats>) -> Diagnostics {
        let Some(sample) = sample else {
            self.previous.clear();
            return Diagnostics::default();
        };
        let mut diagnostics = self.sample(
            sample
                .subscriber_stats
                .iter()
                .chain(&sample.publisher_stats),
        );
        diagnostics.round_trip_ms = [&sample.subscriber_stats, &sample.publisher_stats]
            .into_iter()
            .filter_map(|stats| {
                let pair = stats.iter().find_map(|stat| match stat {
                    RtcStats::Transport(stat) => Some(&stat.transport.selected_candidate_pair_id),
                    _ => None,
                })?;
                stats.iter().find_map(|stat| match stat {
                    RtcStats::CandidatePair(stat)
                        if stat.rtc.id == *pair && stat.candidate_pair.responses_received > 0 =>
                    {
                        tenths(stat.candidate_pair.current_round_trip_time * 1000.0)
                    }
                    _ => None,
                })
            })
            .max();
        diagnostics
    }

    fn sample<'a>(&mut self, sample: impl IntoIterator<Item = &'a RtcStats>) -> Diagnostics {
        let mut diagnostics = Diagnostics::default();
        let mut next = HashMap::new();
        let mut received = 0.0;
        let mut lost = 0.0;
        for stat in sample {
            let RtcStats::InboundRtp(stat) = stat else {
                continue;
            };
            if stat.stream.kind != "audio" {
                continue;
            }
            let packets = stat.received.packets_received;
            let losses = stat.received.packets_lost;
            if packets > 0 {
                diagnostics.jitter_ms = diagnostics
                    .jitter_ms
                    .max(tenths(stat.received.jitter * 1000.0));
            }
            if let Some((before, previous_losses)) = self.previous.get(&stat.rtc.id)
                && let Some(packets) = packets.checked_sub(*before)
            {
                received += packets as f64;
                lost += losses.saturating_sub(*previous_losses).max(0) as f64;
            }
            next.insert(stat.rtc.id.clone(), (packets, losses));
        }
        self.previous = next;
        if received + lost > 0.0 {
            diagnostics.loss_percent = tenths(lost / (received + lost) * 100.0);
        }
        diagnostics
    }
}

fn tenths(value: f64) -> Option<u32> {
    (value.is_finite() && value >= 0.0).then(|| (value * 10.0).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use livekit::webrtc::stats::InboundRtpStats;

    fn inbound(id: &str, received: u64, lost: i64, jitter: f64) -> RtcStats {
        let mut stats = InboundRtpStats::default();
        stats.rtc.id = id.into();
        stats.stream.kind = "audio".into();
        stats.received.packets_received = received;
        stats.received.packets_lost = lost;
        stats.received.jitter = jitter;
        RtcStats::InboundRtp(stats)
    }

    #[test]
    fn loss_uses_interval_deltas_and_reconnect_discards_previous_counters() {
        let mut sampler = Sampler::default();
        assert_eq!(sampler.sample(&[]), Diagnostics::default());
        let first = sampler.room(Some(&SessionStats {
            // Single-peer-connection mode reports both directions in publisher stats.
            publisher_stats: vec![inbound("mic", 90, 10, 0.012)],
            subscriber_stats: Vec::new(),
        }));
        assert_eq!(first.jitter_ms, Some(120));
        assert_eq!(first.loss_percent, None);
        let second = sampler.sample(&[inbound("mic", 108, 12, 0.005), inbound("new", 1, 0, 0.015)]);
        assert_eq!(second.jitter_ms, Some(150));
        assert_eq!(second.loss_percent, Some(100));
        assert_eq!(sampler.room(None), Diagnostics::default());
        let first_after_reconnect = sampler.sample(&[inbound("mic", 500, 100, 0.003)]);
        assert_eq!(first_after_reconnect.jitter_ms, Some(30));
        assert_eq!(first_after_reconnect.loss_percent, None);
    }
}
