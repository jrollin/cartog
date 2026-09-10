# Changelog

## [0.34.0] - 2026-09-10

### Bug Fixes

- **registry**: Review findings — leaking test, forget side effect, subdir naming ([`71fc1ec`](https://github.com/jrollin/cartog/commit/71fc1ecd1ceb4e5b81c6b6092f3671c05ee69274))
- **projects**: Say "no registry" rather than "could not be opened" ([`3c05313`](https://github.com/jrollin/cartog/commit/3c0531348a1b93f22ac9c75ef90449081d6774d5))
- **test**: Remove three load-induced flakes, one of them a real bug ([`dd7626e`](https://github.com/jrollin/cartog/commit/dd7626e1715c0bce4801371b2a7ee97b8c3815aa))
- **registry**: Second review round — four real bugs plus consistency ([`51d4624`](https://github.com/jrollin/cartog/commit/51d4624495830b76c444ead62c25ab5704830d64))
- **search**: Keep unreadable/elided diagnostics when no project matched ([`9b38205`](https://github.com/jrollin/cartog/commit/9b38205ebb2a3be77c3e49892d5ac8a71226992e))
- **test**: Deflake the foreign-peer apply bound by discarding a cold spawn ([`33828c3`](https://github.com/jrollin/cartog/commit/33828c31fa2ca101f56eae7b47c5cfc1169a3f44))
- **registry**: Keep markup inside a code span literal ([`802340e`](https://github.com/jrollin/cartog/commit/802340ebd317eb456e1c729b2388a35528406d3f))
- **mcp**: Do not report an unsearched fan-out as a no-match ([`13882f1`](https://github.com/jrollin/cartog/commit/13882f10ebb100b168497a7c0cd09095a16e0d18))
- **site,mcp**: Whole-tree deploy checkout, envelope budget, multi-tick spans ([`01d3453`](https://github.com/jrollin/cartog/commit/01d345378349b9325769a2627bb015bc3b0a3b9a))
- **registry**: Three README-parsing bugs that lost the description ([`df2cd96`](https://github.com/jrollin/cartog/commit/df2cd963863f651311f52e4cb627981fd4f8a7cd))
- **registry**: Keep quarantine on the write path, and stop overselling it ([`7001c9f`](https://github.com/jrollin/cartog/commit/7001c9f24d7f1f29958e247824584f63ce3917de))
- **registry**: Merge a duplicate row's counts before dropping it ([`2052a6a`](https://github.com/jrollin/cartog/commit/2052a6a1a3ef2ca4574d8c04d12a5f40a304b287))
- **registry**: Complete the void-element list, and unskip the drift tests ([`abdf83d`](https://github.com/jrollin/cartog/commit/abdf83d7646ed4cb7497b0103659004ece197589))
- **search**: Expand a bare `~`, and delete the unreachable cap branch ([`acebac1`](https://github.com/jrollin/cartog/commit/acebac151234b05ef6fe49fee9e7ef9fddb55d8f))
- **search**: Stop blaming the filter, and expand `~` by path component ([`bdfa483`](https://github.com/jrollin/cartog/commit/bdfa483c306a96f4181acbda596017b06dc52104))
- **registry**: Infer a description from a blockquote tagline ([`221d8cc`](https://github.com/jrollin/cartog/commit/221d8cc275c63e3c826a4cea2d2b659c95554639))
- **registry**: End a rejected quote's skip at the next block, not the blank line ([`f8cbd4e`](https://github.com/jrollin/cartog/commit/f8cbd4ecdf5812285bceee4c5a39eb7eadca6186))
- **mcp**: Let the server see the plugin pin, and surface drift once ([`002b47a`](https://github.com/jrollin/cartog/commit/002b47ab08b220957d0dee67631ab7d82171dcd5))
- **hooks**: Converge a drifted binary, and tell the user once ([`54bef6c`](https://github.com/jrollin/cartog/commit/54bef6c702dd63585298a47ce21180115b6840a8))
- **doctor**: Point cargo installs at the command that works ([`5ee3242`](https://github.com/jrollin/cartog/commit/5ee32424837625e0b77e85741c491a7d1257dfc6))
- **hooks**: Address review, and stop the notice racing its own arm ([`bb2ead8`](https://github.com/jrollin/cartog/commit/bb2ead8a12570086440a571037b4dfcd921681f2))

### Documentation

- **agents**: Record the site contract and the one-page decision ([`b767ce3`](https://github.com/jrollin/cartog/commit/b767ce34d891934eb3e4b528649de65058f8bde6))
- **mcp**: Name both opt-in cross-project tools consistently ([`9ed9ac9`](https://github.com/jrollin/cartog/commit/9ed9ac9850fff99fe471af899e63a7eed324a929))
- **readme**: Index the two docs missing from the Diataxis tree ([`f4adaeb`](https://github.com/jrollin/cartog/commit/f4adaeb9ceddc3965b598a3de2769664d12d3bc3))
- Describe the drift notice and the startup arm ([`ab0f9ad`](https://github.com/jrollin/cartog/commit/ab0f9ad551f945df4e145503b279b44f66425a45))

### Features

- **doctor**: Add version, paths, and lsp checks (#178) ([`e838974`](https://github.com/jrollin/cartog/commit/e8389747fce778e0893fe54bbf7b7d975d069dad))
- **languages**: Whole-file component symbol for SFCs + React fixture (#179) ([`6fc3086`](https://github.com/jrollin/cartog/commit/6fc30862b248600d6484a43466288f47d736e5e8))
- **db**: Expose the embedding fingerprint metadata keys ([`8354c69`](https://github.com/jrollin/cartog/commit/8354c691b690a65c2bf09d78c6e5ec773cb529a5))
- **registry**: Projects.sqlite, write hooks, and cartog projects ([`7d17a80`](https://github.com/jrollin/cartog/commit/7d17a8060035c6bfe54d3ba779dfbcc3580a6f67))
- **mcp**: Cartog_list_projects, the 17th MCP tool ([`ccc8852`](https://github.com/jrollin/cartog/commit/ccc8852dfedd67efab332b7fdc4b95f41d341fb0))
- **registry**: Self-populated project description via [project] config and README ([`21fdffa`](https://github.com/jrollin/cartog/commit/21fdffac2a51602de7d8fe28d3d8cd099b85fba4))
- **projects**: Backfill an existing index with `projects add` and `scan` ([`f9c9b32`](https://github.com/jrollin/cartog/commit/f9c9b32c8623adc2c058e9ca90a68eca013d7a1a))
- **search**: Federated symbol search across the machine's other projects ([`60c6c54`](https://github.com/jrollin/cartog/commit/60c6c54ff7e05b590797eeb899835e16a2dae7ae))
- **mcp**: Make the cross-project tools opt-in via --federated ([`6a9f344`](https://github.com/jrollin/cartog/commit/6a9f34402350e825996d7c9ced1deace467ded67))
- **site**: Lead with the mechanism, and serve the docs to agents ([`96eff27`](https://github.com/jrollin/cartog/commit/96eff273b3872726e225aac365d3f51e7ba26c17))

### Refactor

- **registry**: Extract state helpers into cartog-registry ([`8921441`](https://github.com/jrollin/cartog/commit/8921441b136b9a8708554d02b8bb5ac5279d24d6))
- **docs**: Replace employer-shaped example names with generic ones ([`bd62d23`](https://github.com/jrollin/cartog/commit/bd62d239824b1e192165d7d5f6ab67fa10466c88))

### Testing

- **search**: Cover the zero-candidate fan-out path end to end ([`dc5c4e0`](https://github.com/jrollin/cartog/commit/dc5c4e0a2657363c6a79f0fcc0b7ad828dd0ac20))

## [0.33.0] - 2026-08-28

### Bug Fixes

- **languages**: Assert the parse-cancel mechanism, not a grammar's slowness ([`d7d9429`](https://github.com/jrollin/cartog/commit/d7d942913895312c3240eec78cc0fa8d042a295d))

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
