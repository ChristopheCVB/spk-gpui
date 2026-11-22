#!/usr/bin/env bash
set -euo pipefail

runtime="$(mktemp -d)"
cleanup() {
    if [[ -n "${source_pid:-}" ]]; then
        kill "$source_pid" 2>/dev/null || true
        wait "$source_pid" 2>/dev/null || true
    fi
    rm -rf "$runtime"
}
trap cleanup EXIT

export SPK_TEST_SCREEN_WINDOW="SPK fixture ${runtime##*/}"
xcrun swiftc -target "$(uname -m)-apple-macosx15.0" tests/fixtures/macos-screen-share.swift -o "$runtime/source"
"$runtime/source" "$SPK_TEST_SCREEN_WINDOW" "$runtime/ready" "$runtime/stop" &
source_pid=$!
for _ in {1..80}; do
    if [[ -e "$runtime/ready" ]]; then break; fi
    kill -0 "$source_pid"
    sleep .1
done
test -e "$runtime/ready"

SPK_TEST_MAC_SCREEN=display python3 tests/fixtures/livekit-peers.py "$@"
SPK_TEST_MAC_SCREEN=window SPK_TEST_MAC_SCREEN_STOP="$runtime/stop" +    python3 tests/fixtures/livekit-peers.py "$@"
