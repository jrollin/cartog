#!/usr/bin/env bash
# cartog Edge-Resolution Scaling Regression Benchmark
#
# Catches super-linear (e.g. quadratic) regressions in edge resolution — the
# class of bug fixed in #110, where tier-2 import-path resolution was O(unresolved
# × import-edges) and invisible on the ~50-file rate fixtures, only diverging at
# transformers scale (376k edges, ~38 min).
#
# Strategy (self-calibrating, no committed baseline):
#   1. Generate a synthetic Python repo with heavy CROSS-FILE imports + calls,
#      so most edges resolve via tier-2 (import-path) — the #110 hot path.
#   2. Time `cartog index --no-lsp --force` at sizes N and 2N.
#   3. Assert time(2N) / time(N) stays under a near-linear threshold. Linear
#      resolution doubles the time (~2x); quadratic quadruples it (~4x). A
#      threshold of 3x flags the regression while tolerating fixed per-run
#      overhead (process start, walk, parse) that inflates the small-N ratio.
#
# Heuristic-only (--no-lsp): deterministic, server-independent, and the path the
# #110 quadratic lived on. Generated repos go in a temp dir (never committed).
#
# KNOWN FINDING (as of 0.26.0): this bench FAILS at the default N=1000 — resolution
# is still super-linear (~O(edges^2)) even after #110, which only fixed tier-2's
# query *plan*. The remaining cost is in the per-edge resolve loop / 2-pass
# structure (profile: sqlite3VdbeFinishMoveto btree seeks dominate), not in any
# single mis-planned query. A red run today is expected and reproduces the open
# lead; once that is fixed the bench should go green and then guards regressions.
#
# Usage:
#   ./benchmarks/resolution_scale.sh                 # default N (see below)
#   N=2000 ./benchmarks/resolution_scale.sh          # larger N (slower, clearer signal)
#   RATIO_MAX=3.0 ./benchmarks/resolution_scale.sh   # tune the fail threshold
#   CARTOG=target/release/cartog ./benchmarks/resolution_scale.sh
#
# Exit codes: 0 = within threshold, 1 = regression (ratio exceeded), 2 = usage/setup.

set -euo pipefail
export LC_NUMERIC=C

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$BENCH_DIR")"

# N = files in the small run; the large run is 2N. Each file imports from and
# calls into SYMS_PER_FILE symbols spread across a few sibling files, so edge
# count grows ~linearly with file count and resolution stays in tier-2.
N="${N:-1000}"
SYMS_PER_FILE="${SYMS_PER_FILE:-8}"
RATIO_MAX="${RATIO_MAX:-3.0}"

# ── Resolve binary (prefer explicit CARTOG, then release, then debug) ──
CARTOG="${CARTOG:-}"
if [ -z "$CARTOG" ]; then
  if [ -x "$REPO_DIR/target/release/cartog" ]; then
    CARTOG="$REPO_DIR/target/release/cartog"
  elif [ -x "$REPO_DIR/target/debug/cartog" ]; then
    CARTOG="$REPO_DIR/target/debug/cartog"
  else
    echo "No cartog binary found. Run: cargo build --release" >&2
    exit 2
  fi
fi
case "$CARTOG" in
  /*) ;;
  */*) CARTOG="$(cd "$(dirname "$CARTOG")" && pwd)/$(basename "$CARTOG")" ;;
esac
echo "binary:  $CARTOG" >&2
echo "version: $("$CARTOG" --version 2>/dev/null | head -1)" >&2

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# ── Generate a synthetic repo of `count` Python files under `dir` ──
# File i defines SYMS_PER_FILE functions and imports + calls functions from the
# next 3 files (mod_{i+1..i+3}), forcing cross-file (tier-2) resolution. The
# wrap-around modulo keeps every import target in-repo so edges resolve.
gen_repo() {
  local dir="$1" count="$2"
  mkdir -p "$dir"
  python3 - "$dir" "$count" "$SYMS_PER_FILE" <<'PY'
import os, sys
dir, count, spf = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
for i in range(count):
    lines = []
    # Import a few functions from sibling modules (cross-file → tier-2).
    for d in (1, 2, 3):
        j = (i + d) % count
        names = ", ".join(f"fn_{j}_{k}" for k in range(spf))
        lines.append(f"from mod_{j} import {names}")
    lines.append("")
    # Define this module's functions, each calling an imported one.
    for k in range(spf):
        callee = f"fn_{(i + 1) % count}_{k}"
        lines.append(f"def fn_{i}_{k}(x):")
        lines.append(f"    return {callee}(x) + {k}")
        lines.append("")
    with open(os.path.join(dir, f"mod_{i}.py"), "w") as f:
        f.write("\n".join(lines))
PY
}

# ── Time one `cartog index --no-lsp --force` over `dir`; echoes seconds ──
# Uses python3 for monotonic, sub-second timing (portable; `date +%s.%N` is not
# on macOS). Resolution stats are printed to stderr for context.
time_index() {
  local dir="$1"
  local db
  db="$WORK/$(basename "$dir").sqlite"
  rm -f "$db"
  python3 - "$CARTOG" "$dir" "$db" <<'PY'
import subprocess, sys, time
cartog, dir, db = sys.argv[1], sys.argv[2], sys.argv[3]
t0 = time.monotonic()
r = subprocess.run([cartog, "index", "--no-lsp", "--force", dir],
                   env={"CARTOG_DB": db, "PATH": __import__("os").environ["PATH"]},
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
dt = time.monotonic() - t0
if r.returncode != 0:
    sys.stderr.write(r.stderr.decode(errors="replace"))
    sys.stderr.write(f"\nindex failed for {dir}\n")
    sys.exit(1)
print(f"{dt:.4f}")
PY
}

echo "generating synthetic repos (N=$N, 2N=$((N * 2)), syms/file=$SYMS_PER_FILE)..." >&2
gen_repo "$WORK/small" "$N"
gen_repo "$WORK/large" "$((N * 2))"

echo "timing index of N=$N files..." >&2
T_SMALL="$(time_index "$WORK/small")"
echo "timing index of 2N=$((N * 2)) files..." >&2
T_LARGE="$(time_index "$WORK/large")"

# ── Verdict (Python for float math + clean formatting) ──
T_SMALL="$T_SMALL" T_LARGE="$T_LARGE" N="$N" RATIO_MAX="$RATIO_MAX" python3 - <<'PY'
import os, sys
ts, tl = float(os.environ["T_SMALL"]), float(os.environ["T_LARGE"])
n, ratio_max = int(os.environ["N"]), float(os.environ["RATIO_MAX"])
ratio = (tl / ts) if ts > 0 else float("inf")
print()
print(f"{'size':<10}{'files':>8}{'index time (s)':>18}")
print("-" * 36)
print(f"{'N':<10}{n:>8}{ts:>18.3f}")
print(f"{'2N':<10}{n * 2:>8}{tl:>18.3f}")
print("-" * 36)
print(f"time(2N)/time(N) = {ratio:.2f}  (linear ~2.0, quadratic ~4.0, threshold {ratio_max})")
print()
if ratio > ratio_max:
    print(f"REGRESSION: scaling ratio {ratio:.2f} exceeds {ratio_max} — "
          f"resolution looks super-linear (cf. #110).", file=sys.stderr)
    sys.exit(1)
print(f"OK: scaling ratio {ratio:.2f} within {ratio_max} (near-linear).")
PY
