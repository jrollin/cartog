# Updating cartog

> The first install is always [`install.sh`](https://github.com/jrollin/cartog#install) or `cargo install cartog`. Once cartog is on your `PATH`, the steady state is managed by the `cartog self` command group described below.

## Quick reference

| Command | What it does |
|---------|--------------|
| `cartog self update` | Upgrade in place to the latest stable release |
| `cartog self update --check` | Check whether an update exists; do nothing else |
| `cartog self version` | Print version, target triple, install source, last check timestamp |
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

## Inspect the installation

```bash
cartog self version
cartog self version --json
```

Reports the bare semver, target triple (e.g. `aarch64-apple-darwin`), install source, and the timestamp of the last successful update check (`never` if none).

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

The file is best-effort: if it is missing, malformed, or unwritable, cartog falls back to defaults and continues. Safe to delete; it will be recreated on the next check.

PID files for `cartog serve` and `cartog watch` live in the same directory. Slot names are DB-scoped — `serve-<hash>.pid` / `watch-<hash>.pid` — where `<hash>` is a 16-char SHA-256 prefix of the canonical DB path. Two cartog peers on different projects therefore claim different files and coexist; `cartog self update` still detects any running peer regardless of scope.

## Troubleshooting

### "another cartog process is running"

The upgrade refused because a peer `cartog serve` or `cartog watch` is holding the binary open. Stop the named process and re-run `cartog self update`. The error message includes the slot (`serve-<hash>` / `watch-<hash>`, where `<hash>` is a SHA-256 prefix of the peer's DB path) and PID.

### Stale PID files

If a `cartog serve`/`watch` was killed with `SIGKILL` (or the machine crashed), the PID file may remain after the process is gone. Cartog reaps stale entries automatically: every `cartog serve` / `cartog watch` startup runs a sweep that unlinks every `.pid` file whose recorded process has exited (not just the slot being claimed), and `cartog self update` also clears them. Manual cleanup is rarely needed; to clear by hand: `rm $XDG_STATE_HOME/cartog/{serve,watch}-*.pid` (Linux paths; substitute the platform-specific state dir).

### "checksum mismatch"

Exit `4` means the downloaded tarball's SHA256 did not match the release's `SHA256SUMS`. cartog aborts before touching any files. Common causes: a captive-portal proxy modifying the response, a corrupted CDN cache. Retry; if it persists, [open an issue](https://github.com/jrollin/cartog/issues).

### `cargo install cartog` users

`cartog self update` refuses to overwrite cargo-managed binaries on purpose — `cargo install` expects to be the only writer of files in `$CARGO_HOME/bin`. Use `cargo install cartog --force`.
