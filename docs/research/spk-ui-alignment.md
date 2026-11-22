# SPK desktop UI alignment with Web

The desktop baseline already shares the web client's workspace sections, chat and voice concepts, Settings modal, and appearance choices. The clearest source-confirmed workflow difference is that Web keeps a voice control bar available when navigation is hidden. Desktop hides the persistent voice and Profile cards with navigation, so they disappear while viewing chat with the sidebar closed. This makes the workspace decision a prerequisite for placing the shared controls. [Web workspace] · [Web voice bar] · [Desktop workspace]

The comparison uses [SPK Web at `62ea475`][Web baseline], [SPK desktop at `379373b`][Desktop baseline], and [GPUI Kit 0.10.0][Kit lock]. The Kit links use `f697c313`, the revision recorded in the published Kit and component packages. [Kit facade]

## What matches and where the presentations differ

| Surface | Matches already in the pinned sources | Source-confirmed differences to carry into the decisions |
| --- | --- | --- |
| Workspace and navigation | The same three sections, a destination icon and title, a sidebar toggle, a Profile surface, and a search entry point. Both provide keyboard shortcuts and a Quick Switcher. | Web uses rounded navigation sections, a user-resizable desktop sidebar (224–480 CSS px, 288 default), and a drawer below 1,024 CSS px. Desktop uses a 20% sidebar bounded to 270–378 GPUI px and a binary open/closed state. The desktop window minimum is 560 × 440. Web shows a voice bar when a Voice Session is active and the sidebar is out of view. Desktop renders the current call card and Profile's audio controls inside its sidebar. [Web workspace] · [Web sidebar] · [Web voice bar] · [Desktop workspace] · [Desktop window] · [Desktop keys] · [Desktop switcher] |
| Chat header, search, and Pins | The header identifies the destination. Both support scoped message search and a Pins summary above history with jumps back to a message. Both show unread counts and Ping cues in the sidebar. | Wide Web uses a header search field that opens a right panel and switches to a slideover at narrow widths. Desktop opens a centered dialog from its search icon. Web Pins can show the latest expandable preview or an all-pins list behind a count. Desktop uses a Pins button with an expandable list. [Web workspace] · [Web Pins] · [Web search] · [Web sidebar] · [Desktop workspace] · [Desktop search] · [Desktop Pins] · [Desktop chat] |
| Message Composer | The pinned desktop already has a single rounded surface, a full-width growing editor over its tools, and a labeled Send control. It keeps reply, typing, media queue, and send status. | Use [Rework the desktop message composer] as the closed design baseline. The code already sets one to six lines of growth. Paired screenshots and keyboard reachability at minimum width still need checking for this comparison. [Web composer] · [Desktop chat] · [Desktop editor] |
| Message History and actions | Both render individual message actions, Reply previews, reactions, edited and pinned states, unread markers, and a Ping tint. | Web groups messages with date separators and a hover or focus action toolbar. Pinned desktop renders an author header for each message and an always-present Actions control. Its row background uses the same primary tint for a Ping and a jump target. Web uses separate primary and neutral treatments. Carry these differences under [Rework desktop message history and actions]. [Web history] · [Web message] · [Desktop chat] |
| Voice Channels and Voice Session | Both show presence across Voice Channels, an empty-channel state, success color for Speaking, distinct mute and deafen icons, a bottom control group in the Voice Channel, and Camera, Screen Share, and Watch controls. Both have a compact treatment for audio controls outside the stage. | Web uses compact cards before joining and switches to video tiles after joining. Web theater keeps a participant strip that can be hidden. Desktop uses its tile renderer for the selected Voice Channel and filters to the focused tile in theater. Web presents pending and connection states in the stage, the sidebar, and the relevant workspace components. Desktop uses joining, reconnecting, listen-only, and rejoin labels in its call card, stage controls, and notices. Compare those placements independently from actual media behavior. [Web voice stage] · [Web voice tiles] · [Web voice controls] · [Web connection notice] · [Web sidebar] · [Desktop workspace] · [Desktop voice controls] · [Desktop video] |
| Settings and appearance | Settings titles match, with a large desktop modal, a navigation column, and a scrollable content pane. Both offer Profile, Appearance, Voice & Video, Notifications, Shortcuts, Changelog, and role-gated Admin pages. Both expose Light, Dark, System, Comfortable, and Compact. | Web switches Settings to a top grid at narrow widths and a fullscreen modal at the smallest breakpoint. Desktop keeps a left rail capped at 36% within an 85% viewport-height dialog. Web Appearance uses radio groups. Desktop uses selected buttons and saves preferences locally, with Dark as its application default. Voice & Video maps browser device choices to native device controls. Both reduce density spacing, and the pinned desktop also changes a voice tile's width from 320 to 256 GPUI px in Compact. Treat [Complete appearance, shortcuts, and Quick Switcher] as shipped workflow context. [Web Settings] · [Web Settings registry] · [Web Appearance] · [Desktop Settings] · [Desktop device settings] · [Desktop workspace] |
| Sign-in, account links, and Profile | Both use a centered sign-in card with Email, Password, and a primary sign-in action. Both have account creation, password reset, a first-nickname step, avatar and nickname editing, and sign-out. | Web exposes signup and reset routes and uses a nickname modal inside the signed-in shell. Desktop starts account links through a paste/check form, uses a dedicated nickname screen, can retry session restore, and uses the OS file picker for the avatar. Web sign-in errors use a toast, while desktop account forms show inline messages. Web Profile also shows a self-service password change form, a feature gap outside this alignment map. [Web login] · [Web signup] · [Web password reset] · [Web nickname] · [Web Profile] · [Desktop account forms] · [Desktop Profile] |
| Admin | Users, Invites and Soundboard appear behind the Admin role in Settings. Both have create, rename, reorder, and delete workflows for channels, and permission-specific voice moderation. | Web places channel administration in the sidebar with creation buttons, drag-to-reorder, and dialogs. Desktop groups channel administration under Settings → Channels with Move up and Move down controls and a local editor. Compare the keyboard path and confirmation copy alongside the pointer path. [Web sidebar] · [Web Admin dialogs] · [Web Settings registry] · [Web Users] · [Desk channel Admin] · [Desk account Admin] · [Desktop workspace] |

## GPUI Kit 0.10.0 coverage

| Web purpose | Existing Kit support and where SPK supplies layout |
| --- | --- |
| Theme roles and contrast | Web uses amber primary, zinc neutrals, blue info, green success, yellow warning, and red error. Its light primary uses a darker amber than the dark theme. Web separates a base surface for the sidebar, header and overlays from a panel surface for the content and nested cards. The desktop theme already defines amber primary and ring, neutral text and surfaces, success, and danger. Kit also exposes separate sidebar, popover, input, border, muted, selection, and state tokens, with per-theme color, font-family, font-size and radius configuration. Web declares Geist and Geist Mono, and the pinned SPK desktop palette leaves typography at the Kit defaults. Kit’s sidebar and popover roles cover those base surfaces, while the content panel can use a consistently chosen neutral surface role. Use primary and its paired foreground for actions, and reserve the ring for visible keyboard focus. SPK’s workspace and cards still need to assign the roles to each surface. SPK already loads its theme from JSON and applies it through the registry. [Web tokens] · [Web variants] · [Desktop palette] · [Desktop window] · [Kit theme tokens] · [Kit theme config] |
| Actions, menus, and feedback | Use the existing Button, icon and Tooltip API for ghost navigation and toolbar actions, primary commits, danger actions, and loading or disabled feedback. Button can set an accessible label and tab stop. Dropdown and context menus, Popover, Dialog/AlertDialog, Tag, and Alert are available for control and state cues. Porting a web variant such as soft neutral still requires choosing the closest native treatment or styling it locally. [Desktop tool button] · [Kit button] · [Kit menus] · [Kit popover] · [Kit dialog] · [Kit tag] · [Kit alert] |
| Layout and navigation | Sidebar supports groups, header, footer, and icon or off-canvas collapse modes. Resizable panels provide the resize mechanism. SPK still owns width persistence and breakpoint policy, the independent voice bar, nested participant rows, the chat stage, and the behavior when a right search panel leaves too little room. Evaluate existing GPUI layout composition alongside these controls at minimum width. [Kit sidebar] · [Kit sidebar menu] · [Kit resizable] · [Desktop workspace] |
| Lists, forms, and fields | List can serve simple keyboard navigable results and Table/DataTable can serve administrative lists. Account and Settings controls can reuse Input, Select, RadioGroup, Switch, and Slider. Kit's textarea already supports growth as configured in the composer. SPK retains virtual history and its scroll anchor semantics, media tile composition, form status, and OS device paths. [Kit list] · [Kit table] · [Kit input] · [Kit select] · [Kit radio] · [Kit switch] · [Kit slider] · [Desktop editor] · [Desktop chat] · [Desktop voice controls] |

## Reference states and verification

Launch the pinned web checkout with its locked dependencies and `pnpm dev:ui`, then open **Settings → UI sandbox**. The panel renders the actual application with in-browser providers. Its preset, socket events, API delay, voice connection, microphone state, and audio-blocking controls can establish comparable reference states. [Web sandbox directions] · [Web sandbox controls] · [Web sandbox module] · [Web fixtures] · [Web voice fixtures]

| Reference | What to pair with desktop |
| --- | --- |
| Populated workspace | Open `/c/ui-general` at a wide viewport in Light and Dark. Compare stage title, sidebar grouping, selected row, badges in `design-notes` and the Alex DM, and how opening search or hiding navigation affects the usable width. Keep the seeded Lounge Voice Session active for the hidden-sidebar comparison. |
| Chat states | Open `/c/ui-unread` for a Ping, `/c/ui-dm` for a DM, and `/c/ui-showcase` for long content, a deleted-original Reply, a link preview and reaction wrapping. The panel can trigger typing and incoming messages. Pin a fixture message locally to exercise the Pins surface. Use API delay and the errors preset for loading and history failure. |
| Voice states | Open `/c/ui-voice` for the populated tiles and connected control group. Open `/c/ui-voice-empty` for an unoccupied channel. Switch between connecting, reconnecting, and errors to represent cancel, reconnect, unavailable microphone and audio-blocked cues. |
| Settings and Admin | The fake signed-in account has the Admin role. Settings can show Appearance and Voice & Video, Profile forms, populated Users, and empty Invites and Soundboard. The sidebar can open a channel Admin dialog. Use application source or supplied screenshots for signed-out or non-Admin states and any Admin mutation result. |

The fixture search supports plain text and scope. The fixture history returns no page continuation. Uploads, authentication changes, real voice and device I/O, and unsupported mutations are outside the sandbox. The fixtures therefore cover presentation and selected local actions. Use a real SPK Instance or a supplied reference for deployment-specific and device-dependent states. [Web sandbox directions] · [Web fixtures] · [Web sandbox auth] · [Web voice fixtures]

Chromium on Linux under WSL2 exercised the pinned sandbox at 1,440 × 900. It rendered the populated Text Channel in Light and Dark, showed the date divider and the Send label, and kept Leave voice available after hiding the sidebar. The checks reached a Ping, an Alex DM, the showcase and its local pin action, Settings Profile and Admin Users, and the Compact preference. The voice stage showed Watch and Leave voice for the populated fixture, Cancel while connecting, a reconnecting cue, Enable microphone with the errors preset, and the empty-channel message. At 560 × 440 CSS px, a separate browser session exposed the sidebar drawer, Search button, bottom voice bar, and Settings from the drawer. [Web workspace] · [Web sandbox controls] · [Web fixtures] · [Web voice fixtures]

Desktop findings are from the pinned source. Paired visual comparison, actual contrast and focus across controls, native minimum-window reachability, device behavior, and macOS, Windows, and native Linux verification are still open. The 560 × 440 browser run checks the Web breakpoint and does not establish native window behavior. [Desktop window] · [Desktop workspace]

[Web baseline]: https://github.com/meeehdi-dev/spk/tree/62ea475664831ac016a972555b4a1cedc6e9a353
[Desktop baseline]: https://github.com/ChristopheCVB/spk-gpui/tree/379373bb7c57f6872c2cf8ac715938d14712479c
[Web workspace]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/layouts/default.vue
[Web sidebar]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/AppSidebar.vue
[Web voice bar]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/VoiceBar.vue
[Web Pins]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/chat/PinnedBar.vue
[Web composer]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/chat/MessageInput.vue
[Web history]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/chat/ChannelView.vue#L447-L561
[Web message]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/chat/ChatMessage.vue
[Web voice stage]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/VoiceChannelView.vue
[Web voice tiles]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/VoiceTile.vue
[Web voice controls]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/VoiceControl.vue
[Web connection notice]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/ConnectionNotice.vue
[Web Settings]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/SettingsModal.vue
[Web Settings registry]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/shared/utils/settings.ts
[Web Appearance]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/settings/Appearance.vue
[Web login]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/pages/login.vue
[Web signup]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/pages/signup.vue
[Web password reset]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/pages/reset-password.vue
[Web nickname]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/NicknameModal.vue
[Web Profile]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/settings/Profile.vue
[Web Admin dialogs]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/AdminDialogs.vue
[Web Users]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/settings/Users.vue
[Web tokens]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/assets/css/main.css
[Web variants]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/app.config.ts
[Web sandbox directions]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/README.md#L48-L54
[Web sandbox controls]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/dev/ui/SandboxPanel.vue
[Web sandbox module]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/dev/ui/module.ts
[Web fixtures]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/dev/ui/fixtures.ts
[Web voice fixtures]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/dev/ui/voice.ts
[Web sandbox auth]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/dev/ui/auth.ts
[Web search]: https://github.com/meeehdi-dev/spk/blob/62ea475664831ac016a972555b4a1cedc6e9a353/app/components/SearchPanel.vue
[Desktop workspace]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/workspace.rs
[Desktop window]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/main.rs#L36-L52
[Desktop keys]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/workspace_keys.rs
[Desktop switcher]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/switcher.rs
[Desktop chat]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/chat.rs
[Desktop search]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/search.rs
[Desktop Pins]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/chat/actions.rs#L346-L481
[Desktop editor]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui.rs#L104-L118
[Desktop voice controls]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/voice.rs
[Desktop video]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/video.rs
[Desktop Settings]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/settings.rs
[Desktop account forms]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/accounts.rs
[Desktop Profile]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/settings/profile.rs
[Desk channel Admin]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/settings/channels.rs
[Desk account Admin]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/settings/account_admin.rs
[Desktop palette]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/assets/themes.json
[Desktop tool button]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui.rs#L629-L642
[Kit lock]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/Cargo.lock#L2766-L2769
[Desktop device settings]: https://github.com/ChristopheCVB/spk-gpui/blob/379373bb7c57f6872c2cf8ac715938d14712479c/src/ui/settings/audio.rs
[Kit facade]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/kit/src/lib.rs
[Kit theme tokens]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/theme/theme_color.rs
[Kit theme config]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/theme/schema.rs
[Kit button]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/button/button.rs
[Kit menus]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/menu/mod.rs
[Kit popover]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/popover.rs
[Kit dialog]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/dialog/mod.rs
[Kit tag]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/tag.rs
[Kit alert]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/alert.rs
[Kit sidebar]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/sidebar/mod.rs
[Kit sidebar menu]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/sidebar/menu.rs
[Kit resizable]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/resizable.rs
[Kit list]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/list/mod.rs
[Kit table]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/table/mod.rs
[Kit input]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/input/mod.rs
[Kit select]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/select.rs
[Kit radio]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/radio.rs
[Kit switch]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/switch.rs
[Kit slider]: https://github.com/longbridge/gpui-kit/blob/f697c3133d55c64fa8a5b67926147fb3dadd276d/crates/component/src/slider.rs
[Rework the desktop message composer]: https://github.com/ChristopheCVB/spk-gpui/issues/79
[Rework desktop message history and actions]: https://github.com/ChristopheCVB/spk-gpui/issues/80
[Complete appearance, shortcuts, and Quick Switcher]: https://github.com/ChristopheCVB/spk-gpui/issues/56
