# Changelog

## [0.4.4] - 2026-02-28

### Bug Fixes

- Remove redundant clippy-flagged test assertion and enforce fmt/clippy in AGENTS.md ([`203825b`](https://github.com/jrollin/cartog/commit/203825bd6e1fe4ec3816231d92477c3eab56e5d9))
- Format error ([`515cd67`](https://github.com/jrollin/cartog/commit/515cd67f687b90c150d5c7ce451d7310f3ac4111))
- **extract**: Capture calls in package-level var/const initializers ([`3735bd6`](https://github.com/jrollin/cartog/commit/3735bd6e38cfd439ff8cfc0d114f98a672679043))
- **search**: Rank definitions above variables/imports in search results ([`bdfa966`](https://github.com/jrollin/cartog/commit/bdfa966ac8e7fe51d5fd029ec69860bed280622e))
- **ci**: Resolve gitleaks false positive on fixture fake API key ([`9ef398d`](https://github.com/jrollin/cartog/commit/9ef398dec824bfed4ead338d79ef53a9a99ca6d3))
- **ci**: Remove deprecated os and use cross binaries ([`5804ed5`](https://github.com/jrollin/cartog/commit/5804ed55a1a5137abda4b8df00ae90cb88722392))

### Documentation

- **skill**: Document cartog search command and progressive narrowing workflow ([`693f47e`](https://github.com/jrollin/cartog/commit/693f47ed25fdabdf5c3f024e3ff7659d29136020))
- Rewrite README to lead with benchmarks and add demo GIF ([`01ea332`](https://github.com/jrollin/cartog/commit/01ea332aef13d8211ce55129b84f592689b82fec))

### Features

- **perf**: Optimize treesitter parser and sql ([`4746d07`](https://github.com/jrollin/cartog/commit/4746d07e35be8a0e5c8dfbce25a4948c80348b9f))
- Add RAG semantic search, file watcher, and smart search routing ([`ad92a18`](https://github.com/jrollin/cartog/commit/ad92a18ac4685813d0a1424dbbf4dddb983850f6))

### Miscellaneous

- Add git-cliff changelog generation ([`15c45ac`](https://github.com/jrollin/cartog/commit/15c45ac49ce453167777491d29eb4b08dac140d7))
- Add changelog link to Cargo.toml ([`ca0a9dc`](https://github.com/jrollin/cartog/commit/ca0a9dc70d4450800040baf3a2caaebb377a4b32))
- Remove invalid changelog key from Cargo.toml ([`265fedf`](https://github.com/jrollin/cartog/commit/265fedf88efbe96ef7b8d983fa1b448dc10ba588))
- **ci**: Add security checks ([`3815ee6`](https://github.com/jrollin/cartog/commit/3815ee63a0198674bf2cdb52074766fd03f7624e))
- **ci**: Update cargo deny config ([`39bd314`](https://github.com/jrollin/cartog/commit/39bd3145068d7108759edad8226f8d426c7a3c4b))

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


