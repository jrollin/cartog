---
name: cartog-install
description: >-
  Install or upgrade the cartog binary to match the plugin's pinned version.
  Use when the user types /cartog-install, or asks to "install cartog",
  "update cartog", "fix cartog setup", "retry cartog install", or when the
  cartog MCP server failed to start because the binary is missing.
---

# /cartog-install: Install or upgrade the cartog binary

The plugin pins a specific cartog binary version. This skill installs that
version, or upgrades an existing install to match.

```
PLUGIN_VERSION=0.35.1
```

`scripts/release.sh` bumps the `PLUGIN_VERSION=` line above at release time;
the rest of this file references that value as `$PLUGIN_VERSION` and the
agent must substitute it from the line above before running any command. The
literal string `$PLUGIN_VERSION` is NOT a shell variable in your Bash
environment, it is a token you replace.

## What to do

Probe the current state, then run the matching path. Surface every step's
output to the user, they typed this verb because they want to see what
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

### 4. If the binary is present, `>= 0.14.0`, and lacks `--apply-pending`

The deferred flags (`--defer`/`--apply-pending`) landed in **0.20.0**. Binaries
in **0.14.0–0.20.0** have `cartog self update` but reject those flags with a clap
"unexpected argument" (exit 2), so the deferred path below does **not** apply to
them. Detect this band by probing the help text:

```bash
cartog self update --help 2>&1 | grep -q -- '--apply-pending' || echo "PRE_DEFERRED"
```

When it prints `PRE_DEFERRED`, do **not** run `--defer`. Recover by reinstalling
the pinned version with the bundled installer (it overwrites the on-disk binary;
the running MCP server keeps its old inode until the user restarts):

```bash
bash "${CLAUDE_PLUGIN_ROOT}/skills/cartog/scripts/install.sh" $PLUGIN_VERSION
```

Then verify and tell the user to **restart Claude Code** (same as step 2).

### 5. If the binary is present and supports `--apply-pending` (`>= 0.20.0`)

First probe whether this binary has `--force` (added after 0.35.1). Older
binaries reject `--force`, and reject `--to` without `--defer`:

```bash
cartog self update --help 2>&1 | grep -q -- '--force' && echo "HAS_FORCE"
```

Without `HAS_FORCE` (binary 0.35.1 or older), do not run `cartog self update`:
it can only target the latest release and would overshoot the pin. Ask the
user to **wait** (`cartog self update --defer --to $PLUGIN_VERSION`, applied
when the session ends) or **update now** with the bundled installer pinned to
the plugin version, which writes a new file rather than overwriting the
running one:

```bash
bash "${CLAUDE_PLUGIN_ROOT}/skills/cartog/scripts/install.sh" $PLUGIN_VERSION
```

After the installer, tell the user to restart every Claude Code session. On
Windows the installer does not apply: see **Exit 6: wait or update now**.

With `HAS_FORCE`, run `cartog self update --to $PLUGIN_VERSION` (lands on the
pin). Relay its output to the user verbatim, then handle the exit code:

| Exit | Meaning | What to do |
|---|---|---|
| 0 | Updated | "cartog upgraded. Restart Claude Code to pick up the new binary." |
| 3 | Installed via cargo | "cartog was installed via cargo. Run `cargo install cartog --force` to upgrade." Never pass `--force` to `self update` for this: it still refuses. |
| 5 | Disk or permission failure | If the message says `cannot write to`, cartog's install directory is not writable by this user. Do **not** retry with `sudo` yourself. Give the user the command the message ends with (see **Commands you cannot run**). |
| 6 | Other cartog processes are running | Normal inside a Claude Code session: this session's MCP server is one of them, and the message lists every process it found. If it says `another cartog update is already in progress`, tell the user to rerun /cartog-install in a minute. Otherwise ask the user which they want, see **Exit 6: wait or update now** below. |
| other | Unexpected | Surface the exit code + stderr to the user. Then run the bundled installer pinned to `$PLUGIN_VERSION` as a recovery: `bash "${CLAUDE_PLUGIN_ROOT}/skills/cartog/scripts/install.sh" $PLUGIN_VERSION`. Never invoke `install.sh` without the pinned version, that would install the latest GitHub release, drifting off the plugin's pin. |

#### Exit 6: wait or update now

Ask the user, with waiting as the default:

1. **Wait (default)**: `cartog self update --defer --to $PLUGIN_VERSION`. Tell
   the user "cartog will update to $PLUGIN_VERSION when this session ends." It
   can stay blocked while sessions in other projects keep running; if the user
   says it never lands, offer option 2.
2. **Update now** (macOS / Linux only): the binary is replaced while the listed
   processes keep running the previous version until their session restarts.
   - `HAS_FORCE`: `cartog self update --force --to $PLUGIN_VERSION`. Its output
     lists the processes still on the old version: tell the user to restart
     those sessions, this one included.
   - No `--force` (binary 0.35.1 or older): reinstall with the bundled
     installer, which writes a new file rather than overwriting the running
     one: `bash "${CLAUDE_PLUGIN_ROOT}/skills/cartog/scripts/install.sh" $PLUGIN_VERSION`.
     Then tell the user to restart every Claude Code session. The new binary
     has `--force` for next time.
   - On Windows nothing replaces a running `cartog.exe`: tell the user to close
     every Claude Code session and editor using cartog, then, in a terminal:
     - `HAS_FORCE`: `cartog self update --to $PLUGIN_VERSION`
     - no `--force`: download the `.zip` for `$PLUGIN_VERSION` from
       `https://github.com/jrollin/cartog/releases/tag/v$PLUGIN_VERSION` and
       replace `cartog.exe` with the one inside (an old binary rejects `--to`
       without `--defer`).

#### Commands you cannot run

Some steps need rights this session lacks: the user declined the Bash
permission prompt, the sandbox blocked the write, or the output says
`permission denied`, `cannot write to`, or needs `sudo`. Never run `sudo`
yourself (it waits for a password you cannot type). Instead, stop and give the
user the exact command, with `$PLUGIN_VERSION` substituted:

- Commands without `sudo`: they can run it in this session by typing
  `! <command>` (for example `! cartog self update --force --to $PLUGIN_VERSION`),
  or in their own terminal.
- An install directory only root can write (e.g. `/usr/local/bin`): use the
  command from cartog's error message when it printed one, else
  `curl -fsSL https://www.cartog.dev/install.sh | CARTOG_VERSION=$PLUGIN_VERSION sh`,
  run in their own terminal. The installer asks for `sudo` only when the
  directory needs it.

After they run it, re-probe `cartog --version` and continue from step 6.

`cartog self update --defer --to $PLUGIN_VERSION` is the right default from
**inside** a Claude Code session: the MCP server is a running process that
blocks an in-place swap, and `--to` arms the **pinned** version (not the latest
GitHub release) so the deferred update lands exactly on the plugin's pin.
`--force` is for when the user wants the new binary now and accepts that
running sessions stay on the old one until restarted.

### 6. If already up to date

Compare `cartog --version` against the `PLUGIN_VERSION=` line at the top of
this file. If they match, print "cartog is already at the pinned version, 
nothing to do." and stop.

## Notes

- This skill only writes to the cartog binary's install directory. It does
  not touch the project's `.cartog.toml`, the index database, or any user
  files.
- `${CLAUDE_PLUGIN_ROOT}` resolves to the directory Claude Code expanded
  the plugin into. Use it literally, do not substitute a guessed path.
- For a totally offline / vetted install path, point the user at
  `https://www.cartog.dev/install.sh` which they can download
  and inspect before running.
