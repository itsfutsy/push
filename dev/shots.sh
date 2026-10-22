#!/bin/sh
# Renders every preview scenario in real macOS WebKit, light and dark, to /tmp/push-ui-shots/.
# usage: [PORT=5174] dev/shots.sh [scenario ...]    (default: the list below)
set -eu
cd "$(dirname "$0")/.."
OUT=/tmp/push-ui-shots
PORT=${PORT:-5174}
SNAP=/tmp/push-snap
mkdir -p "$OUT"

[ dev/snap.swift -nt "$SNAP" ] && swiftc -O dev/snap.swift -o "$SNAP"

if ! curl -sf "http://127.0.0.1:$PORT/dev/index.html" >/dev/null; then
  python3 dev/serve.py "$PORT" >/dev/null 2>&1 &
  SERVER=$!
  trap 'kill $SERVER' EXIT
  sleep 0.5
fi

[ $# -gt 0 ] || set -- list list-hover focus empty copied confirm uploading uploading-hover success dragover error \
  error-auth noToken firstRun noShortcut settings settings-focus settings-error recording recording-held shortcut-error \
  linux wayland library library-hover library-preview library-video library-audio library-confirm \
  library-select library-select-confirm library-images library-search library-empty library-linux \
  hud-uploading hud-file hud-done hud-failed hud-auth hud-permission hud-hover

status=0
for s in "$@"; do
  case $s in
    hud-*) size="380 144" ;;
    library*) size="960 700" ;;
    linux*|wayland*) size="460 638" ;;
    *) size="460 600" ;;
  esac
  for mode in light dark; do
    log=$("$SNAP" "http://127.0.0.1:$PORT/dev/?s=$s&shot" "$OUT/$s-$mode.png" $size $mode 2 2>&1) || status=1
    echo "$log" | grep -v '^wrote' | sed "s/^/$s-$mode: /" || true
  done
done
echo "screenshots in $OUT"
exit $status
