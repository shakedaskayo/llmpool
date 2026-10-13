#!/bin/sh
# Build llmpool and install it on this Mac: the app in /Applications (menu
# bar icon, starts at login), the command line tool, Claude Code's hooks and
# the /llmpool pane, and your current Claude Code login as your own account.
#
#   sh scripts/install-macos.sh            build and install
#   sh scripts/install-macos.sh --uninstall  take it all back out
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
app="/Applications/llmpool.app"
cli="$app/Contents/MacOS/llmpool"

say() { printf '\n== %s\n' "$1"; }
quit_app() {
    osascript -e 'tell application "llmpool" to quit' >/dev/null 2>&1 || true
    pkill -f "$app/Contents/MacOS/llmpool-gui" >/dev/null 2>&1 || true
}

if [ "${1:-}" = "--uninstall" ]; then
    [ -x "$cli" ] && "$cli" setup --undo || true
    [ -x "$cli" ] && "$cli" daemon --stop >/dev/null 2>&1 || true
    quit_app
    rm -rf "$app"
    echo "llmpool is removed. Your accounts are still in the keychain under the service \"llmpool\"."
    exit 0
fi

[ "$(uname -s)" = "Darwin" ] || { echo "this installer is for macOS" >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "Rust is needed to build llmpool: https://rustup.rs" >&2; exit 1; }
cargo tauri --version >/dev/null 2>&1 || { say "Installing the Tauri build tool (once, a few minutes)"; cargo install tauri-cli --version "^2" --locked; }

say "Building"
sh "$root/scripts/prepare-sidecar.sh"
(cd "$root/crates/llmpool-gui" && cargo tauri build --bundles app)
built="$root/target/release/bundle/macos/llmpool.app"
[ -x "$built/Contents/MacOS/llmpool" ] || { echo "the build did not produce the app with its command line tool" >&2; exit 1; }

say "Installing the app"
# Whatever llmpool daemon is running was started by an older copy; it must not
# outlive the copy that replaces it.
for old in "$cli" "$HOME/.local/bin/llmpool"; do
    [ -x "$old" ] && "$old" daemon --stop >/dev/null 2>&1 || true
done
pkill -f "llmpool daemon" >/dev/null 2>&1 || true
quit_app
rm -rf "$app"
cp -R "$built" "$app"

say "Setting up"
"$cli" setup || true

say "Starting"
open -g -a "$app"
echo "llmpool is in your menu bar. In Claude Code, type /llmpool."
