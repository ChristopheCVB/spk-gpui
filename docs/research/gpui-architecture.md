# GPUI architecture for the SPK desktop client

SPK already has two useful interfaces: the window sends session commands and reads snapshots, and the session sends voice intent and reads media reports. Keep those interfaces and deepen the modules behind them in the existing package. Extract a crate when an independent caller or a measured build or verification need makes it valuable to enforce the dependency direction in Cargo. This is a recommendation for the refactor decision based on source inspection. [SPK session command and view types][spk-session-types] · [SPK voice interface][spk-call]

## What the other applications do

| Application | State and rendering | Background work and platform integration |
| --- | --- | --- |
| **Zed** | The `Workspace` entity owns panes, docks, and a `Project` entity. `Project` owns separate worktree, buffer, and language-server state. `Workspace` and `Editor` implement `Render`. GPUI's ownership guide documents foreground entity access, `notify`/observe, and typed events. [Cargo workspace][zed-cargo] · [Workspace state][zed-workspace] · [Project state][zed-project] · [Workspace rendering][zed-render] · [Editor rendering][zed-editor] · [GPUI ownership guide][zed-guide] | The worktree scanner runs on the background executor. A foreground task applies scan results to the worktree entity, which retains both task handles. Zed's injected filesystem has real and fake implementations. GPUI chooses separate native backends at compile time. [Worktree scan pipeline][zed-scan] · [Filesystem adapters][zed-fs] · [Test filesystem][zed-fake-fs] · [GPUI platform selection][zed-platform] |
| **Pi GPUI** | One Rust package. `HubView` owns navigation, open sessions and child views. `ChatView` owns presentation state for a session and renders the chat. The installed pi process owns the agent loop and durable session data. [Manifest][pi-cargo] · [App principles][pi-principles] · [Hub state][pi-hub] · [Chat state][pi-chat] · [Chat rendering][pi-render] | The chat sends a prompt through its process interface. Reader/writer threads move JSONL across bounded channels, then a weak GPUI entity update applies a bounded batch in order and notifies the chat. The app keeps native actions such as the macOS folder picker in its platform module. Its desktop documentation covers macOS and Linux. [Prompt and batch handling][pi-batch] · [Event pump][pi-pump] · [Process I/O][pi-jsonl] · [Platform adapters][pi-platform] · [Supported platforms][pi-readme] |
| **Pulse** | Two workspace packages. GPUI surfaces observe an `AppStore` that owns one playback adapter and exposes playback and device snapshots. The separate `pulse-engine` manifest has Core Audio, decoding and buffering dependencies. The engine is usable without the screen through its controller and probe. [Cargo workspace][pulse-cargo] · [App architecture][pulse-app-doc] · [Device surface][pulse-devices] · [Engine manifest][pulse-engine-cargo] · [Engine controller][pulse-controller] · [Diagnostic probe][pulse-probe] | The store's foreground task drains controller events and checks active-device presence, then notifies observers when relevant state changes. A worker decodes and feeds a ring buffer for the native realtime output callback. The thread model documents the engine's UI-independent dependency rule. Pulse's output is built for macOS Core Audio. [Store task][pulse-store-task] · [Event application][pulse-store-events] · [Thread model][pulse-thread] · [Tech stack][pulse-stack] |

Zed shows how a view can compose child state and how worker output can update an entity with a specific owner. The crate graph also serves local and remote projects, collaboration, filesystem adapters, and editing. Pi GPUI shows that stateful views, worker-owned I/O, and OS-specific integrations can live in one package. Pulse demonstrates the value of a crate when an engine has its own consumer, dependency set, tests, and realtime constraints. SPK can use the ownership and communication patterns with gpui-kit and its existing Tokio runtime. Pi's subprocess transport, Zed's pane framework, and Pulse's periodic event drain each solve requirements particular to those products. [Zed project state][zed-project] · [Pi transport and ownership][pi-principles] · [Pulse thread model][pulse-thread] · [SPK worker wiring][spk-wiring]

## Where those patterns apply in SPK

### Sign-in, session, and profile assets

`Spk::submit` validates input, clears the password field and sends `Command::SignIn`. The session worker owns restoration, sign-in and sign-out, including saved session persistence and the authenticated client. `Spk::new` starts a dedicated Tokio thread with bounded command and update channels and arranges shutdown. Its foreground receiver applies the next `View`, updates focus and shortcut registration, closes dialogs when signed out, and notifies GPUI. `Spk::render` chooses the sign-in or workspace surface. Keep the form, window lifecycle and view application on the GPUI side, and session credentials, network and teardown on the worker side. The existing source already provides most of this division. [Sign-in action and rendering][spk-login] · [Window and worker wiring][spk-wiring] · [Saved session][spk-saved] · [Session lifecycle][spk-session-run]

Avatar loading is the concrete exception to a UI-independent session snapshot. The worker authenticates the fetch, limits concurrent loads, decodes off the runtime's main thread, and checks the current avatar key when a result arrives. The snapshot contains GPUI `RenderImage` values, and the window releases replaced images when it applies a snapshot. If the session interface needs to become GPUI-free, separate validated image data and the user/key check from GPUI image construction and cache lifetime. Preserve the existing bounds, stale-response rejection, and asynchronous decode. The same-package refactor can leave that conversion where it is until it has a demonstrated consumer or dependency benefit. [Avatar loading and staleness check][spk-avatars] · [Rendering image lifetime][spk-image-drop] · [Avatar regression fixture][spk-avatar-tests]

### Joining, speaking, and leaving

Selecting a voice channel is window-local. Joining sends `JoinVoice`, and leaving sends `LeaveVoice`. `watch_session` serializes command handling, socket events, voice reports, and resource refreshes. The session owns voice channel switching and replacement policy. `Call` owns its LiveKit worker and exposes intent, acknowledgment, update and stop operations. Before connecting to LiveKit, the worker requests an authenticated token and awaits a matching SPK voice join acknowledgment. Cleanup stops audio, sends an SPK leave when appropriate, and closes media. [Voice UI actions][spk-join] · [Leave action][spk-leave] · [Session event loop][spk-session-loop] · [Call interface and worker order][spk-call] · [Voice claim and cleanup][spk-claim]

Speaking has two real inputs. SPK WebSocket presence updates the session's participant state and the channel sidebar reads those flags. The LiveKit worker converts active-speaker events into media reports and the stage uses those reports when available, falling back to SPK flags and masking speaking for mute or deafen. Follow Zed's example of keeping worker and presentation state with their specific owners. Keep the source precedence and recovery order explicit wherever the rendering rules move, especially a fresh presence snapshot before reclaim and stale updates during leave. The current fixtures cover acknowledgment, reclaim/replacement, switch/move behavior and speaking. [Socket and media event handling][spk-events] · [LiveKit reports][spk-reports] · [Sidebar speaking][spk-sidebar-speaking] · [Stage speaking][spk-stage] · [Reclaim sequencing][spk-reclaim] · [Session fixtures][spk-session-tests] · [Voice fixtures][spk-voice-tests]

### Device settings, sound, and shortcuts

The call controls and Settings share the device picker. A choice sends `ChooseDevice`. The session updates preferred device intent, triggers a microphone or speaker retry, applies it to a live call and persists the preference. The worker enumerates and routes audio, then reports the active devices. The picker displays the active route alongside the preference and fallback. Keep one owner for each of preference, active route, and picker state. Keep LiveKit `PlatformAudio`, macOS microphone permission, and Windows communications-device handling behind voice modules selected by the existing target configuration. Pulse supports a small device-facing interface for multiple surfaces, but its Core Audio implementation provides evidence for macOS only. [Settings audio surface][spk-settings-audio] · [Picker and status][spk-picker] · [Intent and persistence][spk-device-command] · [Device rules][spk-device-rules] · [Speaker routing][spk-route] · [Microphone permission][spk-mic] · [Windows default device][spk-windows] · [Target dependencies][spk-cargo] · [Pulse device state][pulse-app-doc]

On-screen buttons and focused and global shortcuts converge on `voice_action`. The window owns focus and recording behavior, and the shortcut module owns bindings, platform registration and suppression of stale global events. Settings owns the dialog's tabs, sliders and focus, observes the root entity, and changes sound preferences through the window. Session events trigger cue playback through a watch-fed sound worker. Borrow Pi GPUI's placement of OS calls near their adapter. A focused action interface and a read-only view of relevant settings would narrow the dialog's dependence on the root without making it a second owner of voice intent or preferences. Preserve shortcut recording and release, sign-in gating, Wayland's focused scope, and cue order. [Window shortcuts and sound preferences][spk-window-actions] · [Voice control dispatch][spk-voice-actions] · [Shortcut registration and dispatch][spk-shortcuts] · [Settings entity][spk-settings] · [Voice sounds][spk-sounds] · [Desktop behavior documentation][spk-readme]

## Smallest viable structure and crate threshold

- **Keep a single Cargo package and give each area an explicit owner.** Keep `main.rs` focused on bootstrapping and let window state and gpui-kit surfaces live on the UI side. Keep `session` responsible for credentials, SPK WebSocket state and Voice Session decisions, `voice` responsible for LiveKit and devices, and `api` responsible for protocol and transport. Keep shortcut registration and sound workers behind their existing interfaces. Within these modules, narrow visibility and group behavior by owner. Start with existing commands, snapshots, intent and reports so callers and tests continue to use the same interfaces. The trade-off is that Cargo still builds one package and does not enforce the internal import direction. [SPK manifest][spk-cargo] · [Worker interface][spk-wiring] · [Voice intent and reports][spk-call]
- **Consider a crate when there is evidence it earns the extra interface.** An actual headless caller or second binary reusing voice or SPK transport would demonstrate reuse. A demonstrated need to run protocol or voice tests without gpui-kit, a measured build problem attributable to the dependency graph, or a repeated dependency violation despite module privacy would demonstrate enforcement value. Confirm first that the candidate builds and tests with GPUI absent from `cargo tree -p <candidate>`, can be driven through a small interface by that caller, and leaves one owner for session and LiveKit state. A voice crate that imports the UI package to access `SpkClient`, `VoiceSocket`, or build-time endpoints would force additional moves or a broader public interface, since the current worker coordinates both SPK presence and LiveKit. The current fixtures already exercise the protocol without a GPUI event loop, which reduces the case for a new package solely to write tests. Pulse's diagnostic probe and engine tests are concrete examples of the independent verification a crate can enable. [SPK Call setup][spk-call] · [SPK token and join handling][spk-claim] · [SPK test workflow][spk-readme] · [Pulse probe][pulse-probe] · [Pulse engine verification][pulse-engine-tests]

For the behavior-preserving plan, run the existing locked tests and platform CI, then verify against the same SPK deployment with a web participant on macOS, Windows and Linux. Exercise sign-in/restore/sign-out, join/switch/leave and reconnect/replacement, two-way audio, system-default and preferred devices, speaking, mute/deafen, and shortcut scopes. Check avatar replacement and Settings alongside those flows if view or cache ownership moves. Source paths are pinned below. These are proposed verification gates. This research pass did not run the applications, benchmark builds or validate live audio. [SPK CI][spk-ci] · [SPK milestone][spk-milestone] · [SPK desktop behavior][spk-readme]

[zed-cargo]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/Cargo.toml#L1-L278
[zed-workspace]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/workspace/src/workspace.rs#L1653-L1698
[zed-project]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/project/src/project.rs#L212-L249
[zed-render]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/workspace/src/workspace.rs#L10091-L10210
[zed-editor]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/editor/src/editor.rs#L12792-L12815
[zed-guide]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/gpui/src/_ownership_and_data_flow.rs#L1-L105
[zed-scan]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/worktree/src/worktree.rs#L1368-L1458
[zed-fs]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/fs/src/fs.rs#L104-L154
[zed-fake-fs]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/fs/src/fs.rs#L1558-L1580
[zed-platform]: https://github.com/zed-industries/zed/blob/2f808b3c2de049ecfdbad0fdb26763cebfcaa3b4/crates/gpui_platform/src/gpui_platform.rs#L61-L82
[pi-cargo]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/Cargo.toml
[pi-principles]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/docs/APP_PRINCIPLES.md
[pi-hub]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/src/hub.rs#L292-L350
[pi-chat]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/src/chat.rs#L364-L440
[pi-render]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/src/chat.rs#L3156-L3221
[pi-batch]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/src/chat.rs#L749-L859
[pi-pump]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/src/chat.rs#L3284-L3304
[pi-jsonl]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/src/jsonl.rs#L29-L114
[pi-platform]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/src/platform.rs#L1-L88
[pi-readme]: https://github.com/bytesbay/pi-gpui/blob/a8d40e30c4ea6f7091e5e0ee8e819e2c3bbe6da2/README.md#L1-L35
[pulse-cargo]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/Cargo.toml#L1-L11
[pulse-app-doc]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/docs/arch/pulse-app.md
[pulse-devices]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/crates/pulse-app/src/surfaces/devices.rs#L26-L85
[pulse-engine-cargo]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/crates/pulse-engine/Cargo.toml
[pulse-controller]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/crates/pulse-engine/src/controller/mod.rs#L27-L115
[pulse-probe]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/crates/pulse-engine/examples/integer_probe.rs
[pulse-store-task]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/crates/pulse-app/src/app_store.rs#L96-L135
[pulse-store-events]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/crates/pulse-app/src/app_store.rs#L388-L423
[pulse-thread]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/docs/arch/thread-model.md
[pulse-stack]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/docs/arch/tech-stack.md
[pulse-engine-tests]: https://github.com/yicheng47/pulse/blob/f7d1ee994cc63c7630918069e9d19b9432b5014e/docs/arch/pulse-engine.md#L227-L233
[spk-session-types]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session.rs#L38-L96
[spk-call]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/voice.rs#L97-L279
[spk-wiring]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/main.rs#L77-L201
[spk-login]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/main.rs#L316-L449
[spk-saved]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session.rs#L328-L405
[spk-session-run]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session.rs#L416-L567
[spk-avatars]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session/avatars.rs#L25-L110
[spk-image-drop]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/main.rs#L221-L236
[spk-avatar-tests]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session/tests.rs#L811-L930
[spk-join]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/ui.rs#L571-L580
[spk-leave]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/ui.rs#L722-L745
[spk-session-loop]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session.rs#L728-L859
[spk-claim]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/voice.rs#L332-L421
[spk-events]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session.rs#L860-L949
[spk-reports]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/voice.rs#L548-L618
[spk-sidebar-speaking]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/ui.rs#L545-L568
[spk-stage]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/ui.rs#L748-L790
[spk-reclaim]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session.rs#L956-L999
[spk-session-tests]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session/tests.rs
[spk-voice-tests]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/voice/tests.rs
[spk-settings-audio]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/ui/settings.rs#L404-L424
[spk-picker]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/ui/voice.rs#L175-L302
[spk-device-command]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/session.rs#L817-L855
[spk-device-rules]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/voice/devices.rs#L25-L122
[spk-route]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/voice.rs#L721-L745
[spk-mic]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/voice.rs#L978-L1018
[spk-windows]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/voice/windows_audio.rs
[spk-cargo]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/Cargo.toml
[spk-window-actions]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/main.rs#L77-L127
[spk-voice-actions]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/ui/voice.rs#L30-L120
[spk-shortcuts]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/shortcuts.rs#L351-L686
[spk-settings]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/ui/settings.rs#L90-L151
[spk-sounds]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/src/sounds.rs#L68-L127
[spk-readme]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/README.md#L67-L79
[spk-ci]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/.github/workflows/build.yml
[spk-milestone]: https://github.com/ChristopheCVB/spk-gpui/blob/3b080cfa0c8ecccdc398a09c1694b20a016a7150/AGENTS.md#L21-L27
