#!/bin/sh
# Renders every preview scenario in real macOS WebKit, light and dark, to /tmp/push-ui-shots/.
# usage: [PORT=5174] dev/shots.sh [scenario ...]    (default: SHOTS in dev/scenarios.js)
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

if [ $# -eq 0 ]; then
  set -- $(sed -n "/^export const SHOTS/,/];/p" dev/scenarios.js | grep -o "'[^']*'" | tr -d "'")
fi

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
