# Phase 1 verification

## Production SPK

Use the production SPK deployment with two designated existing accounts and two existing voice rooms. The deployment operator confirms that both accounts can access both rooms and records the deployed SPK revision or image tag and the configured public LiveKit URL. [Production SPK](https://spk.meeehdi.dev/)

During the agreed test window, sign in to SPK in a browser with one designated account and join one designated voice room. In the browser’s network inspector, check that the `url` returned by `POST /api/voice/token` matches the configured `LIVEKIT_URL` and that the LiveKit connection succeeds. Leave the room after the check. Record the endpoint, revision, and outcome here. Keep credentials and tokens out of the record.

| Check | Result |
| --- | --- |
| Public SPK page | HTTP 200 |
| Deployed SPK revision or image tag | Pending operator confirmation |
| Public LiveKit URL | The public page advertises `wss://rtc.meeehdi.dev`. The browser join still needs to confirm the token response. |
| Both accounts can access both rooms | Pending operator confirmation |
| Browser join and endpoint match | Pending test window |

The second account will be the web counterpart for desktop audio and presence testing in the later voice phases. [SPK voice token endpoint](https://github.com/meeehdi-dev/spk/blob/main/server/api/voice/token.post.ts)

## Desktop builds and launches

The GitHub Actions matrix defines the format check, tests, and locked build on all three targets using example endpoints. Run the desktop smoke test from a fresh checkout on each OS. [Build and launch instructions](../README.md#build-and-sign-in)

| Target | OS and architecture tested | Build | Window launch |
| --- | --- | --- | --- |
| macOS | Apple Silicon | Pending | Pending |
| Windows | Windows 11 build 22631, x64 MSVC | Passed with CMake absent from `PATH` | Passed with production endpoints and both build variables unset at launch. Closed with exit code 0. |
| Linux | Ubuntu 24.04.5 x64 under WSL2 | Passed | Native Ubuntu desktop verification pending |

## Build error checks

On Ubuntu under WSL2, the missing and invalid inputs below produced the expected diagnostics. The insecure loopback pair built in debug, and release rejected each insecure URL. All three endpoint tests passed on Windows and Ubuntu.

Use a checkout without a local `.env` for these checks. Export the values from [`.env.example`](../.env.example) in your shell, then run the following commands on Linux or macOS. Each command should fail and identify the expected variable.

```sh
env -u SPK_URL mise exec -- cargo check --locked
env -u LIVEKIT_URL mise exec -- cargo check --locked
env SPK_URL=https://spk.example/subpath mise exec -- cargo check --locked
env LIVEKIT_URL=https://voice.example mise exec -- cargo check --locked
env SPK_URL=http://localhost:3000 mise exec -- cargo check --locked --release
env LIVEKIT_URL=ws://127.0.0.1:7880 mise exec -- cargo check --locked --release
```

Then verify that the loopback pair builds in debug mode.

```sh
env SPK_URL=http://localhost:3000 LIVEKIT_URL=ws://127.0.0.1:7880 \
  mise exec -- cargo build --locked
```
