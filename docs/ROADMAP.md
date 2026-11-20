# First version roadmap

Deliver a native voice client for macOS, Windows, and Linux. A user with an existing SPK account can open the app, sign in, join a voice channel, talk and listen with people using the web app, choose a microphone and speakers, see who is speaking, mute, deafen, and leave. This is the first milestone toward full SPK web feature parity. [Project instructions](../AGENTS.md)

## 1. Set up the project and test deployment

- Use an existing, unmodified SPK deployment with LiveKit, two test accounts, and two voice channels. Record the SPK revision or image tag used for testing. Use a second account in the web client as the audio and presence counterpart. [SPK setup](https://github.com/meeehdi-dev/spk#readme)
- Create a Rust binary crate with `gpui-kit` and commit its Cargo lockfile. Pin the Rust toolchain and any shared development tools with mise. Document native build prerequisites and get a minimal window running on macOS, Windows, and Linux before expanding the UI. [GPUI Kit](https://github.com/longbridge/gpui-kit), [Rust with mise](https://mise.jdx.dev/lang/rust.html)
- Define two required build inputs, `SPK_URL` and `LIVEKIT_URL`. Validate and embed them at build time, with production builds using HTTPS and WSS. Derive the SPK WebSocket URL from the SPK origin. Confirm that the supplied LiveKit endpoint matches the deployment that issues the room tokens. Keep deployment secrets on the servers and supply any required build or signing credentials through the build environment.

**Done when** a fresh checkout can build and launch the app on all three platforms and missing or invalid endpoint configuration produces a clear build error.

## 2. Sign in and show voice channels

- Build the email and password form with `gpui-kit` using the SPK web app as the layout reference. Use Better Auth's existing `POST /api/auth/sign-in/email` flow, retain its session cookies in a cookie jar scoped to the SPK origin, and use `GET /api/profile/me` to confirm the signed-in user. Use the same session for authenticated requests and send the configured SPK origin where upstream requires it. Implement sign-out through the existing auth API and clear the local session. Keep passwords out of storage and credentials out of logs. Handle sign-in errors and expired sessions with a path back to sign-in. [Web sign-in](https://github.com/meeehdi-dev/spk/blob/main/app/pages/login.vue), [SPK profile lookup](https://github.com/meeehdi-dev/spk/blob/main/app/composables/useMe.ts)
- Fetch `GET /api/channels`, keep channels of type `voice`, and preserve the server's order. Fetch `GET /api/users` for participant display names and `GET /api/voice` for the initial room membership. Show loading, empty, and retryable error states. [Channel API](https://github.com/meeehdi-dev/spk/blob/main/server/api/channels/index.get.ts), [User directory](https://github.com/meeehdi-dev/spk/blob/main/server/api/users/index.get.ts), [Voice snapshot](https://github.com/meeehdi-dev/spk/blob/main/server/api/voice/index.get.ts)
- Open SPK's `/_ws` with the session cookies and the configured SPK origin. Respond to the server's JSON heartbeat with the text frame `ping`, apply `voice:presence` updates, and refresh the channel list on `channels:changed`. [SPK WebSocket handler](https://github.com/meeehdi-dev/spk/blob/main/server/routes/_ws.ts), [Web socket client](https://github.com/meeehdi-dev/spk/blob/main/app/utils/socket-browser.ts)

**Done when** a test user sees the same voice channels and room membership as the web client and can recover from a failed sign-in.

## 3. Join a room and prove two-way audio

- Implement one active room per client. Request `POST /api/voice/token` with the selected `channelId`. Follow the web client's ordering and claim the SPK voice session with `voice:join`. Wait for the matching `voice:joined` acknowledgment using the channel ID, session ID, and nonce. Connect the LiveKit Rust SDK to the embedded LiveKit URL using the returned token, then send `voice:ready` once listening is ready. Keep the initial advertised microphone state muted until publishing succeeds. [Voice token API](https://github.com/meeehdi-dev/spk/blob/main/server/api/voice/token.post.ts), [Web voice implementation](https://github.com/meeehdi-dev/spk/blob/main/app/utils/voice-livekit.ts)
- Prove native capture and playback with physical devices on all three platforms before building the full settings UI. Verify what the chosen LiveKit Rust SDK version provides for device I/O. Evaluate CPAL for any missing capture, playback, or device selection support. Publish microphone audio and play remote microphone tracks, including when multiple participants speak. Use the system defaults for the first pass. [LiveKit Rust SDK](https://github.com/livekit/rust-sdks), [CPAL](https://github.com/RustAudio/cpal)
- Keep network and audio work off the GPUI thread. Show joining and connected states. On leave or a failed join, release any claimed SPK session, disconnect LiveKit, and stop capture and playback. If microphone access fails, show the problem and allow listen-only use when playback works.

**Done when** the desktop and web test accounts can hear each other in the same room on each platform, both see the join and leave, and leaving releases the microphone.

## 4. Add voice controls and participant indicators

- Build the voice channel list, participant view, and voice bar with `gpui-kit`, following the web app's layout and labels where practical. Provide keyboard access and visible state for the voice controls.
- List microphones and speakers separately, including a system-default choice for each. Apply input and output changes during a call. Handle a missing or unplugged device with a clear status and a path to choose another device.
- Mute the published microphone track. Deafen by stopping local playback and muting the microphone, then restore the prior mute choice on undeafen. Keep device errors and the actual microphone state reflected in the UI and SPK presence.
- Drive in-room speaking indicators from LiveKit active-speaker events. Publish local mute, deafen, and speaking changes with SPK's `voice:state` protocol, following upstream payload and update cadence. Use SPK presence for the sidebar. [Web voice state and controls](https://github.com/meeehdi-dev/spk/blob/main/app/utils/voice-livekit.ts)

**Done when** a user can verify the selected physical input and output, toggle mute and deafen, and see speaking and voice-state changes in both clients.

## 5. Handle interruptions and room changes

- Make joining cancellable. Switching channels, leaving, signing out, and closing the app must clean up pending work as well as active audio. Ignore late responses and events from replaced connections so they cannot restore a room or turn on the microphone.
- Reconnect the SPK socket with backoff and refresh presence and channels. If LiveKit is still connected, reclaim SPK presence for the same session and republish its current state. Surface LiveKit reconnect status and offer a retry after terminal failure. Preserve mute and deafen during recovery.
- Honor upstream voice moves, disconnects, and session replacement, including a second client joining with the same account. Clear the signed-in state on session invalidation and return to sign-in. [Voice event handling](https://github.com/meeehdi-dev/spk/blob/main/app/utils/voice-livekit.ts), [Server voice sessions](https://github.com/meeehdi-dev/spk/blob/main/server/utils/voicePresence.ts)
- Add focused automated checks for join acknowledgment matching, late events after leaving, and mute and deafen transitions. Exercise network loss and recovery, permission denial, unplugged devices, and a voice session handoff between clients signed in to the same account.

**Done when** interruptions leave the user with a working call or a clear way to rejoin, and both clients agree on room membership after a switch, disconnect, or leave.

## 6. Package and verify the first release

- Run Cargo formatting, lint, tests, and release builds through mise on macOS, Windows, and Linux. Package a launchable artifact for each platform. Include the microphone permission metadata and native runtime dependencies required by each package. Use the supplied production endpoints for release builds.
- Launch each artifact on its target OS. With a separate web account in the same room, verify sign-in, room listing, two-way audio, speaking indicators, input and output changes, mute, deafen, room switching, reconnecting, and leaving. Check permission prompts and confirm the microphone stops after leave and exit.
- Record the tested OS versions and architectures, the SPK version, the artifacts, and the results. Include the commands needed for developers to reproduce the build and the steps users need to launch it and grant microphone access.

**Done when** each platform has a tested artifact that completes the agreed voice workflow against the existing SPK deployment.
