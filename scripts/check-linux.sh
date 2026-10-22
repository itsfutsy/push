#!/usr/bin/env bash
# Checks the desktop app compiles for Linux, in an ubuntu:24.04 container (native arch, e.g. arm64 under colima).
#
#   scripts/check-linux.sh                       cargo check
#   scripts/check-linux.sh build                 any cargo arguments, e.g. a debug build that links
#   scripts/check-linux.sh clippy
#
# The repo is mounted read-only and copied into the container (tauri-build writes src-tauri/gen).
# Named volumes keep the target dir and the cargo registry, so re-runs are incremental.
# Clean up with: docker volume rm push-linux-target push-linux-cargo; docker rmi push-linux-check
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
image=push-linux-check

echo "Preparing the $image image (the first run takes a few minutes)…" >&2
if ! log=$(docker build --quiet --tag "$image" - 2>&1 <<'EOF'
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
# Same packages as .github/workflows/linux.yml, plus what the runner image already has.
RUN apt-get update && apt-get install -y --no-install-recommends \
      libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev \
      libayatana-appindicator3-dev librsvg2-dev ca-certificates pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --component clippy --default-toolchain stable
EOF
); then
  echo "$log" >&2
  exit 1
fi

[ $# -gt 0 ] || set -- check

docker run --rm \
  --volume "$repo:/src:ro" \
  --volume push-linux-target:/target \
  --volume push-linux-cargo:/opt/cargo/registry \
  --env CARGO_TARGET_DIR=/target \
  "$image" bash -euo pipefail -c '
    mkdir /work
    # tar keeps mtimes, so cargo only rebuilds what changed.
    tar -C /src --exclude=src-tauri/target --exclude=src-tauri/gen -cf - src-tauri ui | tar -C /work -xf -
    cd /work/src-tauri
    cargo "$@"
  ' bash "$@"
