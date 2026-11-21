use std::{ffi::CString, os::unix::net::UnixStream, time::Duration};

use futures::io::AsyncReadExt;
use pulseaudio::{
    Client, RecordBuffer, RecordStream, cookie_path_from_env,
    protocol::{
        ChannelMap, ChannelPosition, RecordStreamParams, SampleFormat, SampleSpec,
        stream::{BufferAttr, StreamFlags},
    },
    socket_path_from_env,
};

use super::{CaptureSink, Device};

const TIMEOUT: Duration = Duration::from_secs(2);
const FRAME_BYTES: usize = 480 * 2 * 2;

pub(super) struct Monitor {
    stream: RecordStream,
    buffer: RecordBuffer,
}

impl Monitor {
    pub(super) async fn open(playback: Option<Device>) -> Option<Self> {
        tokio::time::timeout(TIMEOUT, async {
            let client = tokio::task::spawn_blocking(|| {
                let socket = UnixStream::connect(socket_path_from_env()?).ok()?;
                socket.set_read_timeout(Some(TIMEOUT)).ok()?;
                socket.set_write_timeout(Some(TIMEOUT)).ok()?;
                let cookie = cookie_path_from_env().and_then(|path| std::fs::read(path).ok());
                Client::new_unix(c"SPK Screen Share", socket, cookie).ok()
            })
            .await
            .ok()??;
            let sinks = client.list_sinks().await.ok()?;
            let name = matching_sink(
                playback.as_ref(),
                sinks.iter().map(|sink| {
                    let name = sink.name.to_str().unwrap_or_default();
                    let description = sink.description.as_ref().unwrap_or(&sink.name);
                    (name, description.to_str().unwrap_or_default())
                }),
            )?;
            let sink = client
                .sink_info_by_name(CString::new(name).ok()?)
                .await
                .ok()?;
            let source = client.source_info(sink.monitor_source_index?).await.ok()?;
            if source.monitor_of_sink_index != Some(sink.index) {
                return None;
            }

            let buffer = RecordBuffer::new(FRAME_BYTES * 4);
            let sample_spec = SampleSpec {
                format: SampleFormat::S16Le,
                sample_rate: 48_000,
                channels: 2,
            };
            let stream = client
                .create_record_stream(
                    RecordStreamParams {
                        source_name: Some(source.name),
                        sample_spec,
                        channel_map: ChannelMap::new([
                            ChannelPosition::FrontLeft,
                            ChannelPosition::FrontRight,
                        ]),
                        buffer_attr: BufferAttr {
                            fragment_size: FRAME_BYTES as u32,
                            max_length: FRAME_BYTES as u32 * 4,
                            ..Default::default()
                        },
                        flags: StreamFlags {
                            start_corked: true,
                            // A removed output must never reroute capture to the microphone.
                            no_move: true,
                            adjust_latency: true,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    buffer.as_record_sink(),
                )
                .await
                .ok()?;
            (stream.sink() == source.index && *stream.sample_spec() == sample_spec)
                .then_some(Self { stream, buffer })
        })
        .await
        .ok()?
    }

    pub(super) async fn capture(mut self, sink: &CaptureSink) {
        if tokio::time::timeout(TIMEOUT, self.stream.uncork())
            .await
            .is_ok_and(|result| result.is_ok())
        {
            let mut bytes = [0; FRAME_BYTES];
            while tokio::time::timeout(TIMEOUT, self.buffer.read_exact(&mut bytes))
                .await
                .is_ok_and(|result| result.is_ok())
            {
                sink.audio(
                    bytes
                        .chunks_exact(2)
                        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                        .collect(),
                );
            }
        }
        sink.audio_unavailable();
    }
}

pub(super) fn matching_sink<'a>(
    playback: Option<&Device>,
    sinks: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<&'a str> {
    let Some(playback) = playback else {
        return Some("@DEFAULT_SINK@");
    };
    if playback.id == "index:0" {
        return Some("@DEFAULT_SINK@");
    }

    // The LiveKit Linux ADM exposes PulseAudio's display description without a stable GUID.
    let mut matches = sinks
        .into_iter()
        .filter(|(name, description)| {
            !name.is_empty()
                && if playback.id.starts_with("index:") {
                    *description == playback.name
                } else {
                    *name == playback.id.strip_prefix("pulse:").unwrap_or(&playback.id)
                }
        })
        .map(|(name, _)| name);
    let matched = matches.next()?;
    matches.next().is_none().then_some(matched)
}
