# Phase 4 verification

Use an existing SPK deployment with LiveKit, two test accounts, and a voice room. Record the deployment revision, OS version, architecture, and physical devices used. Open the web client with the second account.

1. Before joining on the desktop, open both audio pickers using the keyboard and select **System default**. Join and confirm two-way audio. Speak from each client and confirm that both show the corresponding speaking indicator.
2. Select a different physical microphone and speakers during the call. Confirm audio uses the selected hardware in each direction and both accounts stay in the room.
3. Mute on the desktop. Check that capture stops, web audio remains audible, and the web participant list shows **Muted**. Unmute and confirm capture resumes. Deafen and check that capture and playback both stop and the web shows **Deafened**. Undeafen once from each prior mute state to check that it restores that choice.
4. Disconnect the selected devices while other devices are available. Wait for SPK to show that it switched to System default, then reconnect them and check that SPK returns to each preferred device automatically. SPK scans once a second. A device that disappears and returns between scans may need to be reselected.
5. Leave and check that the web client removes the desktop member and the OS releases capture. Close and reopen SPK, confirm that the picks were restored, and join again. Test **Enable microphone** after fixing a denied permission or choosing a working input.

| Target | Local checks | Existing SPK deployment with physical devices |
| --- | --- | --- |
| macOS, Apple Silicon | Pending | Pending |
| Windows 11 build 22631, x64 MSVC | Native locked `cargo check --tests` passed. | Pending |
| Ubuntu 24.04.5 x64 under WSL2 | Formatting, Clippy, locked build, and nine tests passed. The desktop and Chromium exchanged audio through LiveKit 1.13.11 and LiveKit JS 2.23.0. Virtual PulseAudio checks confirmed live input and output switching, speaking indicators, microphone release on mute and leave, silence while deafened, mute restoration, fallback and automatic device return, and both saved routes after relaunch. | Pending native Linux desktop |

The WSL2 tests used a loopback SPK protocol fixture. The SPK deployment revision will be recorded with the physical-device runs.
