# cartog-loom-models

Loom model-checking harnesses for cartog's in-process concurrent protocols.
**Test-only, not published.** Run with `make loom` (or
`RUSTFLAGS="--cfg loom" cargo test -p cartog-loom-models`).

## What this is

[Loom](https://docs.rs/loom) exhaustively explores every thread interleaving
**and** every memory reordering the C11 model permits, then checks an invariant
on each. A green run is a proof — over the model — that no schedule violates it.

This complements the TLA+ specs in `specs/tla/`:

- **TLA+** proves the *protocol design* is race-free (cross-process, design level).
- **Loom** proves the *real in-process memory ordering* matches it (atomics,
  mutexes, `Acquire`/`Release`) — the layer TLA+ abstracts away.

## Current model

`single_writer::handler_seeing_primary_finds_rw_db` — the promoter's commit
sequence vs a concurrent write-tool handler, mirroring
`cartog-mcp/src/single_writer.rs::promoter_task` and
`CartogServer::refuse_if_read_only` (by symbol, not line number).

**Invariant:** a handler that observes `Role::Primary` must find the DB cell
already holding the RW connection — never a stale read-only one. This is the
in-process refinement of `Election.tla`'s `LockMatchesPrimary` /
`PrimaryStateConsistent`. It holds because the promoter swaps the DB (mutex
unlock = Release) before storing the role (Release), and the handler loads the
role (Acquire) before locking the DB (Acquire).

Verified to **discriminate**: reordering the commit (store role before the DB
swap) makes loom fail deterministically with a replayable trace. (Weakening the
atomic to `Relaxed` does *not* fail here — the DB mutex already orders the two
writes, so the `Release`/`Acquire` on the role is defensive rather than
load-bearing in this minimal model. The commit *order* is the load-bearing
part, and loom guards it.)

## Why a separate crate

`--cfg loom` is a global compile flag. tokio is loom-aware and gates off
`tokio::signal` under it — which `cartog-mcp` uses (`wait_for_sigint` /
`wait_for_sigterm`), so `cartog-mcp` will not build under loom. Isolating the
harness in a crate that depends on **nothing** from cartog keeps `--cfg loom`
out of tokio's graph. The harness mirrors the production order rather than
importing it, so the production code stays untouched.

The `loom` dependency is scoped to `[target.'cfg(loom)'.dependencies]`, so a
normal `cargo build --workspace` pulls nothing and compiles an empty lib (the
whole crate is `#![cfg(loom)]`). It is therefore invisible to `make check`,
`clippy --all-targets -- -D warnings`, and the release publish list.

## Adding a model

Add a `#[test]` fn inside `src/lib.rs`'s `#[cfg(test)] mod`. Keep each model
**minimal** — loom's state space is exponential; model only the cells that gate
the invariant, not every field of the real struct. Always confirm the model
discriminates (break the protocol, watch loom fail) before trusting a pass.
