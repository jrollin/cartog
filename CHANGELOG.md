# Changelog

## [0.16.2] - 2026-05-19

### Bug Fixes

- Remove redundant clippy-flagged test assertion and enforce fmt/clippy in AGENTS.md ([`203825b`](https://github.com/jrollin/cartog/commit/203825bd6e1fe4ec3816231d92477c3eab56e5d9))
- Format error ([`515cd67`](https://github.com/jrollin/cartog/commit/515cd67f687b90c150d5c7ce451d7310f3ac4111))
- **extract**: Capture calls in package-level var/const initializers ([`3735bd6`](https://github.com/jrollin/cartog/commit/3735bd6e38cfd439ff8cfc0d114f98a672679043))
- **search**: Rank definitions above variables/imports in search results ([`bdfa966`](https://github.com/jrollin/cartog/commit/bdfa966ac8e7fe51d5fd029ec69860bed280622e))
- **ci**: Resolve gitleaks false positive on fixture fake API key ([`9ef398d`](https://github.com/jrollin/cartog/commit/9ef398dec824bfed4ead338d79ef53a9a99ca6d3))
- **ci**: Remove deprecated os and use cross binaries ([`5804ed5`](https://github.com/jrollin/cartog/commit/5804ed55a1a5137abda4b8df00ae90cb88722392))
- **skill**: Add "show me" trigger patterns to cartog skill description ([`fe7020f`](https://github.com/jrollin/cartog/commit/fe7020f18285d3f5902f5b79d73d37e263967e67))
- Update quinn-proto 0.11.13 -> 0.11.14 (RUSTSEC-2026-0037 DoS) ([`68b0399`](https://github.com/jrollin/cartog/commit/68b039901aa99f04e212341610c573e6d3fe7973))
- Do exceed limit description ([`ecd612d`](https://github.com/jrollin/cartog/commit/ecd612d55a44a7587350ea1c2a0a9147196b7d2d))
- Address review findings — chunking, CTE, migration logging ([`3970e5c`](https://github.com/jrollin/cartog/commit/3970e5c69a626c17866ac6027af9c308a9ec17c3))
- Embedding chunking review — UTF-8 safety, false positive filters, docs ([`0907fee`](https://github.com/jrollin/cartog/commit/0907fee98854aab82f0e6d8b166eb1faa42abe1a))
- **plugin**: Restore marketplace.json with distinct name ([`c477286`](https://github.com/jrollin/cartog/commit/c477286cad93682db8bd4bf6c65702693ab0fe7c))
- Let llm find skill direcotry ([`1505530`](https://github.com/jrollin/cartog/commit/15055309b8c1cfd56f55edd182d6927e9d3461b3))
- **site**: Restore feature-card transition, fix hover border-top override, revert unauthorized padding/radius ([`cd99119`](https://github.com/jrollin/cartog/commit/cd991197402ae8a33ff6c2eaea4d22f4736c50e1))
- **site**: Remove dead stats CSS rules, remove redundant install-options grid override ([`5f2078a`](https://github.com/jrollin/cartog/commit/5f2078a3e06ac73dcded3b3b5ed57d0c13036545))
- **workspace**: Mark library crates as publish = false, fix cargo-deny policy ([`53ae662`](https://github.com/jrollin/cartog/commit/53ae662d636eeea97676f008492fbf972c973844))
- **fixtures**: Add standalone workspace to Rust fixture ([`072bf1c`](https://github.com/jrollin/cartog/commit/072bf1c469fa725770215f4c596aa8d20a8ec66c))
- **publish**: Add version to workspace deps and fix release pipeline ([`4b516dd`](https://github.com/jrollin/cartog/commit/4b516dd81ae0023139bc04e65fc3e8afc6aa71a8))
- **publish**: Publish all workspace crates in dependency order ([`a724b96`](https://github.com/jrollin/cartog/commit/a724b9662cf48581dbad30b1324310e7a11ce834))
- **publish**: Enable crates.io publishing for all workspace crates ([`f453017`](https://github.com/jrollin/cartog/commit/f453017c6823cc3cc0637f58480ae1560ae8edee))
- **publish**: Increase delay between crate publishes to 60s ([`a260010`](https://github.com/jrollin/cartog/commit/a260010dd069bad7b90eaf618707ef057138802a))
- **ci**: Upgrade GitHub Actions to Node.js 24 compatible versions ([`776bb2d`](https://github.com/jrollin/cartog/commit/776bb2d505ffd8acc592822caeadeda0d4bb23a2))
- **rag**: Prevent silent vector index wipe and cache providers ([`51aad71`](https://github.com/jrollin/cartog/commit/51aad71a7a8d470911cdd517c2a88560dbd65732))
- **db**: Preserve stored embedding dimension when caller passes default ([`9667a15`](https://github.com/jrollin/cartog/commit/9667a15326787b97f762b3d1fbbf1ba69fafaf98))
- **site**: Correct stale #compare nav anchor to #why ([`2c9576f`](https://github.com/jrollin/cartog/commit/2c9576f8ed05f31e762fbaff202fb9eacd941aaf))
- **db**: Correct column mapping in search() and refs() queries ([`8f6f7ae`](https://github.com/jrollin/cartog/commit/8f6f7ae2402950457b484f97ce7822df9356f2c6))
- **cli**: Handle --kind all in search and changes commands ([`83d3fab`](https://github.com/jrollin/cartog/commit/83d3fab4bad12347de4d131157910bcc30180368))
- **db**: Wrap multi-statement deletes in transactions ([`45aca44`](https://github.com/jrollin/cartog/commit/45aca44f725028bf33a5bb71a5c231f62b331a1a))
- **db**: Add chunking guards and reorder migration ([`8fef1b1`](https://github.com/jrollin/cartog/commit/8fef1b170f09e7e81a605696e33baad10082e157))
- **indexer**: Prevent symlink loops and fix dedup collision bug ([`e5acbc4`](https://github.com/jrollin/cartog/commit/e5acbc41e81e7d2d52c47a1e686846e20bb46c07))
- **lsp**: Detect EOF in read_headers to avoid infinite loop ([`ac0ea59`](https://github.com/jrollin/cartog/commit/ac0ea59ea3e017c8d28842c44793e6269fe1856e))
- **doctor**: Improve robustness of health checks ([`7d3931c`](https://github.com/jrollin/cartog/commit/7d3931cb2ea5962d31081e6d6c04df492b00216e))
- **mcp**: Suppress dead_code warning on tool_router field ([`b123928`](https://github.com/jrollin/cartog/commit/b123928ac496362070d9d6af32c7cb6787200195))
- **plugin**: Remove .mcp.json to avoid duplicate MCP server with plugin ([`1bee808`](https://github.com/jrollin/cartog/commit/1bee8087a71b2b692016b8cf19b4ae7675151cc8))
- **plugin**: Use CLAUDE_PLUGIN_ROOT for hook script path ([`0f91692`](https://github.com/jrollin/cartog/commit/0f9169222f547ad99d9c64709edb6fb7e368da02))
- **plugin**: Remove invalid agents field from manifest ([`a5a6655`](https://github.com/jrollin/cartog/commit/a5a66557cc3f34dfad4bd6d4eb32589bd6dad7be))
- **languages**: Panic on multi-byte UTF-8 in markdown extractor ([`5d731d6`](https://github.com/jrollin/cartog/commit/5d731d60cf1cbce6307b4a60111cd8eeb0571d71))
- **db**: Wrap index_directory Phase 3 in a single transaction ([`e7d854b`](https://github.com/jrollin/cartog/commit/e7d854ba87fb9dc40a00a2411e85c88487eccaf1))
- **db**: Tighten transaction contract docs + e2e rollback test ([`7cf281d`](https://github.com/jrollin/cartog/commit/7cf281da8f0180f5748c85754cccb430759a1fad))
- **plugin**: Use semver compare in update check, not string equality ([`7cea701`](https://github.com/jrollin/cartog/commit/7cea70192de0dd4c664e56514d16556fdd866143))
- **self-update**: Harden upgrade flow against post-swap edge cases ([`81eac1f`](https://github.com/jrollin/cartog/commit/81eac1fa685c4ff18f3ba19e3564e3f43fab028e))
- **release**: Publish cartog-process-lock to crates.io ([`76a2ba1`](https://github.com/jrollin/cartog/commit/76a2ba1e87572290b49a74e48390cc5d20dc650b))
- **test**: Work around Linux ETXTBSY in smoke_test unit tests ([`3596c58`](https://github.com/jrollin/cartog/commit/3596c58eba0286095b9df106b59cf3a329796c73))
- **skill**: Print actionable install guide when cartog is missing ([`8feb09c`](https://github.com/jrollin/cartog/commit/8feb09c4a853391a1abdac4a14216a334411748d))
- **skill**: Auto-install/update cartog binary against plugin version ([`6c5a296`](https://github.com/jrollin/cartog/commit/6c5a296abe0c65fb971e35717644914a407e0e8a))
- **skill**: Defer cartog self update to SessionEnd to avoid peer-running deadlock ([`2e68387`](https://github.com/jrollin/cartog/commit/2e68387d5700b0b2a8a043a41a1af72e25996b43))
- **skill**: Install to ~/.local/bin when ~/.cargo/bin is not on PATH ([`62d3c1c`](https://github.com/jrollin/cartog/commit/62d3c1cd6282e875ff64a74f453ef47e5721514f))
- **self**: Harden migrate-db edge cases ([`27bb9a5`](https://github.com/jrollin/cartog/commit/27bb9a5b2d21d257c3f319863ecc170ef1b6a3d3))
- **db**: Add PRAGMA busy_timeout to prevent SQLITE_BUSY under concurrency ([`aeed052`](https://github.com/jrollin/cartog/commit/aeed0523ffc4269f06c86ae91e03f10f35687041))
- **docs**: Correct factual counts to match the codebase ([`f6936d8`](https://github.com/jrollin/cartog/commit/f6936d8836d26f673e05e0f0b66cf2303e66e69f))
- **site**: Correctness gaps in editor row, scope filter wording, and stats footnoting ([`1ddecad`](https://github.com/jrollin/cartog/commit/1ddecaddc17fb1586e9d50e5fe145d5a0311305c))
- **skill**: Document init, ide, map, and the changes->impact workflow ([`19bdd3a`](https://github.com/jrollin/cartog/commit/19bdd3afe2e7c0b6c2f14d92944cf4371479ce26))
- **skill**: Correctness pass + stress tests for ensure_indexed.sh ([`4f39fd5`](https://github.com/jrollin/cartog/commit/4f39fd5656157af3048138dae64e674d3e36f71f))
- Address CodeRabbit review findings ([`2f8c54c`](https://github.com/jrollin/cartog/commit/2f8c54cd90ae02558a8ca0cc87047f4aeb6cce1b))

### Documentation

- **skill**: Document cartog search command and progressive narrowing workflow ([`693f47e`](https://github.com/jrollin/cartog/commit/693f47ed25fdabdf5c3f024e3ff7659d29136020))
- Rewrite README to lead with benchmarks and add demo GIF ([`01ea332`](https://github.com/jrollin/cartog/commit/01ea332aef13d8211ce55129b84f592689b82fec))
- Consolidate documentation and add conventions ([`2afcb0a`](https://github.com/jrollin/cartog/commit/2afcb0aa9b42313b87138124ec3d3b570fa17cb8))
- Rewrite tech.md with comprehensive design decisions and rationale ([`71d5a90`](https://github.com/jrollin/cartog/commit/71d5a9080094d4724c742d5d7e3e0820ca90eacd))
- Update README, skill, and project docs for LSP feature ([`10eb869`](https://github.com/jrollin/cartog/commit/10eb86912a239fcd31590565da8920d3a0d2ed7f))
- **skill**: Add CLI/MCP mode detection and usage guidance ([`2361676`](https://github.com/jrollin/cartog/commit/236167632a0dc40bd2242735496993205f2d638f))
- Update documentation for incremental reindexing feature ([`2776bb1`](https://github.com/jrollin/cartog/commit/2776bb1558ac66c02695f0a1a13e6cd2dffec316))
- Update documentation for multi-provider embeddings ([`2c09bb8`](https://github.com/jrollin/cartog/commit/2c09bb822739f6afef900105e29d79d8e50eed09))
- **site**: Reorganize landing page and add provider config docs ([`f655c98`](https://github.com/jrollin/cartog/commit/f655c983548eb06e147bc999bc7fbe7b18d6668b))
- **readme**: Broaden positioning and add doc site link ([`173c3ba`](https://github.com/jrollin/cartog/commit/173c3badd95fe6f99d7c3918d44e70954e366901))
- Add marketplace step to plugin install instructions ([`60a32fa`](https://github.com/jrollin/cartog/commit/60a32fa5e7d9fd06bf9b1042a3309e1a91410dd3))
- **doctor**: Add doctor command to skill and site ([`25c4689`](https://github.com/jrollin/cartog/commit/25c46898f68b60e736036a5c94d67923a7a9957f))
- **readme,site**: Rewrite README for broader dev audience, fix stale site content ([`9aa8a9e`](https://github.com/jrollin/cartog/commit/9aa8a9e89b54235fcfb860df990a9c6b8769cb7a))
- Split plugin install into separate steps ([`d6279d8`](https://github.com/jrollin/cartog/commit/d6279d87f0ab4b3cf76ddeca193ad99594e92c88))
- **structure**: Add agents/ and .claude-plugin/ to project tree ([`cf4f161`](https://github.com/jrollin/cartog/commit/cf4f161b9e64d848ccfea0ac7bf7e212c2cbc6c8))
- Add editor-integration page (Neovim / VS Code / Emacs / Zed) (#30) ([`86b94de`](https://github.com/jrollin/cartog/commit/86b94de71ae72a97564d7caaeca264c1c88a6ec2))
- Fix debounce default, add LSP recall caveat, add docs index ([`8a23aaa`](https://github.com/jrollin/cartog/commit/8a23aaaefa8d49c97fd376c53a385b044473f666))
- Sync post-lsp-default-sweep across README, docs, site, skill ([`e20d523`](https://github.com/jrollin/cartog/commit/e20d523f302877919d095d8ceb1d6a5a5a7ca87e))
- **architecture**: Add layered incremental-indexing design doc ([`0494056`](https://github.com/jrollin/cartog/commit/0494056396b2a3997f2c42c6f4b47fb39bcf6f03))
- **release**: Point publish step env back to job-level NOTE ([`63ff2c7`](https://github.com/jrollin/cartog/commit/63ff2c76f3f2e0735e3d2819d3dda8e25db2a73b))
- **self-update**: Add user guide for cartog self commands ([`1894b39`](https://github.com/jrollin/cartog/commit/1894b39b7e9d2de42f1665cebd846c9c56d2b59c))
- **self-update**: Align all docs with the new feature surface ([`5dbe968`](https://github.com/jrollin/cartog/commit/5dbe96869d1a71fe4655c31e01ae97056b4d0500))
- **site**: Add `cartog self` to landing page and CLI reference ([`2a65664`](https://github.com/jrollin/cartog/commit/2a656647064241c0d53318653893fc9873e55965))
- **readme**: Add contributors section (#39) ([`cf1008d`](https://github.com/jrollin/cartog/commit/cf1008d7cf5577d77912e2f628d78fcdeeb5ecc1))
- Sync README + usage to two-step bootstrap flow ([`bb12d61`](https://github.com/jrollin/cartog/commit/bb12d61ecef46cf54fd3dfc023730e2ef04ecc66))

### Features

- **perf**: Optimize treesitter parser and sql ([`4746d07`](https://github.com/jrollin/cartog/commit/4746d07e35be8a0e5c8dfbce25a4948c80348b9f))
- Add RAG semantic search, file watcher, and smart search routing ([`ad92a18`](https://github.com/jrollin/cartog/commit/ad92a18ac4685813d0a1424dbbf4dddb983850f6))
- Add java lang support ([`c0c3cc0`](https://github.com/jrollin/cartog/commit/c0c3cc08141bb48a67237f891621594633d482bc))
- Add information about model download on first time ([`3697cb1`](https://github.com/jrollin/cartog/commit/3697cb194db583151e205c13ac0ac85d7f0d75ee))
- Improve AST navigation with query API, richer types, and better edge resolution (#2) ([`e0d8039`](https://github.com/jrollin/cartog/commit/e0d80390a5d08441a563395d9afe54f9c420cb43))
- Add Claude plugin manifest ([`714c29b`](https://github.com/jrollin/cartog/commit/714c29bc5d92d74e9f50e5bcd1d94364b3bb3eab))
- Batch symbol lookup, Rust visibility precision, and docs updates ([`69b52d7`](https://github.com/jrollin/cartog/commit/69b52d7b7fffd00decaf3969dc3bf418fedd3800))
- Add --tokens budget flag and cartog changes command ([`0786420`](https://github.com/jrollin/cartog/commit/0786420558ebe26fd67489b10ab536d09f6fa41b))
- Add in-degree centrality ranking and cartog map command ([`be62b00`](https://github.com/jrollin/cartog/commit/be62b001d3f48910bcabd0ce16d696582e96d946))
- AST-aware embedding chunks with auto-versioned re-embed ([`e6de91d`](https://github.com/jrollin/cartog/commit/e6de91ddca50773e523b8de1e4a1feb10a064e38))
- **rag**: Skip imports from embedding, sort batches by length, bump format v3 ([`de9dce3`](https://github.com/jrollin/cartog/commit/de9dce336ace4801ab0b97dbbdb92e6910ced7a8))
- **skill**: Add version check and version-aware install (#3) ([`0542ff0`](https://github.com/jrollin/cartog/commit/0542ff094463ceb6b9b98696af4881cc3fc0d9ad))
- **lsp**: Add LSP-based edge resolution with persistent MCP server support ([`83e7cbc`](https://github.com/jrollin/cartog/commit/83e7cbcd811f6fb89c2d6fb08d843e2bac0d777b))
- **site**: Add landing page and docs for GitHub Pages ([`3b4e9ef`](https://github.com/jrollin/cartog/commit/3b4e9ef75324c847f051f0e40f591ee80838eccb))
- **config**: Configurable database path via TOML config, --db flag, and env var ([`69bcb06`](https://github.com/jrollin/cartog/commit/69bcb067293cc8dad9262c08c23ffe110d1c79a5))
- **index**: Incremental AST reindexing with stable IDs and Merkle-tree diffing ([`2a9c3fe`](https://github.com/jrollin/cartog/commit/2a9c3fe1bcfda8c9db778d8d48940046e0017ca1))
- **site**: Rewrite hero — eyebrow, benefit-first h1, grouped install widget, CTA buttons ([`464f718`](https://github.com/jrollin/cartog/commit/464f71889c5474b189128790d571ce7fe6f0854f))
- **site**: Replace stats section with compact stats bar below hero ([`ecbb769`](https://github.com/jrollin/cartog/commit/ecbb769fee88e07d87fd6520b531a98ed062a148))
- **site**: Add How It Works 3-step section ([`a59835c`](https://github.com/jrollin/cartog/commit/a59835c89bfe68de6e14e3abd4b31c1af1deccbe))
- **site**: Update features heading, add npx skills to install section, bump version to v0.8.1 ([`ca7ca2c`](https://github.com/jrollin/cartog/commit/ca7ca2c4f02c2582cd07252ec3e3f53bbf92cac3))
- **site**: Add live GitHub stars fetch with silent fallback ([`6865a76`](https://github.com/jrollin/cartog/commit/6865a769741499a5bbc7e5b8923289c4f0f79914))
- **site**: Redesign landing page with visual polish and CI optimization ([`3d4d2ae`](https://github.com/jrollin/cartog/commit/3d4d2aeb3d065e35861a5bfc2287ba0c71e4dd8b))
- **rag**: Add Markdown document support with code-only default ([`7f3b234`](https://github.com/jrollin/cartog/commit/7f3b234fee72bf6fc0e9fb94de95045c64754033))
- **rag**: Pluggable embedding providers with TOML configuration ([`d6835b7`](https://github.com/jrollin/cartog/commit/d6835b771f776bac5cc028c1de2c0bbac1cbf68d))
- Add `config` command to display current configuration ([`e419cd5`](https://github.com/jrollin/cartog/commit/e419cd505aa0601c814fcc6bc498b15346973a48))
- **mcp**: Add mcp config in plugin ([`562a2cd`](https://github.com/jrollin/cartog/commit/562a2cdb04942c7d776ca01f528c34c78315459a))
- **mcp**: Improve tool discoverability and agent guidance ([`3a52040`](https://github.com/jrollin/cartog/commit/3a52040ca01fa58c2147476e1c4ce86904dabf63))
- **plugin**: Use ensure_indexed.sh as SessionStart hook ([`b1994f8`](https://github.com/jrollin/cartog/commit/b1994f8dc94e5332008189ab2f9e4ed4196ab541))
- **cli**: Add doctor command to validate environment ([`3e6f00d`](https://github.com/jrollin/cartog/commit/3e6f00d3983858c416cc0d1b60135870cb51a5d6))
- **release**: Enable lsp feature and add binary smoke test ([`62c7b76`](https://github.com/jrollin/cartog/commit/62c7b76f7980368cbf4cf7a611bb0a9f40b86a64))
- **agent**: Add codebase-onboarding agent ([`6b4fc74`](https://github.com/jrollin/cartog/commit/6b4fc744e4b9c15c9fc5d3d0ba3ed7049df9254d))
- **agent**: Add refactoring-scout agent ([`46bdb71`](https://github.com/jrollin/cartog/commit/46bdb7141113e11b36930762f60628b61e26733a))
- **cli**: Cartog manpage subcommand + release artifact (#19) ([`199f0ee`](https://github.com/jrollin/cartog/commit/199f0ee94cc8d367a440f9b1f60e1cf24cbee213))
- **rag**: User-tunable retrieval + rerank knobs via .cartog.toml (#20) ([`32beae8`](https://github.com/jrollin/cartog/commit/32beae86d603c9ae6a8f77dabd16e73a4e8bb2c7))
- **mcp**: Cap tool-response size with overflow hint (#22) ([`694d123`](https://github.com/jrollin/cartog/commit/694d1233bca097fcf42ad39fa9d6c037379458db))
- **db**: Typed DbError for the open + migration path (#23) ([`6ae11d6`](https://github.com/jrollin/cartog/commit/6ae11d67cb7085595b36911e04bbcd46ca1fb464))
- **rag,cli**: Lazy reranker loading + [rag] section in cartog config (#25) ([`1208a9f`](https://github.com/jrollin/cartog/commit/1208a9fc3b476334678e7f01586d63bc49d6734c))
- **cli**: Cartog --version prints build SHA + features (#26) ([`06986bb`](https://github.com/jrollin/cartog/commit/06986bb5f7a201a7fae085c193ce48d943ed02e6))
- **watch**: Cartog watch --json streams NDJSON events (#27) ([`9feeb02`](https://github.com/jrollin/cartog/commit/9feeb028a26de73dbf6a2084c028532c636c98d5))
- **lsp**: Enable lsp feature by default ([`ca06cd8`](https://github.com/jrollin/cartog/commit/ca06cd8e3728ab24fafb38938603bbe8635829f7))
- **self-update**: Bake install source + add state module ([`97dcdda`](https://github.com/jrollin/cartog/commit/97dcddac02950726f32f4e8a257c96309bb86d50))
- **self-update**: Add `cartog self` subcommand surface ([`e1c7c87`](https://github.com/jrollin/cartog/commit/e1c7c87da1e5e925f026037889617cc09c253766))
- **self-update**: Implement `cartog self version` ([`f3028d2`](https://github.com/jrollin/cartog/commit/f3028d24a4715f2405c359df1624cae8df736f30))
- **self-update**: Implement `cartog self update --check` ([`df6f3bb`](https://github.com/jrollin/cartog/commit/df6f3bbcc3aa215ada589e7a98ff82513aefeeec))
- **self-update**: Add PID-file lock module ([`e255c07`](https://github.com/jrollin/cartog/commit/e255c074114cfbec360d9f662879917c95f7e2c7))
- **self-update**: Wire PID files into `cartog serve` and `cartog watch` ([`dabe92c`](https://github.com/jrollin/cartog/commit/dabe92cfa591ee6c8d271e349eff3d407ef882c6))
- **self-update**: Implement `cartog self update` upgrade flow ([`710e8ef`](https://github.com/jrollin/cartog/commit/710e8ef0271f416128907d41a32a19cf3d5393d3))
- **self-update**: Implement `cartog self rollback` ([`1f43dbc`](https://github.com/jrollin/cartog/commit/1f43dbcd6c1b746ee3ea66f243d13445bc6f6064))
- **self-update**: Add `should_check` predicate for auto-check ([`4bc810c`](https://github.com/jrollin/cartog/commit/4bc810cc2b75e29d53351e7456ad1cda93c9069b))
- **self-update**: Add detached background check thread ([`b14c3b4`](https://github.com/jrollin/cartog/commit/b14c3b417898a3f806866aed18ccc40da4c16f15))
- **self-update**: Hook auto-check into post-command epilogue ([`50ec48b`](https://github.com/jrollin/cartog/commit/50ec48b6620645bdc79f983e21ecf61fd11ad39a))
- **release**: Emit SHA256SUMS and bake CARTOG_RELEASE_BUILD ([`44d2a8d`](https://github.com/jrollin/cartog/commit/44d2a8dde04f6fb119800fc718a0b04813400edc))
- **plugin**: SessionStart hook delegates version check to cartog binary ([`3917ee0`](https://github.com/jrollin/cartog/commit/3917ee00a04f03194836f7270f457ba3f8a8b43e))
- **db**: Move default database into .cartog/ directory ([`0130d65`](https://github.com/jrollin/cartog/commit/0130d650f40266bd472e923865051f3c271e1c64))
- **languages**: Add PHP support via tree-sitter-php ([`9468aa0`](https://github.com/jrollin/cartog/commit/9468aa0094fe017153cb02bda207aba0e539ede1))
- **lsp**: Add PHP support via Intelephense/Phpactor (#40) ([`5fe298f`](https://github.com/jrollin/cartog/commit/5fe298f7a158d5918fb81c1fa22d8fd42131b9f3))
- **cli**: Add init and ide for one-step MCP setup ([`c2ff8cd`](https://github.com/jrollin/cartog/commit/c2ff8cd1919be68bb14308451f72fa2cc9373f04))
- **site**: Install script + landing polish for non-Rust onboarding ([`eb13235`](https://github.com/jrollin/cartog/commit/eb1323538e118666014838775a1c6cc3a394d4ec))
- **ide**: Interactive picker with dialoguer ([`d21e9e6`](https://github.com/jrollin/cartog/commit/d21e9e6bed5dd2e3620788c3440e2f68f3c0168f))
- **site**: Landing redesign, brand SVGs, fresh demo ([`e3b79ab`](https://github.com/jrollin/cartog/commit/e3b79abc26f9238d8cd046a7e93eca94915f162a))
- **skill**: Ensure_indexed.sh hints at cartog init on fresh repos ([`7f81194`](https://github.com/jrollin/cartog/commit/7f811947d4b5a1e0e1b08f2e5b54a780ddd1de29))
- **skill**: Defer index when .cartog.toml is missing on TTY sessions ([`4702206`](https://github.com/jrollin/cartog/commit/470220602123f5a7d558870b968deea7f7d5620f))

### Miscellaneous

- Add git-cliff changelog generation ([`15c45ac`](https://github.com/jrollin/cartog/commit/15c45ac49ce453167777491d29eb4b08dac140d7))
- Add changelog link to Cargo.toml ([`ca0a9dc`](https://github.com/jrollin/cartog/commit/ca0a9dc70d4450800040baf3a2caaebb377a4b32))
- Remove invalid changelog key from Cargo.toml ([`265fedf`](https://github.com/jrollin/cartog/commit/265fedf88efbe96ef7b8d983fa1b448dc10ba588))
- **ci**: Add security checks ([`3815ee6`](https://github.com/jrollin/cartog/commit/3815ee63a0198674bf2cdb52074766fd03f7624e))
- **ci**: Update cargo deny config ([`39bd314`](https://github.com/jrollin/cartog/commit/39bd3145068d7108759edad8226f8d426c7a3c4b))
- **deps**: Upgrade tree-sitter, notify, rusqlite, rmcp to latest ([`c00701b`](https://github.com/jrollin/cartog/commit/c00701b22c1264c42e12cdd1e1f640287e6378a6))
- **plugin**: Remove marketplace.json and add metadata to plugin.json ([`3fc95e5`](https://github.com/jrollin/cartog/commit/3fc95e50bdd816d43c8dc02281e832dbf21c78ad))
- **github**: Add community health files ([`6c247c2`](https://github.com/jrollin/cartog/commit/6c247c282db4b2f5094580425c2fcb2032576e03))
- **ci**: Add codecov.yml with 1% threshold tolerance ([`7d4af4a`](https://github.com/jrollin/cartog/commit/7d4af4a75f0682a66291c31066a07a692c27220c))
- **security**: Bump rustls-webpki to 0.103.13 (RUSTSEC-2026-0104) ([`c4e0f0c`](https://github.com/jrollin/cartog/commit/c4e0f0cf448068ee8ac63dfd7a770a08861c4995))
- PHP doc alignment + webapp_php benchmark fixture (#38) ([`d54561e`](https://github.com/jrollin/cartog/commit/d54561eec44362522624bf48e1ff366a60e4d752))
- Add CODEOWNERS file ([`3969d6b`](https://github.com/jrollin/cartog/commit/3969d6b99e90e63efa66d97c24dc43d92090d802))
- De-pin versions, add Code of Conduct + site hygiene ([`603f401`](https://github.com/jrollin/cartog/commit/603f401e1459a9e8f5815908adb458e40e7a5503))

### Performance

- **skill**: Move heavy session-start work to background pipeline ([`490a1b3`](https://github.com/jrollin/cartog/commit/490a1b3ec1c95cb7638a8c3946bfeffd5988b478))

### Refactor

- Split single crate into Cargo workspace with 9 crates ([`8184e8e`](https://github.com/jrollin/cartog/commit/8184e8eb52f04fc7b8a70e1dd3bc4b3ea72d3b63))
- Extract default constants and avoid double config-path walk ([`aaac621`](https://github.com/jrollin/cartog/commit/aaac6211e4ae213cf87c7639bd5548324dd24ae4))
- Deduplicate Ollama constants and harden config parsing ([`425a5f5`](https://github.com/jrollin/cartog/commit/425a5f581819ee8f8af8c67b699e13dd59e878d2))
- Harden types and document constraints ([`1703fd8`](https://github.com/jrollin/cartog/commit/1703fd8af21867cd47afe58876fe61dd458eda3e))
- **skill**: Improve SKILL.md structure and eval judge tolerance ([`c2c687e`](https://github.com/jrollin/cartog/commit/c2c687e9656db592f2a4b30d2eedc22d253eba1c))
- **self-update**: Trim narration comments across feature files ([`27f644f`](https://github.com/jrollin/cartog/commit/27f644ffdb005092f58e38bc0ff0dcefd2c1cfbc))
- **release**: Harden SHA256SUMS step and document publish env ([`5fea138`](https://github.com/jrollin/cartog/commit/5fea1383d6db380dde0e8a91952682e8ea2c0411))
- **self-update**: Apply audit medium/low fixes ([`6dfe440`](https://github.com/jrollin/cartog/commit/6dfe4407a6404321b7056e6ad8b1ecf38f139147))
- **test**: Tighten wait_for_exec_ready review fixes ([`cb8c01e`](https://github.com/jrollin/cartog/commit/cb8c01eda83858905ca1fb75b99b453b67978acd))
- **self-update**: Apply CodeRabbit findings (valid subset) ([`f33910b`](https://github.com/jrollin/cartog/commit/f33910b2df5124d2abbd170477415fed55231aec))
- **cli**: Cartog init is now config-only ([`3a1c15d`](https://github.com/jrollin/cartog/commit/3a1c15d4fea6ae774e297fabc4f22a2e18fb7e2a))
- **docs**: Hoist bootstrap, fix language count rule, add decision matrix ([`169a91f`](https://github.com/jrollin/cartog/commit/169a91f104004cddbd1f29054425f619ce5bbb04))

### Testing

- **doctor**: Improve coverage with report and formatting tests ([`d76ee30`](https://github.com/jrollin/cartog/commit/d76ee30c2011d96728ebb42f7e7c9b74105a956f))
- **agents**: Add behavioral eval framework with golden examples ([`05056fe`](https://github.com/jrollin/cartog/commit/05056fe27771cb7348e412963d13472a7483d429))
- **self-update**: Pinpoint coverage for spec rules in Phase 7 ([`3b93053`](https://github.com/jrollin/cartog/commit/3b930538bac1d56e7977582e63d72f300e287af6))
- **self-update**: Unit-test pure helpers for in-process coverage ([`63d1b9d`](https://github.com/jrollin/cartog/commit/63d1b9d1cbb279846da32c763b2a8f28aa152aa1))

## [0.3.1] - 2026-02-26

### Features

- Add symbol search command and MCP tool ([`7074957`](https://github.com/jrollin/cartog/commit/70749578c50d84ea44e9c8562ddae252b538b84d))

## [0.3.0] - 2026-02-26

### Features

- Add MCP server mode (`cartog serve`) ([`e94f71d`](https://github.com/jrollin/cartog/commit/e94f71da77c2612660f359abd17022d2b7e6cf39))

## [0.2.0] - 2026-02-26

### Bug Fixes

- **skill**: Improve trigger relevancy and add refactoring workflow ([`76cc2b1`](https://github.com/jrollin/cartog/commit/76cc2b1c1fcc032c785c65a069e645cb98434f7e))

### Documentation

- Add Ruby to supported languages in README and skill ([`56f3bf4`](https://github.com/jrollin/cartog/commit/56f3bf4cb326f3d4b1a85fb0c8e56ad259f6539d))

### Testing

- Improve coverage across core extractors and db layer ([`3e2c296`](https://github.com/jrollin/cartog/commit/3e2c2962294dc59cbc73909fa0236223c9e62801))

## [0.1.6] - 2026-02-26

### Features

- Add benchmark suite for measuring cartog token efficiency ([`ba11c54`](https://github.com/jrollin/cartog/commit/ba11c54983b40b945583ce5eb16c902c69674751))
- Add benchmark suite for measuring cartog token efficiency ([`f4a5c90`](https://github.com/jrollin/cartog/commit/f4a5c90ff462d05bc9999f4323d5e0f6b5030117))

## [0.1.5] - 2026-02-25

### Bug Fixes

- Correct documentation inaccuracies and stale references ([`48031f5`](https://github.com/jrollin/cartog/commit/48031f53d62ca2381941374ad95850dd9493a986))

### Features

- Use skill convention to add to favorite ai ide ([`f0df41a`](https://github.com/jrollin/cartog/commit/f0df41afd0eec6d84915c7686a7420eb4ec96f32))

## [0.1.4] - 2026-02-25

### Bug Fixes

- **ci**: Upload coverage to codecov ([`e180884`](https://github.com/jrollin/cartog/commit/e180884d768b734600d781089733ee67e3678b3f))

## [0.1.3] - 2026-02-25

### Bug Fixes

- Release workflow in linux ([`ea55fba`](https://github.com/jrollin/cartog/commit/ea55fba135c6f4b4516676b52437c0d4637314af))

## [0.1.2] - 2026-02-25

### Bug Fixes

- Bump Cargo.toml version from git tag before build and publish ([`3c5707e`](https://github.com/jrollin/cartog/commit/3c5707e70eb1752515df0986a0254f2bd35d1069))
- Release script can be used in mac and linux ([`24f0a5e`](https://github.com/jrollin/cartog/commit/24f0a5eb4529d3ef3e90473b9ec7fc873560c1da))

### Features

- Add release script to bump version, tag, and push ([`6a9cd6b`](https://github.com/jrollin/cartog/commit/6a9cd6b4554aed81d71a059cdd865b16c85479f1))

## [0.1.1] - 2026-02-25

### Bug Fixes

- Wrong repository ([`ac56278`](https://github.com/jrollin/cartog/commit/ac562785f16ab207d28fd9e27388cdd5d8a1434d))

## [0.1.0] - 2026-02-25

### Bug Fixes

- **doc**: Typo in repo link ([`7da0682`](https://github.com/jrollin/cartog/commit/7da068242937635ef3eb2d8f12c98a54211f0677))

### Features

- Initial commit — code graph indexer with CI/CD ([`3163919`](https://github.com/jrollin/cartog/commit/3163919c7f5eb6e56ed1cdf247e4b8c67a3e5b1e))


