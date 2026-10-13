#!/bin/sh
# Install llmpool from source.
#
#   curl -fsSL https://raw.githubusercontent.com/shakedaskayo/llmpool/main/scripts/install.sh | sh
#
# On macOS this builds the app, puts it in /Applications, installs the
# `llmpool` command and connects Claude Code. On Linux it installs the command.
# It needs git and Rust (https://rustup.rs). Nothing is downloaded but the
# source and its dependencies; run it again to update.
set -eu

repo="${LLMPOOL_REPO:-https://github.com/shakedaskayo/llmpool}"
src="${LLMPOOL_SRC:-$HOME/.local/share/llmpool-src}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "llmpool needs $1: $2" >&2; exit 1; }; }
need git "https://git-scm.com"
need cargo "install Rust from https://rustup.rs"

if [ -d "$src/.git" ]; then
  echo "== Updating the source in $src"
  git -C "$src" pull --ff-only --quiet
else
  echo "== Fetching the source into $src"
  mkdir -p "$(dirname "$src")"
  git clone --depth 1 --quiet "$repo" "$src"
fi

case "$(uname -s)" in
  Darwin)
    exec "$src/scripts/install-macos.sh"
    ;;
  *)
    # The system keyring is reached over D-Bus; its headers are needed to build.
    if ! pkg-config --exists dbus-1 2>/dev/null; then
      echo "llmpool needs the D-Bus development files to build:" >&2
      echo "  Debian, Ubuntu:  sudo apt install libdbus-1-dev pkg-config" >&2
      echo "  Fedora:          sudo dnf install dbus-devel pkgconf-pkg-config" >&2
      exit 1
    fi
    echo "== Building the llmpool command (a few minutes the first time)"
    cargo install --quiet --locked --path "$src/crates/llmpool-cli" --root "$HOME/.local"
    if command -v claude >/dev/null 2>&1; then
      "$HOME/.local/bin/llmpool" install claude
    fi
    echo
    echo "llmpool is in ~/.local/bin. Next: llmpool agents"
    ;;
esac
