#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
ENGINE=${CONTAINER_ENGINE:-podman}
CACHE=${CARGO_HOME:-$HOME/.cargo}
TARGET_CACHE=${CARGO_TARGET_DIR:-$ROOT/target}
mkdir -p "$CACHE/registry" "$CACHE/git" "$TARGET_CACHE"
if [ "${PUBLIC_COMPILER_DIRECT:-0}" = 1 ]; then
    test -x /opt/rust-qnx/bin/rustc
    test -x /usr/local/bin/link-qnx.sh
    export CARGO_TARGET_DIR="$TARGET_CACHE"
    cd "$ROOT"
    exec sh tools/check-and-build.sh
fi
exec "$ENGINE" run --rm -v "$ROOT:/src" -v "$TARGET_CACHE:/src/target" -v "$CACHE/registry:/cargo/registry" -v "$CACHE/git:/cargo/git" -e CARGO_HOME=/cargo -w /src \
  --entrypoint /bin/sh "$(cat "$ROOT/compiler-image.txt")" tools/check-and-build.sh
