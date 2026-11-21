use std::collections::HashMap;

use livekit::{prelude::ConnectionQuality, rtc_engine::SessionStats, webrtc::stats::RtcStats};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Diagnostics {
    pub quality: Option<ConnectionQuality>,
    // Store tenths to compare reports without floating-point state.
    pub jitter_ms: Option<u32>,
    pub loss_percent: Option<u32>,
    pub round_trip_ms: Option<u32>,
    pub upload_kbps: Option<u32>,
    pub download_kbps: Option<u32>,
}

struct Counters {
    timestamp: i64,
    bytes: u64,
    incoming_packets: Option<(u64, i64)>,
}

#[derive(Default)]
pub(super) struct Sampler {
    previous: HashMap<(usize, String), Counters>,
}

impl Sampler {
    pub(super) fn room(&mut self, sample: Option<&SessionStats>) -> Diagnostics {
        let Some(sample) = sample else {
            self.previous.clear();
            return Diagnostics::default();
        };
        let peer_stats = [&sample.subscriber_stats, &sample.publisher_stats];
        let mut diagnostics = Diagnostics::default();
        let mut next = HashMap::new();
        let (mut received, mut lost) = (0.0, 0.0);
        let (mut upload, mut download) = (None, None);

        for (peer, stats) in peer_stats.into_iter().enumerate() {
            for stat in stats {
                let (rtc, bytes, incoming_packets, bitrate) = match stat {
                    RtcStats::InboundRtp(stat) if stat.stream.kind == "audio" => {
                        let packets = stat.received.packets_received;
                        if packets > 0 {
                            diagnostics.jitter_ms = diagnostics
                                .jitter_ms
                                .max(tenths(stat.received.jitter * 1000.0));
                        }
                        (
                            &stat.rtc,
                            stat.inbound.bytes_received,
                            Some((packets, stat.received.packets_lost)),
                            &mut download,
                        )
                    }
                    RtcStats::OutboundRtp(stat) if stat.stream.kind == "audio" => {
                        (&stat.rtc, stat.sent.bytes_sent, None, &mut upload)
                    }
                    RtcStats::RemoteInboundRtp(stat) if stat.stream.kind == "audio" => {
                        diagnostics.jitter_ms = diagnostics
                            .jitter_ms
                            .max(tenths(stat.received.jitter * 1000.0));
                        if stat.remote_inbound.round_trip_time_measurements > 0 {
                            diagnostics.round_trip_ms = diagnostics
                                .round_trip_ms
                                .max(tenths(stat.remote_inbound.round_trip_time * 1000.0));
                        }
                        continue;
                    }
                    _ => continue,
                };
                let key = (peer, rtc.id.clone());
                if let Some(before) = self.previous.get(&key)
                    && let Some(elapsed_us) = rtc.timestamp.checked_sub(before.timestamp)
                    && elapsed_us > 0
                {
                    if let Some(bytes) = bytes.checked_sub(before.bytes) {
                        // Native WebRTC timestamps are in microseconds.
                        *bitrate.get_or_insert(0.0) += bytes as f64 * 8_000.0 / elapsed_us as f64;
                    }
                    if let (Some((packets, losses)), Some((old_packets, old_losses))) =
                        (incoming_packets, before.incoming_packets)
                        && let Some(packets) = packets.checked_sub(old_packets)
                    {
                        received += packets as f64;
                        lost += losses.saturating_sub(old_losses).max(0) as f64;
                    }
                }
                next.insert(
                    key,
                    Counters {
                        timestamp: rtc.timestamp,
                        bytes,
                        incoming_packets,
                    },
                );
            }
        }
        self.previous = next;

        if received + lost > 0.0 {
            diagnostics.loss_percent = tenths(lost / (received + lost) * 100.0);
        }
        diagnostics.upload_kbps = upload.and_then(tenths);
        diagnostics.download_kbps = download.and_then(tenths);
        diagnostics.round_trip_ms = diagnostics.round_trip_ms.or_else(|| {
            peer_stats
                .into_iter()
                .filter_map(|stats| transport_latency(stats))
                .max()
        });
        diagnostics
    }
}

fn transport_latency(stats: &[RtcStats]) -> Option<u32> {
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
}

fn tenths(value: f64) -> Option<u32> {
    (value.is_finite() && value >= 0.0).then(|| (value * 10.0).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use livekit::webrtc::stats::InboundRtpStats;

    fn inbound(id: &str, timestamp: i64, received: u64, lost: i64, jitter: f64) -> RtcStats {
        let mut stats = InboundRtpStats::default();
        stats.rtc.id = id.into();
        stats.rtc.timestamp = timestamp;
        stats.stream.kind = "audio".into();
        stats.received.packets_received = received;
        stats.received.packets_lost = lost;
        stats.received.jitter = jitter;
        RtcStats::InboundRtp(stats)
    }

    #[test]
    fn loss_uses_interval_deltas_and_reconnect_discards_previous_counters() {
        let mut sampler = Sampler::default();
        let sample = |publisher_stats| SessionStats {
            publisher_stats,
            subscriber_stats: Vec::new(),
        };
        assert_eq!(sampler.room(Some(&sample(vec![]))), Diagnostics::default());
        let first = sampler.room(Some(&sample(vec![inbound("mic", 1, 90, 10, 0.012)])));
        assert_eq!(first.jitter_ms, Some(120));
        assert_eq!(first.loss_percent, None);
        let second = sampler.room(Some(&sample(vec![
            inbound("mic", 2, 108, 12, 0.005),
            inbound("new", 2, 1, 0, 0.015),
        ])));
        assert_eq!(second.jitter_ms, Some(150));
        assert_eq!(second.loss_percent, Some(100));
        assert_eq!(sampler.room(None), Diagnostics::default());
        let first_after_reconnect =
            sampler.room(Some(&sample(vec![inbound("mic", 3, 500, 100, 0.003)])));
        assert_eq!(first_after_reconnect.jitter_ms, Some(30));
        assert_eq!(first_after_reconnect.loss_percent, None);
    }
}
