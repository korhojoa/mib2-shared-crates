#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
ENGINE=${CONTAINER_ENGINE:-podman}
CACHE=${CARGO_HOME:-$HOME/.cargo}
TARGET_CACHE=${CARGO_TARGET_DIR:-$ROOT/target}
mkdir -p "$CACHE/registry" "$CACHE/git" "$TARGET_CACHE"
exec "$ENGINE" run --rm -v "$ROOT:/src" -v "$TARGET_CACHE:/src/target" -v "$CACHE/registry:/cargo/registry" -v "$CACHE/git:/cargo/git" -e CARGO_HOME=/cargo -w /src \
  --entrypoint /bin/sh "$(cat "$ROOT/compiler-image.txt")" -ec 'cargo fetch --locked;
cargo fmt --all --check
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo clippy --locked --offline --workspace --lib --bins --target armv7-unknown-nto-qnx650 -- -D warnings
cargo test --locked --offline --workspace
cargo build --locked --offline --release --workspace --target armv7-unknown-nto-qnx650'
