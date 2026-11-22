# SPK desktop

A native desktop client for the self-hosted SPK chat and voice app. Sign in with an existing account to see who's in voice, join a channel, and talk through your preferred microphone and speakers. [SPK upstream](https://github.com/meeehdi-dev/spk) · [Roadmap](docs/ROADMAP.md)

## Download a CI build

In the GitHub repository, open **Actions → Build**, select a run from `main`, and download your platform's artifact.

| Platform | Download | Launch |
| --- | --- | --- |
| macOS 15 or later, Apple Silicon | `spk-gpui-macos-arm64.zip` | Extract and open `SPK.app`. |
| Windows x64 | `spk-gpui.exe` | Open the executable. |
| Ubuntu 24.04 x64 | `spk-gpui-linux-x64.tar.gz` | Extract with `tar -xzf spk-gpui-linux-x64.tar.gz` and run `./spk-gpui`. |

If macOS blocks a download you trust, use **System Settings → Privacy & Security → Open Anyway**. [Apple launch instructions](https://support.apple.com/en-us/102445)

On Linux, use an X11 or Wayland session with Vulkan and working PulseAudio or ALSA audio.

To enable downloads for your deployment, set the `SPK_URL` HTTPS origin and `LIVEKIT_URL` WSS endpoint in **Settings → Secrets and variables → Actions → Variables**. Use credential-free URLs because the binaries embed them. The next push to `main` builds for that deployment and uploads each successful platform build. If either variable is missing, CI warns and runs its checks with example URLs. [GitHub repository variables](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-variables#creating-configuration-variables-for-a-repository)

## Message alerts

Settings → Notifications controls Direct Messages, Mentions and replies, sound, and system preview text. Mute Notifications is available from a Text Channel or DM Conversation's sidebar menu. Your operating system controls permission to show system notifications. On macOS, open `SPK.app` to use system notifications. On Linux, the desktop notification service must support actions, and Wayland compositors can limit whether an action raises or restores the window.

Quitting SPK ends desktop delivery. Browser Web Push can still deliver messages if your instance supports it and you have subscribed in the browser. A previously delivered notification can reopen SPK on macOS, on Windows, and on Linux desktops with the XDG Notification Portal version 2 or GNOME Shell with version 1. Windows and Linux register the executable for your OS user when SPK runs. If you move the executable, open it from its new location before using a retained alert. On other Linux desktops, alerts require a running SPK client and a notification service with action support.

## Build prerequisites

- **macOS 15 or later, Apple Silicon.** Install Xcode 26.2 and select its developer directory for the ScreenCaptureKit build.
- **Windows x64.** Install Visual Studio 2022 Build Tools with the **Desktop development with C++** workload and a Windows SDK. Use an x64 Developer PowerShell and the MSVC Rust toolchain. Use a short checkout path for WebRTC’s generated files.
- **Ubuntu 24.04 x64.** Install the packages below and Clang 21 from the LLVM Noble repository for the LiveKit build. To launch the app, use an X11 or Wayland session with Vulkan and working PulseAudio or ALSA audio. For session restore, run a Secret Service provider such as GNOME Keyring. [LLVM Ubuntu packages](https://apt.llvm.org/)

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libfontconfig-dev libwayland-dev \
  libxkbcommon-x11-dev libx11-xcb-dev libvulkan1 libglib2.0-dev libjpeg-turbo8-dev \
  libasound2-dev clang-21 libclang-21-dev
export CC=clang-21 CXX=clang++-21
```

Install mise, then run `mise trust` and `mise install` from the project directory. `mise exec -- rustc -vV` shows the selected compiler and host target. [mise setup](https://mise.jdx.dev/getting-started.html)

## Build and sign in

Copy `.env.example` to `.env` in the repository root and replace the example URLs with your deployment's endpoints. Git ignores `.env` and mise loads it for builds. CI can supply the same variables through its environment. When both are set, mise uses the values from `.env`.

macOS

```sh
mise exec -- cargo build --locked
sh build/macos-bundle.sh
open target/debug/SPK.app
```

The development bundle supplies the microphone permission prompt. Allow access to **SPK** when joining voice to enable your microphone.

Linux

```sh
mise exec -- cargo build --locked
./target/debug/spk-gpui
```

Windows PowerShell

```powershell
mise exec -- cargo build --locked
.\target\debug\spk-gpui.exe
```

Open **SPK** and sign in with your existing email and password. Select a channel from **Voice channels**, then select **Join voice** to listen and talk. Use the arrows next to the microphone and headphone controls, or open **Settings → Voice & Video**, to choose your devices. SPK remembers your choices after you close the app and uses **System default** while a preferred device is disconnected.

**Mute** releases your microphone. **Deafen** also silences call playback. Select **Enable microphone** if capture is unavailable, or choose another device and keep listening. Select **Leave voice** to end the call. Closing the app keeps your session in the OS credential store. Use **Settings → Profile → Sign out** to end the session and remove the saved sign-in. [Phase 4 verification](docs/PHASE-4.md)

Record mute and deafen shortcuts in **Settings → Shortcuts**. **In SPK** works while SPK has focus. **Global** also works in the background on macOS, Windows, and Linux X11 while you are signed in. Wayland uses **In SPK**. On macOS, F21–F24 are available in **In SPK** when the system delivers the key event.

Under **Settings → Voice & Video → Voice sounds**, you can adjust or silence the cues for joining, leaving, muting, and deafening. Join and leave cues include other participants in your room. Cues play through the system default media output, even while deafened.

SPK starts in Dark mode. Choose **Dark**, **Light**, or **System** in **Settings → Appearance**. SPK saves the theme for the next launch.

On Windows, **System default** uses the default communications device for input and output. Microphone access follows **Settings → Privacy & security → Microphone → Let desktop apps access your microphone**. [Windows microphone privacy settings](https://support.microsoft.com/en-us/windows/privacy/windows-camera-microphone-and-privacy)

`SPK_URL` is the SPK HTTPS origin, optionally with a port. The build derives the SPK WebSocket URL from that origin at `/_ws`. `LIVEKIT_URL` is the LiveKit WSS endpoint returned by SPK's voice token API. Debug builds also accept `http://` and `ws://` for `localhost` or a loopback IP. Release builds require HTTPS and WSS. Changing an endpoint requires rebuilding. Keep credentials out of both URLs.

Run `mise exec -- cargo fmt --all --check` and `mise exec -- cargo test --locked` with the endpoints configured. The tests use a local SPK protocol fixture and run without test accounts or a desktop credential store.
