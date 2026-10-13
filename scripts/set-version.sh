#!/bin/sh
# Stamp a release's version into what is about to be built, so that
# `llmpool --version` and the app say which release they are.
set -eu
v="${1:?a version, such as 0.1.6}"
cd "$(dirname "$0")/.."
perl -0pi -e 's/(\[workspace\.package\]\nversion = ")[^"]*/${1}'"$v"'/' Cargo.toml
perl -0pi -e 's/(name = "llmpool[^"]*"\nversion = ")[^"]*/${1}'"$v"'/g' Cargo.lock
perl -pi -e 's/("version": ")[^"]*/${1}'"$v"'/ if $. < 10' crates/llmpool-gui/tauri.conf.json
