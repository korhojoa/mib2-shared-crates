#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Build the released helper and retain its original panic source labels.
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
TARGET=armv7-unknown-nto-qnx650
TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
export RUSTFLAGS="${RUSTFLAGS:-} -C relocation-model=pic -C link-arg=-Wl,--build-id=sha1 --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}/git=/root/.cargo/git"
cargo clippy -p extender-settings --bin android-auto-settings --locked --offline --target "$TARGET" -- -D warnings
cargo build -p extender-settings --bin android-auto-settings --locked --offline --profile ship --target "$TARGET"
mkdir -p build
cp "$TARGET_DIR/$TARGET/ship/android-auto-settings" build/android-auto-settings
arm-unknown-nto-qnx6.5.0eabi-strip build/android-auto-settings
chmod 0755 build/android-auto-settings
readelf -h build/android-auto-settings | grep 'Version5 EABI'
if readelf -d build/android-auto-settings | grep NEEDED | grep -Ev 'libc[.]so[.]3|libm[.]so[.]2|libsocket[.]so[.]3'; then
    echo 'Unexpected runtime library' >&2
    exit 1
fi
expected=a0addf2e45f53f0f0c222c59ece44160a78cb950a964fd2906bad715b7d5bdeb
actual=$(sha256sum build/android-auto-settings | cut -d' ' -f1)
[ "$actual" = "$expected" ] || { echo "Settings helper differs from the released file: $actual" >&2; exit 1; }
echo "Exact released Android Auto settings helper: $actual"
