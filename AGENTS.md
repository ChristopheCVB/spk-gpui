# spk-gpui

Build a native GPUI desktop client for the self-hosted SPK chat and voice app. Target macOS, Windows, and Linux.

Implement every feature available in the SPK web app. Use the upstream web client as the reference for feature behavior, starting with voice.

## UI

- Build the desktop UI with `gpui-kit`. Follow the SPK web app's layout, terminology, and workflows where practical.

## Tooling

- Use mise to manage Rust and other versioned development tools. Pin shared tool versions in the repository's mise configuration.

## Server integration

- Embed the SPK and LiveKit URLs supplied at build time. Use the SPK URL for HTTP and WebSocket connections and the LiveKit URL for voice.
- Work with existing SPK deployments using the Nuxt app's HTTP API and WebSocket protocol. Refer to the upstream server and web client when implementing or checking integration behavior. [SPK upstream](https://github.com/meeehdi-dev/spk)
- Use SPK's authenticated voice token endpoint to connect to LiveKit. Use SPK's WebSocket protocol for voice presence so desktop and web users see the same room state. [Voice token endpoint](https://github.com/meeehdi-dev/spk/blob/main/server/api/voice/token.post.ts), [SPK voice design](https://github.com/meeehdi-dev/spk/blob/main/PLAN.md#8-voice-and-video-livekit)
- Keep deployment secrets on the server. Obtain user sessions and room tokens at runtime. Supply any required build credentials through the build environment.

## First milestone

Open the app, sign in with an existing account, and list and join voice channels. Use the system-default microphone and speakers or select devices in the app. Talk and listen, see who is speaking, toggle mute and deafen, and leave.

Verify the workflow on macOS, Windows, and Linux against an existing SPK deployment with a web client in the same room. Check two-way audio, input and output device selection, speaking indicators, mute, deafen, and leaving.
