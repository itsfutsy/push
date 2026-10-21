#!/usr/bin/env bash
# Builds the signed Push.app, with its Share extension inside.
#
#   scripts/build-macos.sh             build and verify
#   scripts/build-macos.sh --install   then replace /Applications/Push.app and enable the Share extension
#
#   PUSH_SIGN_IDENTITY  identity for the app and the extension (default: the Developer ID below if it is in
#                       your keychain, else ad-hoc; "-" = ad-hoc)
#   To notarize, also set APPLE_ID, APPLE_PASSWORD and APPLE_TEAM_ID (or APPLE_API_KEY, APPLE_API_ISSUER
#   and APPLE_API_KEY_PATH). Locally built apps are not quarantined, so this only matters for other Macs.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
IDENTITY="${PUSH_SIGN_IDENTITY:-Developer ID Application: Abram Industries Pty ltd (4TZBGP972A)}"
APP="${CARGO_TARGET_DIR:-$ROOT/src-tauri/target}/release/bundle/macos/Push.app"
APPEX="$APP/Contents/PlugIns/PushShare.appex"
DEST=/Applications/Push.app
EXT_ID=io.github.itsfutsy.push.share
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '4,5p' "$0" | sed 's/^# *//'; }

INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --install) INSTALL=1 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $arg" ;;
  esac
done

# Prerequisites
[[ "$(uname -s)" == Darwin ]] || die "this builds the macOS app (on Linux: cargo tauri build --bundles deb,appimage)"
cargo tauri --version >/dev/null 2>&1 || die "cargo-tauri not found (cargo install tauri-cli --version '^2.12' --locked)"
xcrun --find swiftc >/dev/null 2>&1 || die "Xcode or its command line tools not found (xcode-select --install)"
if [[ "$IDENTITY" != "-" ]]; then
  identities="$(security find-identity -v -p codesigning)"
  if ! grep -qF -- "$IDENTITY" <<<"$identities"; then
    [[ -z "${PUSH_SIGN_IDENTITY:-}" ]] \
      || die "signing identity not in the keychain: $IDENTITY (set PUSH_SIGN_IDENTITY, or - for ad-hoc)"
    echo "No Developer ID certificate found, so Push.app will be signed ad-hoc (fine for your own Mac)."
    IDENTITY=-
  fi
fi

# Build. Tauri's beforeBundleCommand (macos/share/build.sh) builds and signs the appex first;
# both read the same identity so the app and its extension always match.
export PUSH_SIGN_IDENTITY="$IDENTITY" APPLE_SIGNING_IDENTITY="$IDENTITY"
say "Building Push.app"
(cd "$ROOT/src-tauri" && cargo tauri build --bundles app)

say "Verifying"
[[ -d "$APP" ]] || die "no app at $APP"
[[ -d "$APPEX" ]] || die "the Share extension is missing: $APPEX"
codesign --verify --deep --strict "$APP" || die "codesign verification failed"
entitlements="$(codesign -d --entitlements - --xml "$APPEX" 2>/dev/null | plutil -p - 2>/dev/null || true)"
[[ "$entitlements" == *'"com.apple.security.app-sandbox" => true'* ]] \
  || die "the Share extension is not signed with its sandbox entitlement"
info="$(codesign -dvv "$APP" 2>&1)"
signer="$(awk -F= '/^Authority=/ { print $2; exit }' <<<"$info")"
echo "Signature: ${signer:-ad-hoc}. Share extension embedded and sandboxed."
echo "$APP"

if [[ "$INSTALL" == 0 ]]; then
  cat <<EOF

Next: install it with scripts/build-macos.sh --install
(or copy Push.app to /Applications, open it once, and turn on Push under System Settings >
General > Login Items & Extensions > Extensions > Sharing)
EOF
  exit 0
fi

say "Installing to $DEST"
running() { pgrep -qf '/Push\.app/Contents/MacOS/push( |$)'; }
if running; then
  echo "Quitting Push"
  osascript -e 'quit app id "io.github.itsfutsy.push"' >/dev/null 2>&1 || true
  for _ in {1..50}; do running || break; sleep 0.1; done
  if running; then pkill -f '/Push\.app/Contents/MacOS/push( |$)' || true; sleep 1; fi
fi
rm -rf "$DEST"
ditto "$APP" "$DEST"
# Only the installed copy should be registered, or the Share menu can pick up the build folder's.
"$LSREGISTER" -u "$APP" >/dev/null 2>&1 || true
"$LSREGISTER" -f "$DEST"

say "Launching Push"
open "$DEST"
found() { [[ "$(pluginkit -m -i "$EXT_ID" 2>/dev/null)" == *"$EXT_ID"* ]]; }
for _ in {1..20}; do found && break; sleep 0.5; done
found || pluginkit -a "$DEST/Contents/PlugIns/PushShare.appex"
pluginkit -e use -i "$EXT_ID" || true

# pluginkit marks an enabled extension with a leading "+".
if [[ "$(pluginkit -m -i "$EXT_ID" 2>/dev/null)" =~ ^[[:space:]]*\+ ]]; then
  echo "Share extension enabled. In Finder, select a file and choose Share > Push."
else
  cat <<EOF
Turn on the Share extension: System Settings > General > Login Items & Extensions > Extensions >
Sharing > Push (macOS 14: Privacy & Security > Extensions > Sharing). In Finder, Share > Edit
Extensions… gets you there too.
EOF
fi
