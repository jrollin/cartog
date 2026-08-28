# Changelog

## [0.33.0] - 2026-08-28

### Bug Fixes

- **deps**: Bump h2 to resolve RUSTSEC-2026-0258 (#161) ([`1687822`](https://github.com/jrollin/cartog/commit/16878222f6ad27b1a3dd6bd58fbac7385329afc0))
- **site**: Npm audit fix — resolve 6 of 10 advisories, lockfile only (#162) ([`ca028b9`](https://github.com/jrollin/cartog/commit/ca028b90bba3c2837e3b455595ab26bf76ffb103))
- **mcp**: Drop unused `rmcp::model::*` glob imports (#163) ([`581d408`](https://github.com/jrollin/cartog/commit/581d4083316d2a8622ebc2bfa302415cddb9bcde))
- **config**: Honor `[reranker] enabled`, reject unknown keys per section (#169) ([`3e2950a`](https://github.com/jrollin/cartog/commit/3e2950a29af6f4ff18683059c23a35ed6b9d67b0))
- **vscode**: Npm audit fix — clear the 3 remaining high advisories (#173) ([`b056be8`](https://github.com/jrollin/cartog/commit/b056be86b21ea1234cdeaa603fc85db6ad78be4a))
- **config**: Derive index consent from file presence, not parse success (#175) ([`3d11f37`](https://github.com/jrollin/cartog/commit/3d11f37cef38257881b38b64f1e6bf7bb2afc6d6))
- Config-salvage convergence, degraded banner, reranker lock placement, consent unification (#177) ([`573374b`](https://github.com/jrollin/cartog/commit/573374bde664d77ddb46cbd1aef7c1632d8125c1))

### Build

- **deps**: Bump astro and astro-pagefind in /site (#168) ([`557a4cb`](https://github.com/jrollin/cartog/commit/557a4cbfb9ed4318acf6d6f8fa9a995003f1616d))
- **deps**: Bump undici from 7.28.0 to 7.29.0 in /editors/vscode (#164) ([`1c25c7a`](https://github.com/jrollin/cartog/commit/1c25c7a510d80b336f7865fd85507248747d7829))
- **deps**: Bump js-yaml from 4.2.0 to 4.3.2 in /editors/vscode (#165) ([`a6fed85`](https://github.com/jrollin/cartog/commit/a6fed8547974bea16a947535821db34cd13d32ed))

### Documentation

- Finish the `[reranker] enabled` sync and clear a doc-link warning (#176) ([`b67215a`](https://github.com/jrollin/cartog/commit/b67215a3c91355a7be77e4a82ca10edccf393296))

### Performance

- **mcp**: Build the cross-encoder on first query, not at server start (#174) ([`bf19c43`](https://github.com/jrollin/cartog/commit/bf19c434f92db625e8e758fdec406a233b26694b))

## [0.32.2] - 2026-08-05

### Bug Fixes

- **self-update**: Size the peer wait by which lock is blocking ([`83fb13b`](https://github.com/jrollin/cartog/commit/83fb13bbc40628a4302e884a16420dc646199c5b))
- **self-update**: Name a foreign lock in the exit-6 diagnostic ([`3ff9b2b`](https://github.com/jrollin/cartog/commit/3ff9b2b7f4af3fc817bb3cfb9c2e3af791ab338c))

## [0.32.1] - 2026-07-26

### Bug Fixes

- **vscode**: Run init before index so a fresh repo can be set up ([`28ec4f9`](https://github.com/jrollin/cartog/commit/28ec4f99402d79b07d4d5155523066951814b07a))

### Miscellaneous

- **vscode**: Generate the extension changelog on release ([`c6a3a1e`](https://github.com/jrollin/cartog/commit/c6a3a1ecf2b0a51f56cdb55d19ccc0d813f0629f))

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
