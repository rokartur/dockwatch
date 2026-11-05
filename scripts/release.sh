#!/usr/bin/env bash
# Builds .build/release-app/Dockwatch-<version>.dmg: universal binary, Developer ID signature
# with hardened runtime, notarized and stapled. Run it on a Mac whose keychain holds the
# signing certificate and a notarytool profile (xcrun notarytool store-credentials <name>).
#
#   NOTARY_PROFILE=<name> scripts/release.sh
#   SKIP_NOTARIZE=1 scripts/release.sh     # signed local build only
set -euo pipefail
cd "$(dirname "$0")/.."

identity=${SIGN_IDENTITY:-"Developer ID Application: Artur Rok (N529W98U62)"}
version=${VERSION:-0.1.0}
out=.build/release-app
app=$out/Dockwatch.app
dmg=$out/Dockwatch-$version.dmg

# One native build per arch: the default build system and multi-arch builds stamp the
# binary with SDK 14, and macOS then skips the macOS 26 design (Liquid Glass) for it.
for arch in arm64 x86_64; do
	SDKROOT=macosx swift build -c release --build-system native \
		--triple "$arch-apple-macosx14.0" --scratch-path ".build/$arch"
done
rm -rf "$out"
mkdir -p "$out"
lipo -create .build/arm64/release/Dockwatch .build/x86_64/release/Dockwatch -output "$out/Dockwatch"
scripts/bundle.sh "$out/Dockwatch" "$app" "$version"

codesign --force --options runtime --timestamp --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"

hdiutil create -volname Dockwatch -srcfolder "$app" -ov -format UDZO "$dmg"
codesign --force --timestamp --sign "$identity" "$dmg"

if [[ -n ${SKIP_NOTARIZE:-} ]]; then
	echo "Signed, not notarized: $dmg"
	exit 0
fi
profile=${NOTARY_PROFILE:?set NOTARY_PROFILE to a notarytool keychain profile, or SKIP_NOTARIZE=1}
xcrun notarytool submit "$dmg" --keychain-profile "$profile" --wait
xcrun stapler staple "$dmg"
spctl --assess --type open --context context:primary-signature --verbose "$dmg"
echo "Release ready: $dmg"
