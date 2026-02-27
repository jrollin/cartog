# webapp_rs

Synthetic Rust web application fixture for cartog benchmarks. Models auth, tokens, routes, middleware, database, cache, events, and validators.

- **65 files, ~3,175 LOC**
- Crate: single `[[bin]]` with `main.rs` as entry point (see `Cargo.toml`)
- Domain: same as all 5 fixtures (cross-language comparison)

## Validate

```bash
cargo check
```

Note: dead_code warnings are expected (fixture code is not executed).
