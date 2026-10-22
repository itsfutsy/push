#!/usr/bin/env bash
# Builds and signs PushShare.appex, Push's Share extension, into macos/share/build/.
# Runs as Tauri's beforeBundleCommand; safe to run by hand, from any directory, any number of times.
#
#   PUSH_SIGN_IDENTITY  codesign identity (default: the Developer ID below; "-" signs ad-hoc)
#
# Entitlements (PushShare.entitlements): sandboxed, no network, no keychain. The one file exception
# is Push's inbox (~/Library/Caches/io.github.itsfutsy.push/inbox/), plus the exact path of its parent folder
# so the extension can create it if Push has never run.
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
  echo "PushShare: skipped (the Share extension is macOS only)"
  exit 0
fi

HERE="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$HERE/../.." && pwd)"
IDENTITY="${PUSH_SIGN_IDENTITY:-Developer ID Application: Abram Industries Pty ltd (4TZBGP972A)}"
CARGO_TOML="$ROOT/src-tauri/Cargo.toml"
ICON="$ROOT/src-tauri/icons/icon.icns"
OUT="$HERE/build"
STAGE="$OUT/stage"
APPEX="$STAGE/PushShare.appex"

fail() { echo "PushShare: $*" >&2; exit 1; }

# Version from the app's Cargo.toml when it has a plain x.y.z one.
VERSION=1.0.0
if [[ -f "$CARGO_TOML" ]] && v="$(sed -n '/^version = "/{s/^version = "\([^"]*\)".*/\1/p;q;}' "$CARGO_TOML")" \
  && [[ "$v" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
  VERSION="$v"
fi

[[ -f "$ICON" ]] || fail "missing $ICON"

# Build in a staging folder and swap it in at the end, so a failed run never leaves a broken appex.
rm -rf "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$APPEX/Contents/MacOS" "$APPEX/Contents/Resources"

# Plain swiftc, no Xcode project. An appex's entry point is _NSExtensionMain (Foundation).
xcrun swiftc -O -whole-module-optimization -swift-version 6 \
  -target arm64-apple-macos14.0 \
  -sdk "$(xcrun --show-sdk-path --sdk macosx)" \
  -module-name PushShare -parse-as-library -application-extension \
  -framework AppKit -framework UniformTypeIdentifiers \
  -Xlinker -application_extension -Xlinker -e -Xlinker _NSExtensionMain \
  "$HERE/ShareViewController.swift" -o "$APPEX/Contents/MacOS/PushShare" \
  || fail "swiftc failed"

PLIST="$APPEX/Contents/Info.plist"
cp "$HERE/Info.plist" "$PLIST"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$PLIST"
plutil -replace CFBundleVersion -string "$VERSION" "$PLIST"
plutil -lint -s "$PLIST" || fail "invalid Info.plist"
cp "$ICON" "$APPEX/Contents/Resources/icon.icns"

# Sign the appex itself. Tauri never signs bundle.macOS.files and signs the outer app without
# --deep, so this signature (and its sandbox entitlements) survives bundling.
SIGN=(codesign --force --options runtime --entitlements "$HERE/PushShare.entitlements" --sign "$IDENTITY")
[[ "$IDENTITY" == "-" ]] || SIGN+=(--timestamp)
xattr -cr "$APPEX"
log="$("${SIGN[@]}" "$APPEX" 2>&1)" || fail "codesign failed with identity \"$IDENTITY\": $log"
codesign --verify --strict "$APPEX" || fail "signature does not verify"

rm -rf "$OUT/PushShare.appex"
mv "$APPEX" "$OUT/PushShare.appex"

SIGNER="ad-hoc"
[[ "$IDENTITY" == "-" ]] || SIGNER="$IDENTITY"
echo "PushShare: built $OUT/PushShare.appex ($VERSION, arm64, signed: $SIGNER)"
