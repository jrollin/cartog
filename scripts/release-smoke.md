# Release smoke checklist

Manual checks the maintainer runs before tagging a plugin release. Covers
properties the bash harness (`make check-skill`) can't observe: real
Claude Code TUI behavior, marketplace install flow, MCP startup UX.

Run from a machine (or VM/container) where cartog is **not** already
installed. Allow ~10 minutes.

## 1. Cold marketplace add

Goal: confirm the plugin install + first launch does not freeze the TUI.

1. Start with no cartog binary on `PATH`:
   ```bash
   command -v cartog && rm -f "$(command -v cartog)"
   ```
2. In Claude Code, run:
   ```
   /plugin marketplace add jrollin/cartog
   /plugin install cartog@cartog-plugins
   ```
3. Launch a new Claude Code session in any project.

Expected:
- TUI prompt appears in **under 2 seconds**.
- A visible one-line notice mentions either background install or
  `/cartog-install`.
- No spinner / no hang.

If the prompt takes more than 2 seconds, the SessionStart hook regressed —
something is running in the foreground that shouldn't be.

## 2. First-session MCP miss is graceful

Goal: confirm the user gets a helpful error path when MCP isn't ready.

In the same first session as step 1:

1. Ask a code-graph question (e.g. "what does this project do?").

Expected:
- The agent does **not** call cartog MCP tools (binary still installing).
- The agent suggests `/cartog-install` or explains cartog tools are not
  yet available.
- No hard error in the TUI.

## 3. Session 2 has working MCP

Goal: confirm the background install completed and MCP picks up the binary.

1. Wait ~30 seconds for the background install to finish (watch
   `~/.cache/cartog/session.log` if you want to see it land).
2. Restart Claude Code.
3. Ask the same code-graph question.

Expected:
- cartog MCP server starts cleanly (no error in TUI).
- Agent answers using cartog tools (visible in tool calls).
- `cartog --version` on the command line reports the plugin's pinned
  version.

## 4. `/cartog-install` upgrades a drifted binary

Goal: confirm the explicit upgrade verb works.

1. Manually downgrade the binary. Pick the target slug that matches your
   platform from this table (the published artifact suffixes — adapt if
   release.yml ever changes them):

   | OS / arch | Target slug |
   |---|---|
   | macOS Apple Silicon | `aarch64-apple-darwin` |
   | macOS Intel | `x86_64-apple-darwin` |
   | Linux x86_64 (glibc) | `x86_64-unknown-linux-gnu` |
   | Linux ARM64 (glibc) | `aarch64-unknown-linux-gnu` |

   Then:
   ```bash
   TARGET=<paste-slug-from-table>
   curl -fsSL "https://github.com/jrollin/cartog/releases/download/v0.17.0/cartog-${TARGET}.tar.gz" \
     | tar xz -C "$(dirname "$(command -v cartog)")"
   ```
2. Confirm: `cartog --version` shows the older version.
3. In Claude Code, type `/cartog-install`.

Expected:
- The skill runs `cartog self update` (or `install.sh` for <0.14 binaries).
- `cartog --version` after the skill finishes shows the plugin's pinned
  version.
- The skill tells the user to restart Claude Code to pick up the new binary.

## 5. `/cartog-install` on a cargo-installed binary

Goal: confirm the cargo cohort gets a clear, non-destructive message.

1. Install via cargo:
   ```bash
   cargo install cartog --force
   ```
2. In Claude Code, type `/cartog-install`.

Expected:
- The skill detects the cargo install (exit code 3 from
  `cartog self update`).
- The skill prints a clear instruction: `cargo install cartog --force`
  to upgrade.
- The binary is **not** swapped out from under the cargo install.

## 6. Pre-0.14 cohort upgrade path

Goal: confirm the transitional SessionEnd hook still rescues legacy users.

1. Install an old binary. Use the target slug table from step 4 above:
   ```bash
   TARGET=<paste-slug-from-table>
   curl -fsSL "https://github.com/jrollin/cartog/releases/download/v0.13.5/cartog-${TARGET}.tar.gz" \
     | tar xz -C ~/.local/bin
   ```
   Confirm: `cartog --version` shows `0.13.5`.
2. Launch Claude Code.
3. Exit Claude Code cleanly (e.g. `/exit` or close the window).
4. Wait for the SessionEnd hook to log to `~/.cache/cartog/session.log`.

Expected:
- `session.log` contains a line:
  `Upgrading pre-self-update cartog 0.13.5 → <plugin-version> via install.sh...`
- `cartog --version` now shows the plugin's pinned version.

Note: this hook is **transitional** and will be removed in a future
release. After removal, pre-0.14 users will be told to reinstall via
`/cartog-install`.

## After all checks pass

- Tag the release: `./scripts/release.sh patch|minor|major`
- CI runs in order: build the binaries, create the GitHub release with its
  assets, then publish to crates.io and the VS Code marketplaces. The two
  publish jobs are gated on the release so neither advertises a version whose
  tarballs are not yet downloadable.
