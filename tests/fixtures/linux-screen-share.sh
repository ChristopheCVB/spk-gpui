#!/usr/bin/env bash
set -euo pipefail

source="${1:-}"
case "$source" in
    --wayland-display|--wayland-window)
        shift
        ;;
    *) source="" ;;
esac

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
pipewire_pids=()
cleanup() {
    if [[ -n "${playback:-}" ]]; then kill "$playback" 2>/dev/null || true; fi
    kill "$pulse" "${pipewire_pids[@]}" 2>/dev/null || true
    wait "$pulse" "${pipewire_pids[@]}" 2>/dev/null || true
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

if [[ -n "$source" ]]; then
    export SPK_TEST_WAYLAND_SOURCE="${source#--wayland-}"
    export SPK_TEST_WAYLAND_SCREEN_STOP="$runtime/stop"
    export PIPEWIRE_RUNTIME_DIR="$runtime" PIPEWIRE_REMOTE=pipewire-0
    # Only synthetic video should be visible on the private PipeWire graph.
    cat > "$runtime/pipewire.conf" <<'PIPEWIRE'
context.properties = { core.daemon = true core.name = pipewire-0 }
context.spa-libs = { support.* = support/libspa-support }
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-access }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-link-factory }
    { name = libpipewire-module-metadata }
]
PIPEWIRE
    "${CC:-cc}" tests/fixtures/wayland-pipewire-source.c \
      $(pkg-config --cflags --libs libpipewire-0.3) -o "$runtime/source"
    pipewire -c "$runtime/pipewire.conf" > "$runtime/pipewire.log" 2>&1 &
    pipewire_pids+=("$!")
    for _ in {1..40}; do
        if [[ -S "$runtime/pipewire-0" ]]; then break; fi
        sleep .1
    done
    if [[ ! -S "$runtime/pipewire-0" ]]; then cat "$runtime/pipewire.log" >&2; exit 1; fi
    "$runtime/source" > "$runtime/source.log" 2>&1 &
    pipewire_pids+=("$!")
    export SPK_TEST_PIPEWIRE_SOURCE_PID="$!"
    for _ in {1..40}; do
        SPK_TEST_PIPEWIRE_NODE="$(pw-dump | python3 -c '
import json
import sys
print(next((obj["id"] for obj in json.load(sys.stdin)
            if obj["type"] == "PipeWire:Interface:Node"
            and obj["info"]["props"].get("node.name") == "spk-fixture"), ""))')"
        if [[ -n "$SPK_TEST_PIPEWIRE_NODE" ]]; then break; fi
        sleep .1
    done
    export SPK_TEST_PIPEWIRE_NODE
    if [[ -z "$SPK_TEST_PIPEWIRE_NODE" ]] || ! /usr/bin/python3 tests/fixtures/wayland-screen-portal.py \
        python3 tests/fixtures/livekit-peers.py "$@"; then
        cat "$runtime/pipewire.log" "$runtime/source.log" >&2
        exit 1
    fi
else
    # Keep the X server stable when one test closes its last client before the next begins.
    env -u WAYLAND_DISPLAY XDG_SESSION_TYPE=x11 xvfb-run -a -s '-noreset -screen 0 640x480x24' \
      python3 tests/fixtures/livekit-peers.py "$@"
fi

test -z "$(pactl list source-outputs short)"
