#!/usr/bin/env bash
set -euo pipefail

apt-get update
apt-get install -y --no-install-recommends pkg-config libfontconfig1-dev \
  libfreetype-dev libxkbcommon-dev libwayland-dev libx11-dev libxcb1-dev \
  dosfstools qemu-utils libnbd-bin bzip2
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo test -p rufust-helper providers_roundtrip --locked -- --ignored
cargo test -p rufust-helper protected_snapshot --locked -- --ignored
