#!/usr/bin/env bash
# cartog Edge-Resolution Rate Benchmark
#
# Measures edge-resolution rate (resolved / total edges) for every language
# fixture under benchmarks/fixtures/, on a level playing field:
#   - --force         : full re-index, never reuse a stale DB
#   - self-contained  : fixtures have no external deps (no vendor/, node_modules)
#
# Two modes:
#   heuristic (default, --no-lsp) : pure resolver, no language servers. This is
#     the metric to watch when improving the extractor/resolver — it is
#     deterministic and independent of which LSP servers are installed.
#   --lsp : also runs the LSP pass. Per-language it reports whether the server
#     binary is on PATH; rows with no server fall back to the heuristic number
#     and are flagged, so the table never overstates LSP coverage.
#
# Re-run after any change to the extractor (crates/cartog-languages) or the
# resolver (crates/cartog-db src/store/resolution.rs) to see the rate move. Use
# --baseline to diff against the previously saved snapshot.
#
# Usage:
#   ./benchmarks/resolution_rate.sh                 # heuristic, all langs, save snapshot
#   ./benchmarks/resolution_rate.sh --lsp           # add LSP pass (uses installed servers)
#   ./benchmarks/resolution_rate.sh --fixture rs    # one language
#   ./benchmarks/resolution_rate.sh --baseline      # diff vs last saved snapshot
#   ./benchmarks/resolution_rate.sh --no-save       # don't overwrite the snapshot
#   CARTOG=target/debug/cartog ./benchmarks/resolution_rate.sh   # pick a binary
#
# Snapshot: benchmarks/results/resolution_rate{,_lsp}.json (gitignored).

set -euo pipefail
export LC_NUMERIC=C

usage() {
  cat <<'EOF'
cartog Edge-Resolution Rate Benchmark

Measures edge-resolution rate (resolved / total edges) per language fixture.

Usage:
  resolution_rate.sh                 # heuristic, all langs, save snapshot
  resolution_rate.sh --lsp           # add LSP pass (uses installed servers)
  resolution_rate.sh --fixture rs    # one language (py ts rs go rb java php dart swift kt)
  resolution_rate.sh --baseline      # diff vs last saved snapshot (does not overwrite it)
  resolution_rate.sh --no-save       # don't write the snapshot
  CARTOG=target/debug/cartog resolution_rate.sh   # pick a binary

Snapshot: benchmarks/results/resolution_rate{,_lsp}.json (gitignored).
A --baseline or --fixture run never overwrites the full snapshot.
EOF
}

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$BENCH_DIR")"
FIXTURES_DIR="$BENCH_DIR/fixtures"
INDEXES_DIR="$BENCH_DIR/.indexes"
RESULTS_DIR="$BENCH_DIR/results"

# Language tags. Keep in sync with benchmarks/fixtures/webapp_*.
# macOS ships bash 3.2 (no associative arrays), so lookups use case like the
# rest of the suite (see benchmarks/lib/common.sh).
LANGS=(py ts rs go rb java php dart swift kt)

lang_name() {
  case "$1" in
    py) echo Python ;; ts) echo TypeScript/JS ;; rs) echo Rust ;;
    go) echo Go ;; rb) echo Ruby ;; java) echo Java ;; php) echo PHP ;;
    dart) echo Dart ;; swift) echo Swift ;; kt) echo Kotlin ;;
    *) echo "$1" ;;
  esac
}

# Per-language LSP server binary (first spec from crates/cartog-lsp/src/servers.rs).
lsp_bin() {
  case "$1" in
    py) echo pyright-langserver ;; ts) echo typescript-language-server ;;
    rs) echo rust-analyzer ;; go) echo gopls ;; rb) echo ruby-lsp ;;
    java) echo jdtls ;; php) echo intelephense ;; dart) echo dart ;;
    swift) echo sourcekit-lsp ;; kt) echo kotlin-language-server ;;
    *) echo __none__ ;;
  esac
}

CARTOG="${CARTOG:-}"
FIXTURE_FILTER=""
DO_BASELINE=0
DO_SAVE=1
USE_LSP=0

while [ $# -gt 0 ]; do
  case "$1" in
    --fixture)
      [ $# -ge 2 ] || { echo "error: --fixture needs a value" >&2; usage >&2; exit 2; }
      FIXTURE_FILTER="$2"; shift 2 ;;
    --lsp) USE_LSP=1; shift ;;
    --baseline) DO_BASELINE=1; shift ;;
    --no-save) DO_SAVE=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# Validate the fixture tag against the known set so a typo (e.g. "rust") fails
# loudly instead of silently measuring nothing.
if [ -n "$FIXTURE_FILTER" ]; then
  valid=0
  for t in "${LANGS[@]}"; do [ "$t" = "$FIXTURE_FILTER" ] && valid=1 && break; done
  if [ "$valid" -eq 0 ]; then
    echo "error: unknown fixture '$FIXTURE_FILTER' (valid: ${LANGS[*]})" >&2
    exit 2
  fi
fi

if [ "$USE_LSP" -eq 1 ]; then
  SNAPSHOT="$RESULTS_DIR/resolution_rate_lsp.json"
else
  SNAPSHOT="$RESULTS_DIR/resolution_rate.json"
fi

# A partial (--fixture) or comparative (--baseline) run must never overwrite the
# canonical full snapshot: --fixture would shrink it to one row, and --baseline
# would clobber the very baseline it just diffed against (so the next diff reads
# ~0). Both cases force --no-save unless the user is already not saving.
if [ "$DO_SAVE" -eq 1 ] && { [ "$DO_BASELINE" -eq 1 ] || [ -n "$FIXTURE_FILTER" ]; }; then
  DO_SAVE=0
  reason="--baseline"; [ -n "$FIXTURE_FILTER" ] && reason="--fixture"
  echo "note: $reason run — snapshot $SNAPSHOT left unchanged" >&2
fi

# ── Resolve binary ──
# Prefer an explicit CARTOG, then release, then debug. Never the PATH `cartog`
# (that may be an older installed build, defeating before/after comparisons).
if [ -z "$CARTOG" ]; then
  if [ -x "$REPO_DIR/target/release/cartog" ]; then
    CARTOG="$REPO_DIR/target/release/cartog"
  elif [ -x "$REPO_DIR/target/debug/cartog" ]; then
    CARTOG="$REPO_DIR/target/debug/cartog"
  else
    echo "No cartog binary found. Run: cargo build --release" >&2
    exit 1
  fi
fi
echo "binary: $CARTOG" >&2

mkdir -p "$INDEXES_DIR" "$RESULTS_DIR"

# Temp files for captured stderr and the rows handed to the Python renderer.
err_log="$(mktemp)"
ROWS_FILE="$(mktemp)"
trap 'rm -f "$err_log" "$ROWS_FILE"' EXIT

# ── Measure one fixture ──
# Emits: "<tag> <files> <symbols> <edges> <resolved> <has_server>"
# has_server is 1 only in --lsp mode when the language's LSP binary is on PATH.
measure() {
  local tag="$1"
  local src="$FIXTURES_DIR/webapp_$tag"
  [ -d "$src" ] || return 1
  local db="$INDEXES_DIR/webapp_$tag.sqlite"
  rm -f "$db"

  # A plain string (not an array): bash 3.2 + set -u throws on an empty array
  # expansion, and we need the "no flags" case for LSP auto-detect.
  local has_server=0
  local lsp_flag="--no-lsp"
  if [ "$USE_LSP" -eq 1 ] && command -v "$(lsp_bin "$tag")" >/dev/null 2>&1; then
    lsp_flag=""          # let cartog auto-detect the server on PATH
    has_server=1
  fi

  # Capture stderr so a panic / missing-server / bad-fixture failure surfaces a
  # cause, not just "FAILED <tag>". The caller echoes $err_log on failure.
  if ! CARTOG_DB="$db" "$CARTOG" index $lsp_flag --force "$src" >/dev/null 2>"$err_log"; then
    return 1
  fi
  local json
  if ! json=$(CARTOG_DB="$db" "$CARTOG" stats --db "$db" --json 2>"$err_log"); then
    return 1
  fi
  python3 - "$tag" "$has_server" "$json" <<'PY'
import json, sys
tag, has_server, blob = sys.argv[1], sys.argv[2], sys.argv[3]
d = json.loads(blob)
print(tag, d["num_files"], d["num_symbols"], d["num_edges"], d["num_resolved"], has_server)
PY
}

# ── Collect rows ──
ROWS=()
for tag in "${LANGS[@]}"; do
  [ -n "$FIXTURE_FILTER" ] && [ "$tag" != "$FIXTURE_FILTER" ] && continue
  [ -d "$FIXTURES_DIR/webapp_$tag" ] || { echo "skip $tag (no fixture)" >&2; continue; }
  echo "indexing $tag..." >&2
  if ! row=$(measure "$tag"); then
    echo "FAILED $tag" >&2
    [ -s "$err_log" ] && sed 's/^/  | /' "$err_log" >&2
    continue
  fi
  ROWS+=("$row")
done

[ ${#ROWS[@]} -eq 0 ] && { echo "no fixtures measured" >&2; exit 1; }

# ── Render table + JSON snapshot + optional baseline diff (all in Python) ──
# Rows go through a temp file, not stdin: `python3 - <<'PY'` already consumes
# stdin for the script, so piping data there too would collide.
printf '%s\n' "${ROWS[@]}" > "$ROWS_FILE"
NAMES_JSON="$(
  for t in "${LANGS[@]}"; do printf '%s\t%s\n' "$t" "$(lang_name "$t")"; done
)" ROWS_FILE="$ROWS_FILE" SNAPSHOT="$SNAPSHOT" DO_BASELINE="$DO_BASELINE" \
  DO_SAVE="$DO_SAVE" USE_LSP="$USE_LSP" python3 - <<'PY'
import json, os, sys

use_lsp = os.environ["USE_LSP"] == "1"
rate_label = "with-LSP %" if use_lsp else "heuristic %"
names = dict(l.split("\t") for l in os.environ["NAMES_JSON"].splitlines() if l)
rows = []
for line in open(os.environ["ROWS_FILE"]):
    parts = line.split()
    if len(parts) != 6:
        continue
    tag = parts[0]
    files, syms, edges, resolved, has_server = map(int, parts[1:])
    pct = (100.0 * resolved / edges) if edges else 0.0
    rows.append(dict(tag=tag, name=names.get(tag, tag), files=files,
                     symbols=syms, edges=edges, resolved=resolved, pct=pct,
                     has_server=bool(has_server)))

# Sort highest resolution first.
rows.sort(key=lambda r: r["pct"], reverse=True)

snapshot = os.environ["SNAPSHOT"]
baseline = {}
if os.environ["DO_BASELINE"] == "1" and os.path.exists(snapshot):
    prev = json.load(open(snapshot))
    baseline = {r["tag"]: r["pct"] for r in prev.get("results", [])}

# Aggregate (micro-average: pooled edges, not mean of per-language rates).
tot_e = sum(r["edges"] for r in rows)
tot_r = sum(r["resolved"] for r in rows)
overall = (100.0 * tot_r / tot_e) if tot_e else 0.0

show_delta = bool(baseline)
hdr = f"{'Language':<16}{'files':>6}{'symbols':>9}{'edges':>7}{'resolved':>10}{rate_label:>13}"
if use_lsp:
    hdr += f"{'LSP':>6}"
if show_delta:
    hdr += f"{'Δ vs base':>12}"
print()
print(hdr)
print("-" * len(hdr))
for r in rows:
    line = (f"{r['name']:<16}{r['files']:>6}{r['symbols']:>9}{r['edges']:>7}"
            f"{r['resolved']:>10}{r['pct']:>12.0f}%")
    if use_lsp:
        line += f"{('yes' if r['has_server'] else '—'):>6}"
    if show_delta:
        if r["tag"] in baseline:
            d = r["pct"] - baseline[r["tag"]]
            line += f"{d:>+11.1f}%"
        else:
            line += f"{'new':>12}"
    print(line)
print("-" * len(hdr))
agg = (f"{'OVERALL (pooled)':<16}{'':>6}{'':>9}{tot_e:>7}{tot_r:>10}{overall:>12.0f}%")
print(agg)
if use_lsp:
    missing = [r["name"] for r in rows if not r["has_server"]]
    if missing:
        print(f"\nnote: no LSP server installed for {', '.join(missing)} "
              f"— those rows are heuristic-only.")
print()

if os.environ["DO_SAVE"] == "1":
    out = dict(mode=("lsp" if use_lsp else "heuristic"),
               overall_pct=round(overall, 2), total_edges=tot_e,
               total_resolved=tot_r, results=rows)
    json.dump(out, open(snapshot, "w"), indent=2)
    print(f"snapshot saved: {snapshot}", file=sys.stderr)
PY
