#!/usr/bin/env bash
# Builds static reprinter-web binaries, one per architecture reprinter
# supports, into web/dist/ with a SHA256SUMS file:
#
#   web/build-release.sh               # amd64, arm64 and armhf
#   web/build-release.sh arm64         # chosen architectures
#
# The binaries are linked against musl, so each runs on any Linux release of
# its architecture (Debian 12/13, Ubuntu, Raspberry Pi OS, ...). Each one
# contains the whole installer: copy it to the host and run
# `sudo ./reprinter-web-<arch> install`. Needs Rust with rustup; no cross
# toolchain (linking uses rust-lld, see .cargo/config.toml).

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

declare -A TARGETS=(
  [amd64]=x86_64-unknown-linux-musl
  [arm64]=aarch64-unknown-linux-musl
  [armhf]=arm-unknown-linux-musleabihf
)

archs=("$@")
[ ${#archs[@]} -gt 0 ] || archs=(amd64 arm64 armhf)

mkdir -p dist
for arch in "${archs[@]}"; do
  target="${TARGETS[$arch]:-}"
  [ -n "$target" ] || { echo "unknown architecture: $arch (use amd64, arm64 or armhf)" >&2; exit 1; }
  echo "==> $arch ($target)"
  if command -v rustup >/dev/null; then
    rustup target add "$target" >/dev/null
  fi
  cargo build --release --locked --target "$target"
  cp "target/$target/release/reprinter-web" "dist/reprinter-web-$arch"
done
(cd dist && sha256sum reprinter-web-* >SHA256SUMS && cat SHA256SUMS)
