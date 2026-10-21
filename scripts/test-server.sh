#!/usr/bin/env bash
# End-to-end test of a running Push server. Uploads a few test files and deletes them again.
#
#   TOKEN=<upload token> scripts/test-server.sh                         # local, http://localhost:3900
#   BASE=https://share.example.com TOKEN=<upload token> scripts/test-server.sh  # production
#
# Exits non-zero with a FAIL line on the first failed check.
set -euo pipefail

BASE=${BASE:-http://localhost:3900}
BASE=${BASE%/}
: "${TOKEN:?set TOKEN to the PUSH_TOKEN of the server}"

CHUNK=$((8 * 1024 * 1024))
BIG=20000000
ROBOTS_TAG="noindex, nofollow, noarchive, nosnippet, noimageindex"
CSP="default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; sandbox"

tmp=$(mktemp -d)
created=()
cleanup() {
  # Best effort: remove whatever this run uploaded, even after a failure.
  for name in ${created[@]+"${created[@]}"}; do
    curl -sS -o /dev/null --max-time 10 -X DELETE -H "Authorization: Bearer $TOKEN" "$BASE/api/files/$name" || true
  done
  rm -rf "$tmp"
}
trap cleanup EXIT

checks=0
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { checks=$((checks + 1)); echo "ok - $*"; }

# req METHOD PATH [curl args...]: sets STATUS; headers in $tmp/headers, body in $tmp/body.
req() {
  local method=$1 path=$2
  shift 2
  local verb=(-X "$method")
  [[ $method == HEAD ]] && verb=(--head)
  STATUS=$(curl -sS --max-time 120 "${verb[@]}" -o "$tmp/body" -D "$tmp/headers" -w '%{http_code}' "$@" "$BASE$path") ||
    fail "$method $path: curl could not complete the request"
}
auth() { req "$@" -H "Authorization: Bearer $TOKEN"; }

# header NAME: value of a response header from the last request (empty if absent).
header() { grep -i "^$1:" "$tmp/headers" | head -n 1 | cut -d: -f2- | sed 's/^ *//' | tr -d '\r' || true; }
body() { cat "$tmp/body"; }

expect() { # expect STATUS WHAT
  [[ $STATUS == "$1" ]] || fail "$2: expected HTTP $1, got $STATUS: $(head -c 300 "$tmp/body")"
}
expect_header() { # expect_header NAME VALUE WHAT
  local got
  got=$(header "$1")
  [[ $got == "$2" ]] || fail "$3: expected header $1: '$2', got '$got'"
}
expect_common_headers() { # expect_common_headers WHAT
  expect_header x-robots-tag "$ROBOTS_TAG" "$1"
  expect_header referrer-policy no-referrer "$1"
  expect_header x-content-type-options nosniff "$1"
  expect_header content-security-policy "$CSP" "$1"
}

n=0
new_name() { # new_name EXT: a fresh, valid, unique remote name
  n=$((n + 1))
  printf 'e2e%s%d%05d.%s' "$(date +%s)" "$n" "$RANDOM" "$1"
}

size() { wc -c <"$1" | tr -d ' '; }

# slice FILE OFFSET LENGTH OUT: bytes [OFFSET, OFFSET+LENGTH) of FILE.
slice() {
  tail -c +"$(($2 + 1))" "$1" >"$tmp/rest"
  head -c "$3" "$tmp/rest" >"$4"
}

# patch NAME OFFSET TOTAL FILE: sends FILE as the chunk starting at OFFSET.
patch() {
  auth PATCH "/api/files/$1" -H "Upload-Offset: $2" -H "Upload-Length: $3" \
    -H 'Content-Type: application/octet-stream' --data-binary "@$4"
}

# upload NAME FILE [START]: chunked upload from START (default 0), checking every response.
upload() {
  local name=$1 file=$2 offset=${3:-0} total len
  total=$(size "$file")
  while ((offset < total)); do
    len=$((total - offset < CHUNK ? total - offset : CHUNK))
    slice "$file" "$offset" "$len" "$tmp/chunk"
    patch "$name" "$offset" "$total" "$tmp/chunk"
    offset=$((offset + len))
    if ((offset < total)); then
      expect 202 "PATCH $name at $((offset - len))"
      expect_header upload-offset "$offset" "PATCH $name at $((offset - len))"
    else
      expect 201 "final PATCH $name"
    fi
  done
}

# download NAME FILE: GET /i/NAME and compare it byte for byte with FILE.
download_matches() {
  req GET "/i/$1"
  expect 200 "GET /i/$1"
  cmp -s "$tmp/body" "$2" || fail "GET /i/$1: downloaded bytes differ from the upload"
}

echo "Testing $BASE"

# Basics
req GET /healthz
expect 200 "GET /healthz"
ok "healthz"

req GET /robots.txt
expect 200 "GET /robots.txt"
expect_common_headers "robots.txt"
expect_header cache-control no-store "robots.txt"
grep -q '^Allow: /i/$' "$tmp/body" || fail "robots.txt: no 'Allow: /i/' for preview bots"
tail -n 2 "$tmp/body" | tr '\n' '|' | grep -q '^User-agent: \*|Disallow: /|$' ||
  fail "robots.txt: does not end with 'User-agent: *' / 'Disallow: /'"
ok "robots.txt allows preview bots on /i/ and disallows everyone else"

req GET /
if [[ $STATUS == 302 ]]; then
  [[ -n $(header location) ]] || fail "GET /: 302 without a Location"
  ok "root redirects to $(header location)"
else
  expect 404 "GET / (no PUSH_ROOT_REDIRECT)"
  ok "root is 404 (no redirect configured)"
fi

req GET /nope
expect 404 "GET /nope"
expect_common_headers "404"
expect_header cache-control no-store "404"
ok "unknown paths are 404 with security headers"

# Auth
req GET /api/ping
expect 401 "ping without a token"
expect_common_headers "401"
cp "$tmp/body" "$tmp/401-missing"
req GET /api/ping -H "Authorization: Bearer ${TOKEN}x"
expect 401 "ping with a wrong token"
cmp -s "$tmp/body" "$tmp/401-missing" || fail "missing and wrong tokens get different 401 bodies"
ok "missing and wrong tokens get identical 401s"

name=$(new_name png)
req PATCH "/api/files/$name" -H 'Upload-Offset: 0' -H 'Upload-Length: 5' --data-binary hello
expect 401 "PATCH without a token"
req HEAD "/api/files/$name" -H "Authorization: Bearer nope"
expect 401 "HEAD with a wrong token"
req DELETE "/api/files/$name"
expect 401 "DELETE without a token"
ok "every API route needs the token"

auth GET /api/ping
expect 204 "ping with the token"
ok "ping with the token is 204"

# Single-chunk upload
name=$(new_name png)
created+=("$name")
printf '\x89PNG\r\n\x1a\nnot really a png' >"$tmp/small.png"
patch "$name" 0 "$(size "$tmp/small.png")" "$tmp/small.png"
expect 201 "single-chunk upload"
grep -q "\"url\":\"[^\"]*/i/$name\"" "$tmp/body" || fail "201 body has no url for $name: $(body)"
grep -q "\"name\":\"$name\"" "$tmp/body" || fail "201 body has no name: $(body)"
grep -q "\"size\":$(size "$tmp/small.png")[,}]" "$tmp/body" || fail "201 body has the wrong size: $(body)"
download_matches "$name" "$tmp/small.png"
expect_header content-type image/png "GET png"
expect_header cache-control "private, max-age=86400" "GET png"
expect_common_headers "GET png"
[[ -z $(header content-disposition) ]] || fail "png should render inline, got Content-Disposition"
ok "single-chunk upload is 201 and serves inline as image/png"

patch "$name" 0 "$(size "$tmp/small.png")" "$tmp/small.png"
expect 409 "re-upload of an existing name"
grep -q '"error":"exists"' "$tmp/body" || fail "409 should be exists: $(body)"
ok "existing names can't be overwritten (409 exists)"

# Chunked transfer encoding (no Content-Length), in case a proxy re-chunks the body. Chunked encoding is
# HTTP/1.1 only: over HTTP/2 curl would send the chunk framing as part of the body.
name=$(new_name txt)
created+=("$name")
printf 'hello, chunked' >"$tmp/chunked.txt"
auth PATCH "/api/files/$name" --http1.1 -H 'Upload-Offset: 0' -H "Upload-Length: $(size "$tmp/chunked.txt")" \
  -H 'Transfer-Encoding: chunked' --data-binary "@$tmp/chunked.txt"
expect 201 "chunked upload"
download_matches "$name" "$tmp/chunked.txt"
expect_header content-type "text/plain; charset=utf-8" "GET txt"
ok "chunked upload without Content-Length is 201; txt serves as text/plain"

# HTML must download, never render
name=$(new_name html)
created+=("$name")
echo '<script>alert(document.domain)</script>' >"$tmp/x.html"
upload "$name" "$tmp/x.html"
download_matches "$name" "$tmp/x.html"
expect_header content-type application/octet-stream "GET html"
expect_header content-disposition attachment "GET html"
ok "html is served as an octet-stream attachment"

# 20 MB in 8 MiB chunks
head -c "$BIG" /dev/urandom >"$tmp/big.bin"
name=$(new_name mp4)
created+=("$name")
upload "$name" "$tmp/big.bin"
auth HEAD "/api/files/$name"
expect 200 "HEAD complete upload"
expect_header upload-offset "$BIG" "HEAD complete upload"
expect_header upload-complete 1 "HEAD complete upload"
download_matches "$name" "$tmp/big.bin"
expect_header content-type video/mp4 "GET mp4"
ok "20 MB upload in 8 MiB chunks (202, 202, 201) downloads byte for byte"

# Range and conditional requests
req GET "/i/$name" -H 'Range: bytes=1000-1999'
expect 206 "Range request"
expect_header content-range "bytes 1000-1999/$BIG" "Range request"
slice "$tmp/big.bin" 1000 1000 "$tmp/range"
cmp -s "$tmp/body" "$tmp/range" || fail "Range request returned the wrong bytes"
expect_common_headers "206"
ok "Range request is 206 with the right bytes"

req HEAD "/i/$name"
expect 200 "HEAD /i/"
expect_header content-length "$BIG" "HEAD /i/"
etag=$(header etag)
[[ -n $etag ]] || fail "no ETag on file responses"
req GET "/i/$name" -H "If-None-Match: $etag"
expect 304 "If-None-Match"
expect_common_headers "304"
ok "HEAD works and ETag revalidation is 304"

# Resume after an interrupted chunk
name=$(new_name mov)
created+=("$name")
slice "$tmp/big.bin" 0 "$CHUNK" "$tmp/chunk"
patch "$name" 0 "$BIG" "$tmp/chunk"
expect 202 "first chunk"
slice "$tmp/big.bin" "$CHUNK" "$CHUNK" "$tmp/chunk"
# Hang up part-way through the second chunk, like a dropped connection.
curl -sS -o /dev/null --limit-rate 1M --max-time 3 -X PATCH -H "Authorization: Bearer $TOKEN" \
  -H "Upload-Offset: $CHUNK" -H "Upload-Length: $BIG" --data-binary "@$tmp/chunk" \
  "$BASE/api/files/$name" 2>/dev/null && fail "the interrupted upload unexpectedly finished"
sleep 1
auth HEAD "/api/files/$name"
expect 200 "HEAD partial upload"
resume=$(header upload-offset)
[[ $resume =~ ^[0-9]+$ ]] && ((resume >= CHUNK && resume < 2 * CHUNK)) ||
  fail "HEAD after a hang-up: Upload-Offset '$resume' should be within the second chunk"
[[ -z $(header upload-complete) ]] || fail "partial upload reported as complete"
patch "$name" 0 "$BIG" "$tmp/chunk"
expect 409 "PATCH at a stale offset"
grep -q '"error":"offset"' "$tmp/body" || fail "409 should be offset: $(body)"
expect_header upload-offset "$resume" "409 offset"
upload "$name" "$tmp/big.bin" "$resume"
download_matches "$name" "$tmp/big.bin"
ok "interrupted chunk kept $((resume - CHUNK)) bytes; HEAD then resume completes byte for byte"

# Limits and bad input
head -c $((10 * 1024 * 1024 + 1)) /dev/zero >"$tmp/too-big"
name=$(new_name bin)
patch "$name" 0 $((10 * 1024 * 1024 + 1)) "$tmp/too-big"
expect 413 "chunk over 10 MiB"
ok "chunks over 10 MiB are 413"

name=$(new_name bin)
created+=("$name")
printf 0123456789 >"$tmp/ten"
patch "$name" 0 5 "$tmp/ten"
expect 400 "chunk past Upload-Length"
auth HEAD "/api/files/$name"
[[ $STATUS == 404 || $(header upload-offset) == 0 ]] || fail "rejected chunk left data behind"
ok "chunk past Upload-Length is 400 and leaves nothing behind"

for path in /i/..%2Fsecret.png /i/abcdef.PNG /i/abc.png /i/abcdef /i/abcdef.png.part; do
  req GET "$path" --path-as-is
  expect 404 "GET $path"
done
req GET /i/../tmp/abcdef.png.part --path-as-is
expect 404 "GET /i/../tmp/"
auth PATCH "/api/files/..%2Fx.png" -H 'Upload-Offset: 0' -H 'Upload-Length: 1' --data-binary x
expect 400 "PATCH with a traversal name"
ok "traversal and invalid names are rejected"

# Delete
name=${created[0]}
auth DELETE "/api/files/$name"
expect 204 "DELETE"
expect_common_headers "204"
req GET "/i/$name"
expect 404 "GET after DELETE"
auth DELETE "/api/files/$name"
expect 404 "second DELETE"
auth HEAD "/api/files/$name"
expect 404 "HEAD after DELETE"
ok "delete is 204, then the file is gone (404)"

echo "PASS: $checks checks against $BASE"
