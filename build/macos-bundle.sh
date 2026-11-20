#!/bin/sh
set -eu

app=target/debug/SPK.app
mkdir -p "$app/Contents/MacOS"
cp target/debug/spk-gpui "$app/Contents/MacOS/spk-gpui"
cp build/macos-info.plist "$app/Contents/Info.plist"
codesign --force --sign - --options runtime --entitlements build/macos-entitlements.plist "$app"
