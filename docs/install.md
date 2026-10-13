---
title: Install
permalink: /install/
---
# Install

```bash
curl -fsSL https://raw.githubusercontent.com/shakedaskayo/llmpool/main/scripts/install.sh | sh
```

The script builds from source, so it needs git and [Rust](https://rustup.rs). The first build takes a few minutes. Run it again to update.

| | What you get |
|---|---|
| macOS | The menu-bar app in `/Applications`, the `llmpool` command in `~/.local/bin`, Claude Code connected (hooks, status line, `/llmpool`), your current login added, start at login |
| Linux | The `llmpool` command in `~/.local/bin`, and Claude Code connected if it is installed |

Windows is not supported.

## First steps

```bash
llmpool doctor                       # what is in place and what is missing
llmpool add claude --from-current    # the login Claude Code has now
llmpool add claude                   # sign in to another account, in your browser
llmpool add key openai               # a key for any provider; typed, never an argument
llmpool agents                       # the agents llmpool found on this machine
```

On macOS, open **llmpool** from the menu bar. <kbd>⌃⌥⌘N</kbd> switches the current session to the account with the most room from anywhere; <kbd>⌃⌥⌘L</kbd> opens the window.

![Adding an account]({{ '/assets/add-account.png' | relative_url }})

## From a checkout

```bash
git clone https://github.com/shakedaskayo/llmpool && cd llmpool
scripts/install-macos.sh                       # macOS: app, command, Claude Code
cargo install --path crates/llmpool-cli        # anywhere: the command alone
```

## Remove

```bash
llmpool setup --undo        # takes out the hooks, status line, mod and start-at-login
```

Your previous status line is restored. Accounts stay in your keychain until you remove them with `llmpool remove`.

## Where things live

| | |
|---|---|
| Data | `~/Library/Application Support/llmpool` on macOS, `$XDG_DATA_HOME/llmpool` on Linux |
| Logins and keys | The macOS Keychain (service `llmpool`); the system keyring on Linux |
| Rules | `llmpool policy path` |
