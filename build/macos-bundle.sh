#!/bin/sh
set -eu

app=target/debug/SPK.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/debug/spk-gpui "$app/Contents/MacOS/spk-gpui"
cp build/macos-info.plist "$app/Contents/Info.plist"
cp assets/logo.icns build/media-licenses.txt "$app/Contents/Resources/"
codesign --force --sign - --options runtime --entitlements build/macos-entitlements.plist "$app"
