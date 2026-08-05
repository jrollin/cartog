# Updating cartog

> The first install is always [`install.sh`](https://github.com/jrollin/cartog#install) or `cargo install cartog`. Once cartog is on your `PATH`, the steady state is managed by the `cartog self` command group described below.

## Quick reference

| Command | What it does |
|---------|--------------|
| `cartog self update` | Upgrade in place to the latest stable release |
| `cartog self update --check` | Check whether an update exists; do nothing else |
| `cartog self update --defer` | Arm a deferred update (record the target, do **not** swap); succeeds even while a peer runs |
| `cartog self update --apply-pending` | Apply a previously-armed deferred update once no peer holds the lock |
| `cartog self version` | Print version, `describe` build string, target triple, install source, last check timestamp, pending update |
| `cartog self rollback` | Restore the previous binary (the `<bin>.old` sibling) |

## Upgrade in place

```bash
cartog self update
```

Downloads the matching tarball/zip for your platform, verifies its SHA256 against the release's `SHA256SUMS` asset, atomically swaps the binary in place, and runs a smoke test. The previous binary is preserved as a sibling `<bin>.old` for one rollback.

If a peer `cartog serve` or `cartog watch` is still running, the upgrade refuses (exit `6`) and names the live process so you can stop it first. PID files at the platform state directory (see below) are the detection mechanism.

### Output formats

```bash
cartog self update --check              # human-readable
cartog self update --check --json       # {"current":"…","latest":"…","outdated":bool}
cartog self update --check --quiet      # no output; exit code is the only signal
```

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Up to date (or upgrade succeeded) |
| `1` | Update available (only with `--check`) |
| `2` | Network or parse error reaching `api.github.com` |
| `3` | Refused — binary was installed via `cargo install`. Run `cargo install cartog --force` instead |
| `4` | Checksum mismatch; no filesystem mutation, original binary intact |
| `5` | New binary failed smoke test; previous binary restored |
| `6` | A peer `cartog serve`/`watch` is running |

## Deferred update (in-session)

Inside a Claude Code session the cartog plugin runs `cartog serve --watch` as the MCP server. That process holds the serve PID lock for the whole session, so a plain `cartog self update` would refuse (exit `6`) — you cannot swap the inode of a running binary. The deferred flow splits the decision from the swap:

1. **Arm** — `cartog self update --defer` records the target version in the state file and exits **without** touching the binary. It succeeds even while the serve peer is live (it deliberately skips the peer check). By default it arms the **latest stable** release; pass `--to <version>` to pin an exact target. Both `/cartog-install` and the `cartog_update` MCP tool arm the plugin's **pinned** version (`--to $PLUGIN_VERSION`, discovered from the plugin manifest) so a plugin-managed update can't overshoot the pin; they fall back to latest only outside a plugin install.
2. **Apply** — `cartog self update --apply-pending` reads the armed target, waits for any peer lock to clear, performs the real swap, and clears the intent. The wait is sized by what holds the lock: a lock this project owns gets a bounded grace (~2s) to absorb its own `serve` shutting down, while a lock held outside this project is not waited on at all, since it stays held for as long as that session stays open (waiting on it would only run a session hook out of time). The swap only proceeds when the armed target is **newer** than the installed binary — an armed target at or below the current version is a clean no-op (no downgrade). If a peer is still live after the wait (e.g. a second Claude Code window on the same project), it exits `6`, keeps the intent, and retries — the binary lands once the other session closes. Apply runs at **two** boundaries: the plugin's SessionEnd hook (after the serve process exits), and the **next SessionStart** as a catch-up. The SessionStart apply runs in the background pipeline, so it never blocks the session.
3. **Confirm** — the next SessionStart surfaces a one-line "cartog updated to X" breadcrumb, and the drift warning becomes "cartog X will be applied when this session ends" while an update is pending.

> **Why two apply points.** SessionEnd is bounded by Claude Code's teardown grace window; if the serve peer is slow to exit (SIGTERM → SIGKILL) the hook can be cancelled mid-download and the swap never lands. The SessionStart catch-up has no such deadline. But the new session's own `cartog serve --watch` takes the serve lock at startup and holds it all session, so the catch-up runs `--apply-pending --at-startup`, which **excludes this project's own serve/watch peer** from the peer-wait — the atomic same-FS swap is safe under a live same-project peer (it keeps its file descriptor on the old inode until it re-execs). A serve peer from **another** project still blocks (exit `6`, retry). On Windows, `--at-startup` is a no-op (a running `.exe` cannot be renamed while a peer holds it), so there the swap still waits for the same-project peer to exit. The apply is idempotent (`decide_apply` skips when already at/past the target) and self-clearing, so running at both boundaries is safe.

The tarball is fetched at **apply** time, keyed to the armed target — not at arm time.

```bash
cartog self update --defer                 # arm the latest stable release, no swap (exit 0 even with a peer)
cartog self update --defer --to 0.20.0     # arm an exact pinned version instead of latest
cartog self update --defer --json          # {"status":"armed","current":…,"target":…,"apply":"session-end-or-restart"}
cartog self update --apply-pending          # apply the armed update once no peer holds the lock
cartog self update --apply-pending --at-startup  # as above, but ignore THIS project's own serve peer (SessionStart use)
```

### Flow

```mermaid
flowchart TD
    drift{"installed < plugin pin?"}
    drift -- no --> done([up to date — no-op])
    drift -- yes --> arm["arm intent: --defer --to PIN<br/>(state file; binary untouched)"]

    arm --> apply

    subgraph apply["apply (idempotent, runs at both boundaries)"]
        direction TB
        endHook["SessionEnd hook<br/>(grace-window bounded)"]
        startHook["next SessionStart, background<br/>(--at-startup: ignore own serve peer)"]
        endHook --> peer
        startHook --> peer
        peer{"peer holds the lock?<br/>(own: ~2s grace,<br/>another project: no wait)"}
        peer -- "still held" --> kept["exit 6: keep intent,<br/>retry next boundary"]
        peer -- "clear / own peer only" --> swap["download tarball, verify SHA-256,<br/>atomic swap, smoke test"]
        swap -- "ok" --> clear["clear intent +<br/>write 'updated to X' breadcrumb"]
        swap -- "checksum (4) / smoke (7)" --> rollback["restore .old,<br/>clear intent (deterministic)"]
        swap -- "network (2) / disk (5)" --> kept
    end

    kept -. "retries until lock clears" .-> peer
    clear --> confirm([next SessionStart confirms<br/>'cartog updated to X'])
```

The two apply boundaries are the key to convergence: a SessionEnd apply cancelled by teardown is retried at the next SessionStart, where `--at-startup` lets it land despite the session's own serve peer.

### Plugin version bumps (new and existing users)

When a new plugin version ships, here is what each cohort experiences:

- **New user** — `cartog serve` can't start the first session (no binary yet); the SessionStart hook forks `install.sh` pinned to the plugin version (downloads the release tarball, **verifies its SHA-256**, installs). cartog tools are live from the **next** session. `/cartog-install` installs synchronously if you don't want to wait.
- **Existing user, passive, `>= 0.20`** — at SessionEnd the hook auto-arms the pinned version on drift (`--defer --to $PLUGIN_VERSION`) and applies it once the serve lock clears; if that apply is cancelled by session teardown, the next SessionStart applies it as a background catch-up. Either way the next SessionStart confirms "cartog updated to X". No manual action required.
- **Existing user, active** — running `/cartog-install` (or the `cartog_update` tool) mid-session arms the pin immediately; it lands at the same SessionEnd boundary.
- **Existing user, `0.14`–`0.20`** — these binaries have `cartog self update` but predate the deferred flags (`--defer`/`--apply-pending` landed in 0.20.0). The SessionEnd hook probes capability and converges them via the bundled `install.sh` pinned to `$PLUGIN_VERSION` — the same pin-exact path as the legacy cohort. (A plain `cartog self update` would fetch the **latest** release, overshooting the pin with no `--to` to constrain it on those versions.) Without this, firing the deferred flags at them errored with clap exit `2` and looped forever as a false "transient" failure.
- **cargo-installed user** — `cartog self update` refuses (exit `3`) because it must not clobber a cargo-managed binary. The SessionStart drift line and the SessionEnd breadcrumb both tell this cohort to run `cargo install cartog --force` (not `/cartog-install`).
- **Legacy `<0.14` user** — auto-upgraded at SessionEnd via `install.sh` (that cohort predates `cartog self update`).

Notes and edge cases:

- **Confirmation latency** — for the armed/active path the full loop spans two session boundaries (arm this session → apply at SessionEnd → confirm next SessionStart). There is no mid-session confirmation; SessionEnd hook output goes to the session log, not the chat.
- **Multi-window** — a second Claude Code window holding the serve lock defers the apply (exit `6`, intent kept) until that window closes; the SessionStart drift line says so.
- **Release timing** — `release.sh` pushes the version bump and tag before the release workflow finishes building the tarballs. In the few-minute build window the pinned tarball can 404; armed paths self-heal (network failure keeps the intent and retries next session), and the marketplace only serves new plugin files to users after the build completes in practice.
- **A broken release** — a checksum (`4`) or smoke-test (`7`) failure clears the intent, restores the previous binary, and surfaces an actionable message rather than retry-looping. If a swap is interrupted (e.g. SIGKILL mid-rename), the previous binary is preserved at `<bin>.old` — recover with `cartog self rollback`.

### Exit codes — `--defer`

| Code | Meaning |
|------|---------|
| `0` | Armed (or already up to date — nothing to arm) |
| `2` | Network or parse error reaching `api.github.com` |
| `3` | Refused — binary was installed via `cargo install` |
| `5` | State write failed; the intent was not persisted |

`--defer` never returns `6` — arming under a live peer is the whole point.

### Exit codes — `--apply-pending`

| Code | Meaning | Pending intent |
|------|---------|----------------|
| `0` | Applied, nothing armed, or already at target | cleared (or n/a) |
| `2` | Network error fetching the target tarball | kept (retry next session) |
| `3` | Cargo-installed | cleared |
| `4` | Checksum mismatch | cleared (won't self-heal) |
| `5` | Disk/permission fault during the swap | kept (retry next session) |
| `6` | A peer is still running after the bounded wait | kept (retry next session) |
| `7` | New binary failed its smoke test; previous binary restored | cleared (deterministic — won't self-heal) |

Deterministic failures (`4` checksum, `7` smoke) clear the intent so the same tarball is not retried every session; transient ones (`2` network, `5` disk) keep it for the next boundary. A smoke failure surfaces an actionable message pointing at `cartog self update` / `/cartog-install` rather than implying a silent retry will fix it.

`--apply-pending` also serializes against a concurrent `--apply-pending` (two Claude Code windows closing at once) via a short-lived `apply-update` lock: the second invocation sees the lock held and exits `0` as a benign no-op, leaving the in-flight apply to land the swap.

## Inspect the installation

```bash
cartog self version
cartog self version --json
```

Reports the bare semver, a `describe` string (`git describe` output, e.g. `v0.29.1-2-g3e2822c` for an unreleased main build vs `v0.29.1` for a release), target triple (e.g. `aarch64-apple-darwin`), install source, and the timestamp of the last successful update check (`never` if none). The semver, not `describe`, is what `cartog self update` compares against the latest release.

`install_source` is one of:
- `release-tarball` — downloaded from a GitHub release (or installed via `install.sh`)
- `cargo` — installed via `cargo install cartog`
- `dev` — built locally with `cargo build`

## Roll back a bad update

```bash
cartog self rollback
```

Atomically swaps the `<bin>.old` sibling back onto `<bin>`. Exits non-zero with a clear message if no `.old` is present. Forward-rollback is not supported: after a successful rollback, the `.old` is removed.

## Cargo-installed binaries

`cartog self update` refuses to overwrite a `cargo install cartog` binary (exit `3`) and prints the exact replacement command:

```bash
cargo install cartog --force
```

`--check`, `version`, and `rollback` still work — only the in-place upgrade is refused.

## Daily background check

By default, cartog runs at most one update check per 24 hours from interactive sessions. The check is non-blocking: it spawns a background thread that fetches the latest release tag, persists the result, and exits without ever holding up your command. The result surfaces as a one-line hint at the start of the *next* invocation.

The check is suppressed when:
- `stdout` is not a TTY (CI, pipes, scripts)
- The current command is `cartog serve` or `cartog watch`
- `CARTOG_NO_UPDATE_CHECK=1` is set
- `CARTOG_UPDATE_CHECK=never` is set

### Environment variables

| Variable | Effect |
|----------|--------|
| `CARTOG_NO_UPDATE_CHECK=1` | Disable all auto-check |
| `CARTOG_UPDATE_CHECK=never` | Same as above (alternative name) |
| `CARTOG_UPDATE_CHECK=daily` | Default — check at most once per 24h |
| `CARTOG_UPDATE_CHECK=always` | Check on every invocation (debugging) |
| `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` | Honored by all network calls |

## State file

cartog persists `last_update_check`, `last_known_latest`, and `last_known_outdated` in a small TOML file under the platform-specific state directory:

| Platform | Path |
|----------|------|
| Linux | `$XDG_STATE_HOME/cartog/state.toml` (typically `~/.local/state/cartog/state.toml`) |
| macOS | `~/Library/Application Support/cartog/state.toml` |
| Windows | `%LOCALAPPDATA%\cartog\state.toml` |

When a deferred update is armed (`--defer`), cartog also writes a `[pending_update]` table:

```toml
[pending_update]
target_version = "0.20.0"   # the version --apply-pending will install
armed_from = "0.19.0"        # the version that armed it (stale detection)
armed_at = "2026-05-29T10:00:00Z"
```

`--apply-pending` clears this table after a successful swap (or when it finds the intent stale — already at or past the target). `cartog self version --json` echoes it as a `pending_update` field so the plugin's SessionStart hook can report it.

The file is best-effort: if it is missing, malformed, or unwritable, cartog falls back to defaults and continues. Safe to delete; it will be recreated on the next check.

PID files for `cartog serve` and `cartog watch` live in the same directory. Slot names are DB-scoped — `serve-<hash>.pid` / `watch-<hash>.pid` — where `<hash>` is a 16-char SHA-256 prefix of the canonical DB path. Two cartog peers on different projects therefore claim different files and coexist; `cartog self update` still detects any running peer regardless of scope.

## Troubleshooting

Update failures map to the exit codes above. For peer-lock refusals ("another
cartog process is running"), stale PID files, checksum mismatches, and
`cargo install` users, see [troubleshooting.md](troubleshooting.md).
