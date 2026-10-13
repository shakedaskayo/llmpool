#!/bin/sh
# Install llmpool.
#
#   curl -fsSL https://raw.githubusercontent.com/shakedaskayo/llmpool/main/scripts/install.sh | sh
#
# On macOS: the app in /Applications, the `llmpool` command, Claude Code
# connected. On Linux: the command. A ready-made build is downloaded from the
# latest release and checked against its published checksum; where there is
# none for this machine, or with LLMPOOL_FROM_SOURCE=1, it is built from
# source (which needs git and Rust). Run it again to update.
set -eu

repo="${LLMPOOL_REPO:-https://github.com/shakedaskayo/llmpool}"
src="${LLMPOOL_SRC:-$HOME/.local/share/llmpool-src}"
os="$(uname -s)"
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) arch="$(uname -m)" ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || { echo "llmpool needs $1: $2" >&2; exit 1; }; }
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d" " -f1; else shasum -a 256 "$1" | cut -d" " -f1; fi; }

# Fetch a release file and its checksum into $tmp; fail quietly if either is
# missing or they do not agree.
fetch() {
  curl -fsSL -o "$tmp/$1" "$repo/releases/latest/download/$1" 2>/dev/null || return 1
  curl -fsSL -o "$tmp/checksums.txt" "$repo/releases/latest/download/checksums.txt" 2>/dev/null || return 1
  want="$(grep " $1\$" "$tmp/checksums.txt" | cut -d" " -f1)"
  [ -n "$want" ] && [ "$want" = "$(sha256 "$tmp/$1")" ] || { echo "the download of $1 does not match its checksum; not using it" >&2; return 1; }
}

from_source() {
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
  case "$os" in
    Darwin) exec "$src/scripts/install-macos.sh" ;;
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
      ;;
  esac
}

linux_done() {
  if command -v claude >/dev/null 2>&1; then "$HOME/.local/bin/llmpool" install claude; fi
  echo
  echo "llmpool is in ~/.local/bin. Next: llmpool agents"
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [ -n "${LLMPOOL_FROM_SOURCE:-}" ]; then
  from_source
  [ "$os" = Darwin ] || linux_done
  exit 0
fi

case "$os" in
  Darwin)
    if fetch "llmpool-macos-$arch.zip"; then
      echo "== Downloaded llmpool for macOS ($arch)"
      ditto -x -k "$tmp/llmpool-macos-$arch.zip" "$tmp/app"
      curl -fsSL -o "$tmp/install-macos.sh" "$repo/raw/main/scripts/install-macos.sh"
      LLMPOOL_APP="$tmp/app/llmpool.app" sh "$tmp/install-macos.sh"
    else
      echo "== No ready-made build for this Mac; building from source"
      from_source
    fi
    ;;
  Linux)
    if fetch "llmpool-linux-$arch.tar.gz"; then
      echo "== Downloaded llmpool for Linux ($arch)"
      mkdir -p "$HOME/.local/bin"
      tar -xzf "$tmp/llmpool-linux-$arch.tar.gz" -C "$tmp"
      install -m 755 "$tmp/llmpool" "$HOME/.local/bin/llmpool"
    else
      echo "== No ready-made build for this machine; building from source"
      from_source
    fi
    linux_done
    ;;
  *)
    echo "llmpool runs on macOS and Linux" >&2
    exit 1
    ;;
esac
