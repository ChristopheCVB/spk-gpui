use std::{ffi::c_void, os::fd::AsRawFd, ptr::NonNull};

use ashpd::{
    Error, PortalError,
    desktop::{
        ResponseError, Session,
        screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    },
};
use futures::StreamExt;
use tokio::{sync::oneshot, task::JoinHandle};

use super::*;

const PORTAL_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) async fn pick(playback: Option<Device>) -> Result<Selection, CaptureError> {
    let (portal, mut session, sources, cursor) = timeout(PORTAL_TIMEOUT, async {
        // A dedicated connection also cleans up the portal if creation is cancelled.
        let connection = zbus::Connection::session()
            .await
            .map_err(|_| CaptureError::Unavailable)?;
        let portal = Screencast::with_connection(connection)
            .await
            .map_err(portal_error)?;
        let sources = portal
            .available_source_types()
            .await
            .map_err(portal_error)?
            & SourceType::all();
        if sources.is_empty() {
            return Err(CaptureError::Unavailable);
        }
        let embedded = portal
            .available_cursor_modes()
            .await
            .is_ok_and(|modes| modes.contains(CursorMode::EMBEDDED));
        let cursor = if embedded {
            CursorMode::EMBEDDED
        } else {
            CursorMode::HIDDEN
        };

        let session = portal
            .create_session(Default::default())
            .await
            .map_err(portal_error)?;
        let session = PortalSession::new(session).await?;
        portal
            .select_sources(
                &session.handle,
                SelectSourcesOptions::default()
                    .set_sources(sources)
                    .set_multiple(false)
                    .set_cursor_mode(cursor),
            )
            .await
            .map_err(portal_error)?
            .response()
            .map_err(portal_error)?;
        Ok((portal, session, sources, cursor))
    })
    .await
    .map_err(|_| CaptureError::Unavailable)??;

    let response = tokio::select! {
        biased;
        response = portal.start(&session.handle, None, Default::default()) => {
            response.map_err(portal_error)?.response().map_err(portal_error)?
        }
        _ = &mut session.closed => return Err(CaptureError::PortalSelection),
    };
    let [stream] = response.streams() else {
        return Err(CaptureError::PortalSelection);
    };
    let source = stream
        .source_type()
        .or_else(|| (sources.bits().count_ones() == 1).then_some(sources));
    let label = match source {
        Some(SourceType::MONITOR) => "Display",
        Some(SourceType::WINDOW) => "Window",
        Some(SourceType::VIRTUAL) => "Virtual display",
        _ => "Screen Share",
    };
    let display = source == Some(SourceType::MONITOR) && sources.contains(SourceType::MONITOR);
    let remote = timeout(
        PORTAL_TIMEOUT,
        portal.open_pipe_wire_remote(&session.handle, Default::default()),
    )
    .await
    .map_err(|_| CaptureError::Unavailable)?
    .map_err(portal_error)?;
    let node = stream.pipe_wire_node_id();
    let pipewire = tokio::task::spawn_blocking(move || {
        // WebRTC duplicates the portal's file descriptor when opening the remote.
        NonNull::new(unsafe {
            spk_pipewire_stream_open(node, remote.as_raw_fd(), cursor == CursorMode::EMBEDDED)
        })
        .map(PipeWireStream)
    })
    .await
    .map_err(|_| CaptureError::Unavailable)?
    .ok_or(CaptureError::Unavailable)?;
    let video = VideoCapture {
        frames: Arc::default(),
        backend: VideoBackend::Wayland(PortalVideo { pipewire, session }),
    };
    ready_selection(video, label.into(), display, playback).await
}

fn portal_error(error: Error) -> CaptureError {
    match error {
        Error::Response(ResponseError::Cancelled) | Error::Portal(PortalError::Cancelled(_)) => {
            CaptureError::Cancelled
        }
        Error::Portal(PortalError::NotAllowed(_)) => CaptureError::Permission,
        Error::Response(ResponseError::Other) | Error::Portal(PortalError::Failed(_)) => {
            CaptureError::PortalSelection
        }
        _ => CaptureError::Unavailable,
    }
}

struct PortalSession {
    handle: Arc<Session<Screencast>>,
    closed: JoinHandle<()>,
}

impl PortalSession {
    async fn new(handle: Session<Screencast>) -> Result<Self, CaptureError> {
        let handle = Arc::new(handle);
        let watcher = handle.clone();
        let (ready, ready_rx) = oneshot::channel();
        let closed = tokio::spawn(async move {
            match watcher.receive_closed().await {
                Ok(closed) => {
                    let _ = ready.send(Ok(()));
                    futures::pin_mut!(closed);
                    closed.next().await;
                }
                Err(error) => {
                    let _ = ready.send(Err(portal_error(error)));
                }
            }
        });
        let session = Self { handle, closed };
        ready_rx.await.map_err(|_| CaptureError::Unavailable)??;
        Ok(session)
    }
}

impl Drop for PortalSession {
    fn drop(&mut self) {
        if !self.closed.is_finished() {
            self.closed.abort();
            let handle = self.handle.clone();
            tokio::spawn(async move {
                let _ = timeout(PORTAL_TIMEOUT, handle.close()).await;
            });
        }
    }
}

pub(super) struct PortalVideo {
    pipewire: PipeWireStream,
    session: PortalSession,
}

impl PortalVideo {
    pub(super) fn request_frame(&self, frames: &Mutex<FrameStatus>) -> Result<(), CaptureError> {
        if self.session.closed.is_finished() {
            return Err(CaptureError::SourceLost);
        }
        unsafe {
            spk_pipewire_stream_frame(
                self.pipewire.0.as_ptr(),
                (frames as *const Mutex<FrameStatus>).cast(),
                on_frame,
            )
        };
        Ok(())
    }
}

struct PipeWireStream(NonNull<c_void>);

// The stream has an internal PipeWire thread and is moved between Rust tasks with unique ownership.
unsafe impl Send for PipeWireStream {}

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        unsafe { spk_pipewire_stream_free(self.0.as_ptr()) };
    }
}

type FrameCallback = unsafe extern "C" fn(*const c_void, *const u8, i32, i32, i32);

unsafe extern "C" {
    fn spk_pipewire_stream_open(node: u32, remote: i32, cursor: bool) -> *mut c_void;
    fn spk_pipewire_stream_frame(
        stream: *mut c_void,
        frames: *const c_void,
        on_frame: FrameCallback,
    );
    fn spk_pipewire_stream_free(stream: *mut c_void);
}

unsafe extern "C" fn on_frame(
    frames: *const c_void,
    pixels: *const u8,
    width: i32,
    height: i32,
    stride: i32,
) {
    // The Rust state and WebRTC frame both remain alive for the synchronous callback.
    let state = unsafe { &*frames.cast::<Mutex<FrameStatus>>() };
    unsafe { state.lock().unwrap().receive(pixels, width, height, stride) };
}
