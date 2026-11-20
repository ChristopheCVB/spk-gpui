# Phase 3 verification

Build with the deployment endpoints and use the two designated accounts and rooms from [Phase 1](PHASE-1.md). Use headphones or separate machines to avoid acoustic feedback. Record the SPK revision or image tag, OS version, and architecture with each result.

1. Sign in on the desktop and open the web client with the second account. Join the same voice channel and confirm both clients show the desktop member when listening is ready.
2. Speak from each client in turn. Confirm the other client hears the audio on its default output. Have two web participants speak to verify concurrent playback when a third test account is available.
3. Select **Leave** on the desktop. Confirm the web client removes the desktop member and that the microphone is released. Join again and repeat by closing the desktop window.
4. Deny microphone access while keeping speakers available. Join and check that the desktop shows **Listen only**, the web sees a muted member, and web audio can still be heard. Restore the original permission after the check.
5. Cancel a join before it completes, then join the second room. Confirm SPK never shows a late join to the first room and the microphone stays off until the second call.
6. On Windows, confirm audio uses the default communications devices for input and output. If the communications and general defaults differ, confirm the call uses the communications pair.

| Target | Local verification | Production audio and presence |
| --- | --- | --- |
| macOS 15, Apple Silicon | AVFoundation microphone permission code type-checked for Apple Silicon. Native build and launch pending. | Pending |
| Windows 11 build 22631, x64 MSVC | Locked build and all six tests passed. A local call using the desktop voice worker delivered decoded microphone frames to a LiveKit peer. Windows reported active playback on its default communications output. SPK marked the microphone unmuted after capture started, and the worker left cleanly. | User confirmed the rebuilt app works with the configured SPK deployment after the microphone fix. |
| Ubuntu 24.04 x64 under WSL2 | Locked build, six tests, and Clippy passed. The desktop exchanged audio with a local LiveKit 1.13.11 test peer and with Chromium using LiveKit JS 2.23.0. Both used virtual PulseAudio devices and an SPK protocol fixture. Rejoin worked, and Leave and window close released capture. | Pending native desktop and physical audio |
