# Changelog

## [0.32.0] - 2026-07-26

### Bug Fixes

- **lang**: Extract nested C enums and C++ function-pointer members
- **lang**: Rust enum variants inherit the enum's visibility
- **rag**: Stop test bodies crowding out code in semantic search

### Features

- **lang**: Add C and C++ support (17th and 18th languages)
- **lang**: Add EnumMember and Macro kinds, fix extraction gaps
- Detect test code and demote it in search ranking

## [0.30.1] - 2026-06-26

### Added

- Register cartog as an MCP server provider for GitHub Copilot
  (`mcpServerDefinitionProviders`), spawning `cartog serve --watch` with no
  config file required.
- Auto-detect the `cartog` binary (`cartog.binaryPath` override →
  `$CARTOG_INSTALL_DIR` → `PATH` → `~/.local/bin` → `~/.cargo/bin`).
- `cartog.watch` setting to toggle the `--watch` flag.
- Actionable prompt when the binary is missing (install instructions / set path).
- **Install cartog** action (macOS/Linux desktop) that runs the version-pinned
  installer in a terminal — typed but not run, so you review the `curl … | sh`
  line before pressing Enter.
- **cartog: Recheck** command to re-detect the binary after an install and
  register the server; offers to index the repo when found.
