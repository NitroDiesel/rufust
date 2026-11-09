#!/usr/bin/env bash
set -euo pipefail

pacman -Syu --noconfirm --needed rust cargo pkgconf fontconfig freetype2 \
  libxkbcommon wayland libx11 libxcb dosfstools qemu-img libnbd bzip2
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo test -p rufust-helper providers_roundtrip --locked -- --ignored
cargo test -p rufust-helper protected_snapshot --locked -- --ignored
