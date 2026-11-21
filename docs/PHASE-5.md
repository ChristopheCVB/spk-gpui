# Phase 5 verification

## Automated checks

On Ubuntu 24.04.5 x64 under WSL2, the following passed on `bf360a5` using the Rust toolchain pinned in mise.

```sh
mise exec -- cargo fmt --all --check
mise exec -- cargo clippy --locked --all-targets -- -D warnings
mise exec -- cargo test --locked
mise exec -- cargo build --locked
```

All 29 tests passed. The suite used a loopback SPK protocol fixture and simulated LiveKit media events to exercise recovery, rejoin, channel switching, server-directed moves, and teardown. The host had no `/dev/snd`.

The earlier CI run for `fa9bf7d` passed formatting, locked tests, and locked debug builds on macOS 15, Windows Server 2022, and Ubuntu 24.04. The macOS development bundle passed signature verification. [Build run](https://github.com/ChristopheCVB/spk-gpui/actions/runs/44895348253)

## Physical-device checks

Use the existing SPK Instance, designated accounts, and two Voice Channels from [Phase 1](PHASE-1.md). Build with that instance's SPK and LiveKit endpoints. Open the web client with the second account and keep it available to observe Voice Presence. Coordinate server-directed actions with an Admin and use isolated network controls on the desktop host. [Build instructions](../README.md#build-and-sign-in)

| Scenario and expected outcome | macOS | Windows | Native Linux |
| --- | --- | --- | --- |
| **Sign in and join.** Confirm both clients list the same Voice Channels. Join the same channel with System Default selected for capture and playback. Speak in both directions and confirm both clients show the correct speaker and matching Voice Presence. | Pending | Pending | Pending |
| **Choose and unplug devices.** Switch to another physical microphone and speaker output during a call. Confirm the actual routes in both directions. Unplug each Preferred Device with another device available, confirm the fallback to System Default, then reconnect and confirm that the Preferred Device becomes active again. | Pending | Pending | Pending |
| **Mute and deafen.** Confirm Mute stops capture and preserves incoming audio. Confirm Deafen stops capture and incoming audio. Undeafen from both prior mute states and confirm each one is restored. Check the indicators in both clients after each change. | Pending | Pending | Pending |
| **Deny and restore microphone access.** Join with access denied. Confirm the desktop shows **Listen only**, can hear the web participant, and appears muted on the web. Restore access and use **Enable microphone**. Confirm two-way audio and updated Voice Presence. | Pending | Pending | Pending |
| **Interrupt the SPK WebSocket.** Keep LiveKit reachable and confirm two-way audio continues. Restore SPK connectivity and check that channel membership, mute, deafen, and speaking states agree on both clients. | Pending | Pending | Pending |
| **Interrupt LiveKit.** Keep SPK reachable. Observe **Reconnecting voice…**, restore connectivity, and check that the call and prior mute and deafen states recover. Repeat with an interruption long enough for a terminal failure. Confirm presence clears, then restore connectivity and use the desktop rejoin action. Verify two-way audio and matching Voice Presence. | Pending | Pending | Pending |
| **Switch or cancel.** Switch to the second Voice Channel during an active call. Check that the web sees one membership in the destination and that audio follows it. Repeat while a join is pending, and cancel a pending join. Check that late events never restore the old call or reactivate the microphone. | Pending | Pending | Pending |
| **Apply server-directed actions.** Have an Admin move the desktop participant to the second Voice Channel and then disconnect them. Verify audio and Voice Presence follow the move, then confirm the disconnect releases capture and clears membership in both clients. | Pending | Pending | Pending |
| **Hand off the same account.** Join from a separate browser profile with the desktop account while the independent web account observes. Confirm the desktop releases capture and only the replacement Voice Session remains. Leave from the replacement client and confirm Voice Presence clears. | Pending | Pending | Pending |
| **Leave, sign out, and exit.** Rejoin for each action and verify that it releases the microphone and clears Voice Presence. Check desktop state directly after leaving or signing out and after reopening following exit. | Pending | Pending | Pending |
| **Switch macOS microphones during continuous incoming speech.** Keep the speaker selection fixed, switch microphones, then switch back. Repeat with AirPods and with built-in or wired devices. Incoming audio should recover promptly at normal pitch and speed without persistent stutter or dropouts. Confirm the web participant hears each selected input and both clients stay in the channel. [Microphone-switching report](https://github.com/ChristopheCVB/spk-gpui/issues/3) | Pending | — | — |

Record the OS version and architecture, desktop build or commit, SPK revision or image tag, physical microphone and speakers, and each outcome. For the macOS microphone switch, record the symptoms, duration, and the input and output sample rates if available, including any 24 kHz to 48 kHz transition. Use an agreed test window and keep credentials and tokens out of the record.

Native hosts with physical devices and designated SPK test accounts were unavailable for this verification. [Physical-device verification issue](https://github.com/ChristopheCVB/spk-gpui/issues/10)
