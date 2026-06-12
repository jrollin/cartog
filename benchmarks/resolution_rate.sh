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
  resolution_rate.sh --lsp           # add LSP pass (uses host-installed servers)
  resolution_rate.sh --lsp --docker-lsp  # LSP via Docker images (build them first; no host fallback)
  resolution_rate.sh --fixture rs    # one language (py ts rs go rb java php dart swift kt)
  resolution_rate.sh --baseline      # diff vs last saved snapshot (does not overwrite it)
  resolution_rate.sh --no-save       # don't write the snapshot
  CARTOG=target/debug/cartog resolution_rate.sh   # pick a binary

--docker-lsp: run each language's LSP server via Docker through a generated
`[lsp.<lang>]` override (no host fallback). The `cartog-lsp-<lang>:stable` images
must be built first with `make lsp-images`; a missing image is an explicit error.

Snapshot: benchmarks/results/resolution_rate{,_lsp}.json (gitignored).
A --baseline or --fixture run never overwrites the full snapshot.
EOF
}

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$BENCH_DIR")"
FIXTURES_DIR="$BENCH_DIR/fixtures"
INDEXES_DIR="$BENCH_DIR/.indexes"
RESULTS_DIR="$BENCH_DIR/results"

# Capture the user's explicit CARTOG before sourcing common.sh, which defaults it
# to the PATH `cartog`. We prefer the local build (see binary resolution below),
# so only a user-set value should win.
USER_CARTOG="${CARTOG:-}"

# Shared fixture filter (should_skip_fixture) lives in common.sh — single source
# of truth with token_savings.sh and the scenarios. It reads $FIXTURE_FILTER.
source "$BENCH_DIR/lib/common.sh"
export FIXTURE_FILTER=""

# Language tags. Keep in sync with benchmarks/fixtures/webapp_*.
# macOS ships bash 3.2 (no associative arrays), so lookups use case like the
# rest of the suite (see benchmarks/lib/common.sh).
LANGS=(py ts rs go rb java php dart swift kt vue svelte astro)

lang_name() {
  case "$1" in
    py) echo Python ;; ts) echo TypeScript/JS ;; rs) echo Rust ;;
    go) echo Go ;; rb) echo Ruby ;; java) echo Java ;; php) echo PHP ;;
    dart) echo Dart ;; swift) echo Swift ;; kt) echo Kotlin ;;
    vue) echo Vue ;; svelte) echo Svelte ;; astro) echo Astro ;;
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
    vue) echo vue-language-server ;; svelte) echo svelteserver ;;
    astro) echo astro-ls ;;
    *) echo __none__ ;;
  esac
}

# Fixture tag → cartog language name (the `[lsp.<lang>]` key and image suffix).
lsp_lang() {
  case "$1" in
    py) echo python ;; ts) echo typescript ;; rs) echo rust ;;
    rb) echo ruby ;; kt) echo kotlin ;;
    *) echo "$1" ;;  # go, java, php, dart, swift are identity
  esac
}

# Docker LSP image name for a fixture tag (built from benchmarks/lsp-images/).
# Every language uses the uniform `cartog-lsp-<lang>:stable` tag; a Dockerfile may
# simply `FROM` an upstream image (python/typescript wrap lspcontainers) — that
# choice is local to each Dockerfile and invisible here.
docker_image() { echo "cartog-lsp-$(lsp_lang "$1"):stable"; }

CARTOG="$USER_CARTOG"
DO_BASELINE=0
DO_SAVE=1
USE_LSP=0
USE_DOCKER_LSP=0

while [ $# -gt 0 ]; do
  case "$1" in
    --fixture)
      [ $# -ge 2 ] || { echo "error: --fixture needs a value" >&2; usage >&2; exit 2; }
      FIXTURE_FILTER="$2"; shift 2 ;;
    --lsp) USE_LSP=1; shift ;;
    --docker-lsp) USE_DOCKER_LSP=1; shift ;;
    --baseline) DO_BASELINE=1; shift ;;
    --no-save) DO_SAVE=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# --docker-lsp only makes sense during an LSP pass.
if [ "$USE_DOCKER_LSP" -eq 1 ] && [ "$USE_LSP" -eq 0 ]; then
  echo "error: --docker-lsp requires --lsp" >&2; usage >&2; exit 2
fi
if [ "$USE_DOCKER_LSP" -eq 1 ] && ! command -v docker >/dev/null 2>&1; then
  echo "error: --docker-lsp needs docker on PATH" >&2; exit 2
fi

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
# Absolutize the binary: measure() runs cartog from inside the fixture dir (so it
# discovers the override .cartog.toml), where a relative CARTOG would not resolve.
case "$CARTOG" in
  /*) ;;                                   # already absolute
  */*) CARTOG="$(cd "$(dirname "$CARTOG")" && pwd)/$(basename "$CARTOG")" ;;
  *) ;;                                    # bare name on PATH — leave as-is
esac
echo "binary: $CARTOG" >&2

# ── Provenance ── so a snapshot can be traced back to an exact build.
CARTOG_VERSION="$("$CARTOG" --version 2>/dev/null | head -1)"
GIT_SHA="$(git -C "$REPO_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)"
TIMESTAMP="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
export CARTOG_VERSION GIT_SHA TIMESTAMP

mkdir -p "$INDEXES_DIR" "$RESULTS_DIR"

# Temp files for captured stderr and the rows handed to the Python renderer.
err_log="$(mktemp)"
ROWS_FILE="$(mktemp)"
trap 'rm -f "$err_log" "$ROWS_FILE"' EXIT

# Resolve the LSP source for a fixture under --lsp. Sets globals (bash 3.2 has no
# nameref): M_HAS_SERVER (0/1), M_LSP_SOURCE ("docker:<img>"/"host:<bin>"/"none"),
# M_CARTOG_TOML (path to a temp override config to clean up, or empty).
#
# --docker-lsp is strict: it uses the prebuilt `cartog-lsp-<lang>:stable` image
# only, never falling back to a host binary (the preflight already verified the
# image exists). Without --docker-lsp it auto-detects the host server.
resolve_lsp_source() {
  local tag="$1" src="$2"
  M_HAS_SERVER=0; M_LSP_SOURCE="none"; M_CARTOG_TOML=""
  [ "$USE_LSP" -eq 1 ] || return 0

  if [ "$USE_DOCKER_LSP" -eq 1 ]; then
    local img; img="$(docker_image "$tag")"
    # Override config lives in the fixture dir; `.cartog*` is gitignored, so it
    # never pollutes git, and cartog discovers it by walking up from cwd.
    # Guard the write explicitly: a bare `> "$file"` redirect failure does not
    # reliably trip `set -e`, which would let M_HAS_SERVER=1 falsely claim the
    # override was configured when the config was never written.
    M_CARTOG_TOML="$src/.cartog.toml"
    if ! printf '[lsp.%s]\ncommand = ["docker", "run", "--rm", "-i", "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "%s"]\n' \
      "$(lsp_lang "$tag")" "$img" > "$M_CARTOG_TOML"; then
      echo "error: failed to write $M_CARTOG_TOML for $tag" >&2
      M_CARTOG_TOML=""
      return 1
    fi
    M_HAS_SERVER=1; M_LSP_SOURCE="docker:$img"
    return 0
  fi

  if command -v "$(lsp_bin "$tag")" >/dev/null 2>&1; then
    M_HAS_SERVER=1; M_LSP_SOURCE="host:$(lsp_bin "$tag")"
  fi
}

# ── Measure one fixture ──
# Emits: "<tag> <files> <symbols> <edges> <resolved> <has_server> <lsp_source>"
# has_server is 1 when --lsp found a server (Docker image or host binary).
# lsp_source records which resolved the edges ("docker:<img>"/"host:<bin>"/"none").
measure() {
  local tag="$1"
  local src="$FIXTURES_DIR/webapp_$tag"
  [ -d "$src" ] || return 1
  local db="$INDEXES_DIR/webapp_$tag.sqlite"
  rm -f "$db"

  resolve_lsp_source "$tag" "$src" || return 1
  # Clean up any override config we wrote, on every return path.
  [ -n "$M_CARTOG_TOML" ] && trap 'rm -f "$M_CARTOG_TOML"' RETURN

  # A plain string (not an array): bash 3.2 + set -u throws on an empty array
  # expansion, and we need the "no flags" case for LSP auto-detect.
  local lsp_flag="--no-lsp"
  [ "$M_HAS_SERVER" -eq 1 ] && lsp_flag=""   # let cartog use the resolved server

  # Capture stderr so a panic / missing-server / bad-fixture failure surfaces a
  # cause, not just "FAILED <tag>". The caller echoes $err_log on failure.
  # Run from inside the fixture dir so cartog discovers the override .cartog.toml.
  if ! ( cd "$src" && CARTOG_DB="$db" "$CARTOG" index $lsp_flag --force . ) >/dev/null 2>"$err_log"; then
    return 1
  fi
  local json
  if ! json=$(CARTOG_DB="$db" "$CARTOG" stats --db "$db" --json 2>"$err_log"); then
    return 1
  fi
  python3 - "$tag" "$M_HAS_SERVER" "$M_LSP_SOURCE" "$json" <<'PY'
import json, sys
tag, has_server, lsp_source, blob = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
d = json.loads(blob)
print(tag, d["num_files"], d["num_symbols"], d["num_edges"], d["num_resolved"], has_server, lsp_source)
PY
}

# ── Preflight: --docker-lsp requires every image built up front ──
# Strict by design: a missing image is an explicit error (no host fallback), so
# the reported numbers always reflect the containerized server. Build images with
# `make lsp-images` (all) or a per-language `docker build` (see benchmarks/README.md).
if [ "$USE_DOCKER_LSP" -eq 1 ]; then
  missing=""
  for tag in "${LANGS[@]}"; do
    should_skip_fixture "webapp_$tag" && continue
    [ -d "$FIXTURES_DIR/webapp_$tag" ] || continue
    img="$(docker_image "$tag")"
    docker image inspect "$img" >/dev/null 2>&1 || missing="$missing $img"
  done
  if [ -n "$missing" ]; then
    echo "error: --docker-lsp needs these images built first (run: make lsp-images):" >&2
    for img in $missing; do echo "  - $img" >&2; done
    exit 1
  fi
fi

# ── Collect rows ──
ROWS=()
for tag in "${LANGS[@]}"; do
  # Shared filter from common.sh (matches the _<tag> suffix on the fixture name).
  should_skip_fixture "webapp_$tag" && continue
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
    if len(parts) != 7:
        continue
    tag = parts[0]
    files, syms, edges, resolved, has_server = (int(parts[i]) for i in range(1, 6))
    lsp_source = parts[6]
    pct = (100.0 * resolved / edges) if edges else 0.0
    rows.append(dict(tag=tag, name=names.get(tag, tag), files=files,
                     symbols=syms, edges=edges, resolved=resolved, pct=pct,
                     has_server=bool(has_server), lsp_source=lsp_source))

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
               cartog_version=os.environ.get("CARTOG_VERSION", "unknown"),
               git_sha=os.environ.get("GIT_SHA", "unknown"),
               timestamp=os.environ.get("TIMESTAMP", "unknown"),
               overall_pct=round(overall, 2), total_edges=tot_e,
               total_resolved=tot_r, results=rows)
    json.dump(out, open(snapshot, "w"), indent=2)
    print(f"snapshot saved: {snapshot}", file=sys.stderr)
PY
