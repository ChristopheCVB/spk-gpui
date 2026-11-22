#!/bin/sh
set -eu

build_dir=${1:-target/debug}
app="$build_dir/SPK.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$build_dir/spk-gpui" "$app/Contents/MacOS/spk-gpui"
cp build/macos-info.plist "$app/Contents/Info.plist"
cp assets/logo.icns build/media-licenses.txt "$app/Contents/Resources/"
codesign --force --sign - --options runtime --entitlements build/macos-entitlements.plist "$app"
