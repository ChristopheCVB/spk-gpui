# SPK desktop UI alignment verification

The interactive pass covered Windows x64 and the Linux X11 client under WSL2. An interactive macOS smoke pass is pending. The integrated Desktop build for the paired Windows captures was `cd46851149c264bce9e9f16532c8e104849279d9`. Individual workflow checks also used intermediate builds as the changes landed. The comparison used the Desktop baseline `379373bb`, SPK Web `62ea475664831ac016a972555b4a1cedc6e9a353`, and GPUI Kit 0.10.0. [Pinned baseline comparison][baseline] · [Alignment pull request and captures][pr] · [Verification issue][issue]

## Paired Windows and Web captures

Windows 11 x64 build 22631 used a 1920 × 1080 display at 96 DPI (100% scaling). The browser used device pixel ratio 1 and a CSS viewport equal to each measured Windows client area. The paired captures show the populated Text Channel in Light and Dark at these sizes. [Alignment pull request and captures][pr]

| Window state | Native client area and Web CSS viewport |
| --- | --- |
| Normal, from the default bounds | 1200 × 760 |
| Windows Snap, side by side | 958 × 1000 |
| Logical minimum | 560 × 440 |

A dedicated SPK Instance from the pinned Web revision served both clients for the matched chat captures and authenticated checks. It used a disposable SQLite database, a seeded Admin and regular user, three Text Channels, three Voice Channels, Text Channel and DM Pins, a Ping, and 60 newer messages before each archive target. Invite Link and password recovery used a disposable user. Object storage was unconfigured.

The separate Web UI sandbox used synthetic participants, presence, media, and status. It supplied populated chat, unread and Ping cues, a DM Conversation, Pins, hidden-sidebar Voice Session controls, populated and empty Voice Channels, joining, reconnecting, and microphone or playback errors. Only the authenticated fixture exercised account recovery, Admin mutations, older history pagination, and a real LiveKit connection.

## Interactive workflows

| Surface | Observed behavior |
| --- | --- |
| Navigation and keyboard | Windows and Linux showed the drawer at narrow widths and a resizable sidebar at wide widths. At 560 × 440, the Windows drawer exposed destinations and Profile, Escape returned focus to Open sidebar, and Enter reopened it. On Linux X11, the Lounge Voice Session stayed accessible while browsing chat at 1200 × 760, 960 × 620, and 560 × 440. Windows UI Automation and Linux AT-SPI exposed distinct Email and Password controls. |
| Search and Message Composer | Linux opened exact unloaded targets in a Text Channel and a DM Conversation, retained query and scope across the breakpoint, and returned focus after Escape. Closing Search preserved the Draft and the Message History scroll position. That check produced an identical history crop. The Composer remained reachable at 1032 × 760 with the sidebar at its maximum width. Windows retained the query while resizing and reached an unloaded target by keyboard at 560 × 440. |
| Pins, Unread, and Ping | Linux checked zero, one, and multiple Pins, keyboard and pointer actions, trigger focus on Escape, and loaded and unloaded jumps in Text Channels and DM Conversations at 1200 × 760 and 560 × 440. The populated Web fixtures exposed Unread and Ping cues, and the existing Session protocol test verified shared unread counts and Pings in the Desktop sidebar. |
| Voice Channel and Voice Session | Linux and the pinned Web peer covered empty and populated pre-join, joining and cancellation, joined controls, theater with a keyboard-reachable participant strip, Mute and Deafen. Stopping and restarting LiveKit showed reconnecting and recovery while browsing chat, with Leave voice still available. Windows with a Web peer showed populated pre-join, joined tiles at 560 × 440, theater hide and keyboard restore, and Mute, Deafen, and Leave voice from chat. |
| Sign-in and Profile | Windows showed inline required-field validation and keyboard access to account links at 560 × 440. The native Linux pass exercised an invalid link, a real Invite Link, initial nickname, password reset, and retrying saved-session restore after a fixture HTTP 503. Windows saved a nickname change, restored the fixture nickname and opened and canceled the native avatar picker. Avatar upload remains unverified with object storage disabled. |
| Settings and Appearance | Windows checked Profile and Admin Users at minimum width, including the placement of Storage above Accounts. Linux exercised regular and Admin navigation, role-dependent visibility, and opening, dismissing and restoring focus by keyboard at wide and minimum sizes. Windows saved explicit Light and Dark modes. Linux used Space to change Theme and reported native System Light → Dark → Light without restarting via private desktop-portal notifications. Focus and validation were visible in both appearances. |
| Devices and capture | Linux selected the WSL microphone. Windows exposed native microphone controls and opened and canceled the Screen Share picker. These checks covered the controls and picker only. The interactive pass did not establish physical device routing, two-way audio, microphone-driven Speaking, camera capture, or published screen video and audio. |
| Admin Channel management | On Linux at 1200 × 760 and 560 × 440, the Admin created and renamed a Channel, moved a Channel in both directions by keyboard, and canceled and confirmed deletion. The User Role did not expose management actions at either width. Settings no longer exposed a Channels section. Windows at minimum width opened New text channel from the drawer, validated a blank name by keyboard, and created a Text Channel. |

## Differences from the pinned baseline

- The pinned Desktop hid active Voice Session controls with the sidebar. The aligned Desktop keeps them available during chat and while navigation is closed.
- Desktop Search used to open in a centered dialog. It now opens beside the stage when there is room and moves into a slideover at narrow widths. The window's Composer stayed reachable in the width check above.
- The old Desktop Settings rail remained at narrow widths, and Channel management lived under Settings. The new layout puts Settings navigation above the pane at minimum width and Channel administration in the sidebar. It provides keyboard Move up and Move down actions for the workflow that Web exposes with drag-to-reorder.
- The aligned Desktop renders a compact participant list before joining and a stage with a hideable participant strip in theater. Sign-in keeps inline validation, account recovery uses pasted links, and avatar selection uses the OS picker.
- Message History still differs visually from Web in row grouping, separators, and action treatment. [Message History and actions][history]

## Platform and automated coverage

- **Windows x64** ran interactively on Windows 11. Windows Snap was measured from the actual client area.
- **Linux x64** ran the native Linux executable on Ubuntu 24.04.5 under WSL2 with Xvfb and X11. The pass used X11 keyboard events, AT-SPI and private desktop-portal appearance events. A native Wayland desktop and physical media devices remain unverified in this interactive pass.
- **macOS arm64** passed the hosted build, regular tests, and controlled LiveKit and Screen Share fixtures at `95d98e5`. No interactive macOS host was available. System appearance, 560 × 440 layout, keyboard focus, and the app's native device and capture interfaces still need an interactive run against the seeded SPK Instance. [Hosted platform build and fixtures][ci]
- The existing automated seams cover saved appearance preferences, Session account recovery, exact message targets, unread counts and Pings, and Voice Session recovery and teardown. The native window, keyboard and accessibility checks exercised the rendered application. On the integrated revision, `cargo fmt --all --check` and `cargo test --locked` passed under mise. The suite reported 116 passing tests and 13 opt-in skips. Running the existing graphical chat test separately under Xvfb confirmed that the initial jump places its target row immediately below the header. Four additional controlled-peer tests passed with the native LiveKit fixture, covering watch and reconnect behavior, video delivery, and publication of synthetic Screen Share video and a separate audio track.

[baseline]: https://github.com/ChristopheCVB/spk-gpui/blob/e403a8fa751992c1a4d5c2bc67365bd5ae7419fc/docs/research/spk-ui-alignment.md
[pr]: https://github.com/ChristopheCVB/spk-gpui/pull/113
[issue]: https://github.com/ChristopheCVB/spk-gpui/issues/112
[history]: https://github.com/ChristopheCVB/spk-gpui/issues/80
[ci]: https://github.com/ChristopheCVB/spk-gpui/actions/runs/45072140058
