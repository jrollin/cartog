#!/usr/bin/env bash
# Run the TLA+ specs through TLC headlessly and assert the expected verdict.
#
# Correct specs must pass `Safe` (all conjuncts). Deliberately-broken variants
# must fail — and we assert WHICH named invariant fires, not just that *some*
# violation occurred, so the per-invariant claims in README.md are actually
# defended (a conjunct that never fires on its own broken variant is dead
# weight, and this harness would surface that). The broken variants are
# REGENERATED from the correct translation on every run so they can't drift.
#
# TLC ships inside the TLA+ Toolbox app (no separate install). The jar is pure
# Java and runs headless on any JDK; only the Toolbox GUI is Intel-only.
# Override JAR= to point at a standalone tla2tools.jar.
set -uo pipefail
cd "$(dirname "$0")"

# Reap generated artifacts on exit: broken-variant modules/cfgs, per-invariant
# override cfgs, pcal .old backups, and the TLC states/ metadir. All gitignored,
# but this keeps the working dir tidy run-over-run.
trap 'rm -f -- *Broken.tla *Broken.cfg .*.cfg *.old; rm -rf -- states' EXIT

JAR="${JAR:-/Applications/TLA+ Toolbox.app/Contents/Eclipse/tla2tools.jar}"
if [[ ! -f "$JAR" ]]; then
  echo "SKIP: tla2tools.jar not found at: $JAR" >&2
  echo "      Install the Toolbox (brew install --cask tla+-toolbox) or set JAR=." >&2
  echo "      Set TLA_REQUIRE=1 to make a missing jar a hard failure (e.g. in CI)." >&2
  [[ "${TLA_REQUIRE:-0}" == "1" ]] && exit 1
  exit 0
fi
if ! command -v java >/dev/null 2>&1; then
  echo "SKIP: java not found on PATH (TLA+ checks need a JDK)." >&2
  [[ "${TLA_REQUIRE:-0}" == "1" ]] && exit 1
  exit 0
fi
# python3 builds the broken-variant patches (patch_or_die heredocs). Probe it
# here so its absence skips cleanly instead of surfacing as a misleading
# "anchor drifted" FATAL deep in the run.
if ! command -v python3 >/dev/null 2>&1; then
  echo "SKIP: python3 not found on PATH (needed to derive the broken-spec variants)." >&2
  [[ "${TLA_REQUIRE:-0}" == "1" ]] && exit 1
  exit 0
fi

# -nocfg: never touch the hand-written .cfg files (pcal otherwise prepends a
# stray "Add statements" line and can clobber CONSTANT/CONSTRAINT lines).
# A pcal.trans failure is fatal: continuing would model-check stale output.
translate() {
  if ! java -cp "$JAR" pcal.trans -nocfg "$1" >/dev/null; then
    echo "  FATAL  pcal.trans failed on $1 (PlusCal error or jar/JDK mismatch)" >&2
    exit 1
  fi
}
run_tlc() { java -XX:+UseParallelGC -cp "$JAR" tlc2.TLC -config "$2" "$1" 2>&1; }

rc=0

# A cfg that overrides INVARIANT to a single named invariant, reusing the
# module's SPECIFICATION/CONSTANTS/CONSTRAINT from the base cfg.
# write_single_invariant_cfg <base.cfg> <out.cfg> <InvariantName>
write_single_invariant_cfg() {
  # Keep SPECIFICATION/CONSTANT(S)/CONSTRAINT lines; drop the base INVARIANT
  # line(s); append the single invariant under test.
  grep -vE '^[[:space:]]*INVARIANT' "$1" >"$2"
  echo "INVARIANT $3" >>"$2"
}

# expect_pass <module> <cfg> <label> — correct spec must satisfy all of Safe.
expect_pass() {
  local out; out="$(run_tlc "$1" "$2")"
  if grep -q "No error has been found" <<<"$out"; then
    echo "  PASS  $3 (Safe holds)"
  else
    echo "  FAIL  $3 — expected no error, got:"; echo "$out" | tail -5 | sed 's/^/        /'
    rc=1
  fi
}

# expect_invariant_violated <module> <base.cfg> <InvariantName> <label>
# Asserts the NAMED invariant fires on this (broken) module — defends the
# per-invariant prose in README.md. A "Deadlock"/parse error is NOT a pass:
# we require the specific "Invariant <Name> is violated" banner.
expect_invariant_violated() {
  local cfg=".${1}.${3}.cfg"
  write_single_invariant_cfg "$2" "$cfg" "$3"
  local out; out="$(run_tlc "$1" "$cfg")"
  if grep -q "Invariant $3 is violated" <<<"$out"; then
    echo "  PASS  $4 (Invariant $3 violated as expected — spec discriminates)"
  else
    echo "  FAIL  $4 — expected 'Invariant $3 is violated', got:"
    echo "$out" | grep -E 'Invariant|Error|No error|Deadlock' | head -3 | sed 's/^/        /'
    rc=1
  fi
}

# patch_or_die <description> — run a python heredoc on stdin; abort on failure
# (an assert that fires = the anchor drifted, which must surface as itself,
# not as a downstream "no violation found").
patch_or_die() {
  if ! python3; then
    echo "  FATAL  patch failed: $1 (translation anchor drifted?)" >&2
    exit 1
  fi
}

# --- PidLock: the atomic PID-file lock acquire protocol ------------------
echo "== PidLock (cartog-process-lock) =="
translate PidLock.tla
expect_pass PidLock.tla PidLock.cfg "correct protocol"
# Broken variant: drop the unlink_if_unchanged (pid,st) recheck.
sed 's/MODULE PidLock/MODULE PidLockBroken/' PidLock.tla > PidLockBroken.tla
patch_or_die "PidLock unlink guard" <<'PY'
import re, sys
p = "PidLockBroken.tla"; s = open(p).read()
pat = re.compile(
    r"\\/ /\\ IF ~IsEmpty\(file\) /\\ file\.pid = seenPid\[self\] /\\ file\.st = seenSt\[self\]\s*"
    r"THEN /\\ file' = Empty\s*ELSE /\\ TRUE\s*/\\ file' = file")
s2, n = pat.subn(r"\/ /\ file' = Empty  \* BUG: unconditional unlink", s)
if n != 1:
    sys.stderr.write(f"PidLock patch: expected 1 site, got {n}\n"); sys.exit(1)
open(p, "w").write(s2)
PY
cp PidLock.cfg PidLockBroken.cfg
# AtMostOneHolder is the conjunct that catches the clobber (a stale reaper
# wipes a live winner's file, then wins the empty slot -> two holders).
expect_invariant_violated PidLockBroken.tla PidLockBroken.cfg AtMostOneHolder \
  "clobber bug -> two holders"
# LiveHolderOwnsItsFile catches the wipe itself (a live holder's file emptied
# under it). Asserting it fires here proves it is NOT vacuous.
expect_invariant_violated PidLockBroken.tla PidLockBroken.cfg LiveHolderOwnsItsFile \
  "clobber bug -> live holder's file wiped"

# --- Election: single-writer election + promoter handoff -----------------
echo "== Election (cartog-mcp single_writer) =="
translate Election.tla
expect_pass Election.tla Election.cfg "correct protocol"
# Broken variant: drop the POST-acquire validate_pinned_state (commit blindly).
sed 's/MODULE Election/MODULE ElectionBroken/' Election.tla > ElectionBroken.tla
patch_or_die "Election postValidate" <<'PY'
import sys
p = "ElectionBroken.tla"; s = open(p).read()
old = """                         \\/ /\\ IF pinned[self] = schema
                                  THEN /\\ role' = [role EXCEPT ![self] = "primary"]
                                       /\\ pc' = [pc EXCEPT ![self] = "primaryRun"]
                                       /\\ UNCHANGED lockOwner
                                  ELSE /\\ lockOwner' = NoOwner
                                       /\\ role' = [role EXCEPT ![self] = "aborted"]
                                       /\\ pc' = [pc EXCEPT ![self] = "done"]
                            /\\ alive' = alive"""
new = """                         \\/ /\\ role' = [role EXCEPT ![self] = "primary"]  \\* BUG: no POST re-validate
                            /\\ pc' = [pc EXCEPT ![self] = "primaryRun"]
                            /\\ UNCHANGED lockOwner
                            /\\ alive' = alive"""
if old not in s:
    sys.stderr.write("Election patch: postValidate anchor not found\n"); sys.exit(1)
open(p, "w").write(s.replace(old, new, 1))
PY
cp Election.cfg ElectionBroken.cfg
# PrimaryStateConsistent is the SOLE discriminator here: a peer commits as
# primary on a schema a since-exited third writer migrated under it, while
# remaining the only live primary (AtMostOnePrimary stays satisfied).
expect_invariant_violated ElectionBroken.tla ElectionBroken.cfg PrimaryStateConsistent \
  "schema-drift bug (POST-validate removed)"

echo
if [[ $rc -eq 0 ]]; then echo "All TLA+ checks passed."; else echo "TLA+ checks FAILED."; fi
exit $rc
