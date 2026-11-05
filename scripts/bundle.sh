#!/usr/bin/env bash
# Wraps a built Dockwatch binary into an app bundle.
#
#   scripts/bundle.sh <binary> <output .app> [version]
set -euo pipefail
cd "$(dirname "$0")/.."

binary=$1
app=$2
version=${3:-0.0.0}

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/Dockwatch"
# Assets.car carries the Liquid Glass, dark and tinted icon for macOS 26+; Dockwatch.icns is the macOS 15 fallback.
xcrun actool packaging/Dockwatch.icon --compile "$app/Contents/Resources" --app-icon Dockwatch \
    --platform macosx --minimum-deployment-target 15.0 \
    --output-partial-info-plist "$(mktemp -d)/partial.plist" --errors >/dev/null
sed "s/__VERSION__/$version/g" packaging/Info.plist >"$app/Contents/Info.plist"
