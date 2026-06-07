# TLA+ specs: cartog's concurrent protocols

Machine-checked models of the two race-prone subsystems in cartog. Run with
`make tla` (from the repo root) or `./run.sh` (here).

| Spec | Models | Rust source | Invariants |
|------|--------|-------------|------------|
| `PidLock.tla` | the atomic PID-file lock acquire | `crates/cartog-process-lock/src/lib.rs` (`ProcessLock::acquire`) | `AtMostOneHolder`, `LiveHolderOwnsItsFile` |
| `Election.tla` | single-writer election + promoter handoff | `crates/cartog-mcp/src/single_writer.rs` | `AtMostOnePrimary`, `LockMatchesPrimary`, `PrimaryStateConsistent` |

The lock primitive (`PidLock`) is what the election (`Election`) abstracts as a
test-and-set, so the two specs compose: `PidLock` proves the acquire is
race-free, `Election` proves the promotion logic built on top of it is too.

## Why model these

Both protocols are argued correct in prose comments and sampled by concurrency
tests — one of which is **quarantined as flaky**
(`concurrent_acquires_never_observe_empty_target`, `lib.rs` ~L861). A flaky
concurrency test is the signal that tests are *sampling* interleavings, not
*covering* them. TLC covers them: it explores every reachable interleaving of
N processes (with a crash possible at any step) and either proves the safety
invariant or hands back the shortest violating trace.

## Results

`make tla` runs five checks — each correct spec must pass `Safe`, and each
broken variant must fail the specific named invariant that should catch its bug
(proving the spec actually discriminates):

`make tla` prints one line per check (verbatim shape):

```text
== PidLock (cartog-process-lock) ==
  PASS  correct protocol (Safe holds)
  PASS  clobber bug -> two holders (Invariant AtMostOneHolder violated as expected ...)
  PASS  clobber bug -> live holder's file wiped (Invariant LiveHolderOwnsItsFile violated ...)
== Election (cartog-mcp single_writer) ==
  PASS  correct protocol (Safe holds)
  PASS  schema-drift bug (POST-validate removed) (Invariant PrimaryStateConsistent violated ...)
```

The harness checks the correct spec against the full `Safe` conjunction, then
re-runs each broken variant against the **specific named invariant** that must
catch the bug. Asserting the named invariant (not just "some violation") is
what defends the per-invariant prose below: if a conjunct never fires on its
own broken variant it is dead weight, and the harness would surface that.

The broken variants are **regenerated from the correct translation on every
run** (only the one guard is removed), so a vacuously-passing spec can't hide.

### PidLock invariants

- **`AtMostOneHolder`** — never two live processes hold the slot. Two holders ⇒
  two MCP primaries ⇒ two writers on one DB. This is the workhorse: removing
  the `(pid,st)` recheck lets a stale reaper wipe a fresh winner's file and then
  win the empty slot, so two `holder` flags end up TRUE at once.
- **`LiveHolderOwnsItsFile`** — a live holder's on-disk file still records that
  holder's `(pid, st)`; no acquirer wipes/overwrites it out from under a live
  holder. This is the `unlink_if_unchanged` TOCTOU guard (`unlink_if_unchanged`
  in `cartog-process-lock/src/lib.rs`), the thing that stops a `cartog self
  update` reap from clobbering a live primary. It is anchored on the *live
  holder* (not on `file.pid`) precisely so a wipe is a real violation rather
  than a satisfied empty antecedent — the harness asserts it fires on the broken
  variant, proving it is not vacuous.

Illustrative trace of the clobber the broken PidLock variant produces (the
harness asserts the *verdict*, not this trace; the steps are hand-annotated):

```text
1:  proc 1 owns the lock                       file=pid1  holder=<T,F,F>
2:  proc 1 crashes                             alive=<F,T,T>  (file=pid1 now stale)
3-5: proc 2 reads stale(1), heads to unlink    (snapshot pid=1)
6-7: proc 3 ALSO reads stale(1)                (snapshot pid=1)
8:  proc 2 reaps -> file Empty
9:  proc 2 WINS                                file=pid2  holder=<F,T,F>  (legit)
10-11: proc 3, still on its pid=1 snapshot, UNCONDITIONALLY wipes file       (CLOBBER)
12: proc 3 wins the now-empty slot             holder=<F,T,T>   TWO HOLDERS
```

### Election invariants

- **`AtMostOnePrimary`** — at most one *live* peer in the Primary role. (Dead
  primaries write nothing and are reaped; the danger is two live RW writers.)
  Note this conjunct does NOT catch the schema-drift bug on its own — see below.
- **`LockMatchesPrimary`** — a live Primary always owns the serve lock.
- **`PrimaryStateConsistent`** — a live Primary's pinned snapshot
  (schema_version + embedding fingerprint) equals the on-disk schema. This is
  what the PRE/POST double-validate (`validate_pinned_state` in
  `cartog-mcp/src/single_writer.rs`) buys, and it is the **sole** discriminator
  on the broken variant: dropping the POST-acquire validate lets a single live
  primary run with a stale snapshot without any second primary, so
  `AtMostOnePrimary` stays satisfied while this invariant fires.

Illustrative trace of the third-writer drift the broken Election variant
produces (verdict asserted by the harness; steps hand-annotated):

```text
peer 2 attaches ReadOnly (pinned=0) -> primary 1 exits ->
peer 2 passes PRE-validate (0==0) -> a THIRD writer (peer 3) wins, MIGRATES
(schema 0->1), exits -> peer 2 finally acquires the lock (pinned=0, schema=1) ->
[BROKEN] peer 2 commits as Primary anyway -> live Primary on a schema it never
reconciled. PrimaryStateConsistent violated.
```

The correct POST-validate catches this exactly there: `pinned=0 ≠ schema=1` →
drop the lock, abort cleanly.

## How the specs and the Rust "articulate"

They share **zero code**, and TLA+ never reads Rust. Each spec is a *separate
artifact* that models the protocol at the granularity of "what is one atomic
step." The link is a **discipline**, in three parts:

1. **One labeled PlusCal step = one atomic operation in the Rust.** In
   `PidLock`: `tryLink` ↔ `hard_link`, `readHolder` ↔ `read_lock_file`,
   `unlinkIfUnchanged` ↔ the re-read-and-compare. In `Election`: `elect` /
   `acquire` ↔ the O_EXCL acquire, `preValidate` / `postValidate` ↔ the two
   `validate_pinned_state` calls. Splitting them into separate labels is what
   lets TLC schedule a crash or a competing writer *between* two syscalls — the
   schedules a test can only sample.

2. **The spec records its assumptions, and they become review obligations on
   the Rust.** `PidLock`'s load-bearing one: `hard_link` is atomic
   (creates-or-fails, no empty-file window). True on local POSIX, **false on
   NFS**. The proof holds *given* that — so don't swap `hard_link` for
   `create_new`+`write`, don't run the state dir on NFS. `Election` assumes the
   serve lock is a correct test-and-set, which is exactly what `PidLock`
   proves. The specs are where those preconditions are written down.

3. **The invariants are the prose comments, made executable.** `lib.rs` says
   "without the recheck `cartog self update` could clobber a live primary's
   lock"; `LiveHolderOwnsItsFile` is that sentence as a checkable predicate.
   `single_writer.rs` says the POST-validate is needed because "a third writer
   could have promoted, upgraded the schema, and exited"; the broken-`Election`
   counterexample is the proof that's real and the guard is load-bearing.

### What it buys, what it does not

- **Buys:** a proof that the *protocol design* is race-free under all schedules
  including mid-protocol crashes — and a concrete trace when it is not. Far
  stronger than a 1-in-500 flaky test.
- **Does not buy:** any guarantee about the Rust itself. If the code grows a
  branch the spec omits, or an assumption is false in production (NFS), the bug
  slips through. The model is a claim about the *design*; tests + types remain
  the claim about the *code*. Complementary, not substitutes.

So the workflow is: **change the protocol → update the spec → TLC proves the
invariant (or hands you a counterexample) → implement so each atomic action
matches a labeled step → keep the spec's assumptions true on later changes.**
The specs are the canonical statement of *why* the retry loop and the
double-validate are shaped the way they are.

The link is currently **one-way** (spec → Rust): the specs cite the Rust by
symbol name, but no Rust comment yet points back here. That means changing the
promoter or the acquire loop gives no in-code reminder to update the spec — the
one gap in the discipline. Adding `// see specs/tla/<spec>.tla` near
`unlink_if_unchanged` and the promoter commit would close it.

## Running

```bash
make tla                              # from repo root — runs all five checks
./run.sh                              # same, from this directory
JAR=/path/to/tla2tools.jar ./run.sh   # override jar location
TLA_REQUIRE=1 ./run.sh                # make a missing jar a hard failure (CI)
```

Needs a JDK (any) and `tla2tools.jar`. The jar ships inside the TLA+ Toolbox
(`brew install --cask tla+-toolbox`) at
`/Applications/TLA+ Toolbox.app/Contents/Eclipse/tla2tools.jar`; only the GUI
is Intel-only, the jar runs headless on any JVM. When the jar is absent,
`run.sh` **skips** (exit 0) unless `TLA_REQUIRE=1` — so a contributor without
TLA+ tooling is not blocked, and `make tla` stays out of `make check`.

## Files

| File | Role |
|------|------|
| `PidLock.tla` / `Election.tla` | The models. Edit the PlusCal in the `(* --algorithm … *)` block; `run.sh` re-runs `pcal.trans -nocfg` to regenerate the TLA+ below `BEGIN TRANSLATION`. |
| `PidLock.cfg` / `Election.cfg` | TLC configs (constants, constraints, `INVARIANT Safe`). Hand-written — `-nocfg` keeps `pcal.trans` from touching them. |
| `*Broken.tla` / `*Broken.cfg`, `.*.cfg` | Generated by `run.sh` (broken variants + per-invariant override cfgs) to prove each spec discriminates. **Do not edit or commit** — gitignored. |
| `run.sh` | Translate + model-check both specs and their broken variants; asserts the expected verdict. |
