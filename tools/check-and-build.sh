#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
set -eu
cargo fetch --locked;
cargo fmt --all --check
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo clippy --locked --offline --workspace --lib --bins --target armv7-unknown-nto-qnx650 -- -D warnings
cargo test --locked --offline --workspace
cargo build --locked --offline --release --workspace --target armv7-unknown-nto-qnx650
sh tools/build-android-auto-helper.sh
