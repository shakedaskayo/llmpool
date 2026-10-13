#!/bin/sh
# Build the command line tool and place it where the app bundle expects its
# companion binary: binaries/llmpool-<target triple>.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
triple="$(rustc -vV | sed -n 's/^host: //p')"
cargo build --release --manifest-path "$root/Cargo.toml" -p llmpool-cli
mkdir -p "$root/crates/llmpool-gui/binaries"
cp "$root/target/release/llmpool" "$root/crates/llmpool-gui/binaries/llmpool-$triple"
