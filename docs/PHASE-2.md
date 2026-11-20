# Phase 2 verification

Build with the intended SPK and LiveKit endpoints. Use the two designated existing accounts and voice rooms from [Phase 1](PHASE-1.md). Keep credentials and session tokens out of the verification record.

1. In the desktop app, enter an incorrect password once, then confirm that the correct password signs in.
2. Close and relaunch SPK. Confirm the session restores and shows the same voice channel order as the web client.
3. On the second account in the web client, join, switch, and leave voice rooms. Confirm that the desktop sidebar shows the same membership after each change.
4. Interrupt the desktop connection. Move the web account to another room and reconnect the desktop. Confirm that the sidebar catches up.
5. Sign out on the desktop and relaunch. Confirm that the sign-in form opens.

Record the OS, architecture, SPK deployment revision, and result for each target.

| Target | Local verification | Production result |
| --- | --- | --- |
| macOS | Pending | Pending |
| Windows | Windows 11 build 22631, x64 MSVC. Locked build and all five tests passed. The desktop window opened and closed successfully with the endpoint variables unset at launch. | Pending |
| Linux | Ubuntu 24.04 x64 under WSL2. The desktop app passed the loopback sign-in, room membership, reconnect, session restore, and sign-out flow with an isolated Secret Service. | Pending |
