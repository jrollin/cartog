# cartog self — Exit Codes, State, and Environment Variables

> Full procedural guide: [../how-to/update-cartog.md](../how-to/update-cartog.md) | Full reference: [../updates.md](../updates.md)

## Exit codes — `cartog self update`

| Code | Meaning |
|------|---------|
| `0` | Up to date (or upgrade succeeded) |
| `1` | Update available (only with `--check`) |
| `2` | Network or parse error reaching `api.github.com` |
| `3` | Refused — binary was installed via `cargo install`. Run `cargo install cartog --force` instead |
| `4` | Checksum mismatch; no filesystem mutation, original binary intact |
| `5` | New binary failed smoke test; previous binary restored |
| `6` | A peer `cartog serve`/`watch` is running |

## Exit codes — `--defer`

| Code | Meaning |
|------|---------|
| `0` | Armed (or already up to date — nothing to arm) |
| `2` | Network or parse error reaching `api.github.com` |
| `3` | Refused — binary was installed via `cargo install` |
| `5` | State write failed; the intent was not persisted |

`--defer` never returns `6` — arming under a live peer is the whole point.

## Exit codes — `--apply-pending`

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

## Environment variables

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
