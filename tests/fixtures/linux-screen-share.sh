#!/usr/bin/env bash
set -euo pipefail

runtime="$(mktemp -d)"
export PULSE_RUNTIME_PATH="$runtime" PULSE_SERVER="unix:$runtime/socket" PULSE_COOKIE="$runtime/cookie"
export SPK_TEST_PULSE_OUTPUT=spk_screen
cat > "$runtime/screen.pa" <<PULSE
load-module module-native-protocol-unix socket=$runtime/socket auth-anonymous=true
load-module module-null-sink sink_name=$SPK_TEST_PULSE_OUTPUT rate=48000 channels=2
load-module module-null-sink sink_name=spk_other rate=48000 channels=2
set-default-sink spk_other
set-default-source spk_other.monitor
PULSE

XDG_CONFIG_HOME="$runtime" pulseaudio -n -F "$runtime/screen.pa" --daemonize=no \
  --use-pid-file=no --exit-idle-time=-1 > "$runtime/pulse.log" 2>&1 &
pulse=$!
cleanup() {
    if [[ -n "${playback:-}" ]]; then kill "$playback" 2>/dev/null || true; fi
    kill "$pulse" 2>/dev/null || true
    wait "$pulse" 2>/dev/null || true
    rm -rf "$runtime"
}
trap cleanup EXIT
for _ in {1..40}; do
    if pactl info >/dev/null 2>&1; then break; fi
    if ! kill -0 "$pulse" 2>/dev/null; then break; fi
    sleep .1
done
if ! pactl info > /dev/null; then cat "$runtime/pulse.log" >&2; exit 1; fi

python3 - "$runtime/tone.pcm" <<'PY'
import math
import struct
import sys

second = b"".join(struct.pack("<hh", int(9000 * math.sin(i * math.tau * 503 / 48000)),
                              int(5000 * math.sin(i * math.tau * 701 / 48000)))
                  for i in range(48000))
with open(sys.argv[1], "wb") as output:
    output.write(second * 300)
PY
pacat --playback --raw --rate=48000 --channels=2 --format=s16le \
  --device="$SPK_TEST_PULSE_OUTPUT" "$runtime/tone.pcm" &
playback=$!

env -u WAYLAND_DISPLAY XDG_SESSION_TYPE=x11 xvfb-run -a -s '-screen 0 640x480x24' \
  python3 tests/fixtures/livekit-peers.py "$@"

test -z "$(pactl list source-outputs short)"
