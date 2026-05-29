---
name: cartog-install
description: >-
  Install or upgrade the cartog binary to match the plugin's pinned version.
  Use when the user types /cartog-install, or asks to "install cartog",
  "update cartog", "fix cartog setup", "retry cartog install", or when the
  cartog MCP server failed to start because the binary is missing.
---

# /cartog-install — Install or upgrade the cartog binary

The plugin pins a specific cartog binary version. This skill installs that
version, or upgrades an existing install to match.

```
PLUGIN_VERSION=0.19.0
```

`scripts/release.sh` bumps the `PLUGIN_VERSION=` line above at release time;
the rest of this file references that value as `$PLUGIN_VERSION` and the
agent must substitute it from the line above before running any command. The
literal string `$PLUGIN_VERSION` is NOT a shell variable in your Bash
environment — it is a token you replace.

## What to do

Probe the current state, then run the matching path. Surface every step's
output to the user — they typed this verb because they want to see what
happens.

### 1. Probe `cartog --version`

Run `cartog --version` via Bash. Capture exit code and version string.

### 2. If the binary is missing or returns a non-zero exit

Run the bundled installer, substituting `$PLUGIN_VERSION` with the version
from the `PLUGIN_VERSION=` line at the top of this file:

```bash
bash "${CLAUDE_PLUGIN_ROOT}/skills/cartog/scripts/install.sh" $PLUGIN_VERSION
```

After install:

1. Run `hash -r` so the shell forgets cached lookups.
2. Run `cartog --version` to confirm the install matches `$PLUGIN_VERSION`.
3. If `cartog --version` still fails, check whether the install directory
   (printed by `install.sh`) is on the user's PATH. If not, tell the user
   the exact `export PATH="<dir>:$PATH"` line to add to their shell rc.

Tell the user to **restart Claude Code** so the MCP server picks up the
new binary. The cartog MCP server can't reconnect mid-session.

### 3. If the binary is present and `< 0.14.0`

Pre-0.14.0 binaries don't have `cartog self update`. Use the same bundled
installer with the pinned version (substitute `$PLUGIN_VERSION` from the top):

```bash
bash "${CLAUDE_PLUGIN_ROOT}/skills/cartog/scripts/install.sh" $PLUGIN_VERSION
```

Then verify and tell the user to restart Claude Code (same as step 2).

### 4. If the binary is present and `>= 0.14.0`

Run `cartog self update` via Bash. Relay its output to the user verbatim.

Handle the documented exit codes:

| Exit | Meaning | What to tell the user |
|---|---|---|
| 0 | Updated | "cartog upgraded. Restart Claude Code to pick up the new binary." |
| 3 | Installed via cargo | "cartog was installed via cargo. Run `cargo install cartog --force` to upgrade." |
| 6 | Another cartog process is running | Inside a Claude Code session this is the **normal** case — the cartog MCP server (`cartog serve`) is the running peer. Arm a deferred update **to the pinned version** instead, substituting `$PLUGIN_VERSION` from the top of this file: `cartog self update --defer --to $PLUGIN_VERSION`. Tell the user "cartog will update to $PLUGIN_VERSION when this session ends." (Manual alternative: close other Claude Code sessions, stop any background `cartog watch`, then run /cartog-install again.) |
| other | Unexpected | Surface the exit code + stderr to the user. Then run the bundled installer pinned to `$PLUGIN_VERSION` as a recovery: `bash "${CLAUDE_PLUGIN_ROOT}/skills/cartog/scripts/install.sh" $PLUGIN_VERSION`. Never invoke `install.sh` without the pinned version — that would install the latest GitHub release, drifting off the plugin's pin. |

`cartog self update --defer --to $PLUGIN_VERSION` is the right call from
**inside** a Claude Code session: the MCP server is the peer that blocks an
in-place swap, and `--to` arms the **pinned** version (not the latest GitHub
release) so the deferred update lands exactly on the plugin's pin. Plain
`cartog self update` is for a terminal with no cartog serve/watch running.

### 5. If already up to date

Compare `cartog --version` against the `PLUGIN_VERSION=` line at the top of
this file. If they match, print "cartog is already at the pinned version —
nothing to do." and stop.

## Notes

- This skill only writes to the cartog binary's install directory. It does
  not touch the project's `.cartog.toml`, the index database, or any user
  files.
- `${CLAUDE_PLUGIN_ROOT}` resolves to the directory Claude Code expanded
  the plugin into. Use it literally — do not substitute a guessed path.
- For a totally offline / vetted install path, point the user at
  `https://jrollin.github.io/cartog/install.sh` which they can download
  and inspect before running.
