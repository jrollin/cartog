#!/usr/bin/env bash
# Guard the idle memory footprint of `cartog serve`.
#
# A serve process that has answered no semantic query should not hold an ONNX
# cross-encoder resident. Several projects open at once made that the dominant
# consumer on the machine: four idle peers measured 2.06 GB combined, while
# `ps` RSS showed only 5-35 MB each because the pages were compressed out.
# RSS is therefore useless here; this uses `footprint`, which counts dirty +
# compressed + swapped.
#
# macOS only (`footprint` is an Apple tool). Skips cleanly elsewhere so the
# target is safe to wire into a cross-platform `make check`.
set -euo pipefail

CARTOG="${CARTOG:-$(cd "$(dirname "$0")/.." && pwd)/target/release/cartog}"
# Ceiling. Measured baselines on a 12-core arm64 host: 246 MB with the reranker
# eagerly loaded, 91 MB with it lazy, 84 MB with no reranker configured. 150 MB
# leaves headroom for allocator and platform variance while still failing if the
# cross-encoder is ever loaded at startup again (which alone costs ~162 MB).
MAX_IDLE_MB="${MAX_IDLE_MB:-150}"
# Floor. A broken sampler that reports 0 would otherwise pass forever; a real
# serve process cannot be this small.
MIN_IDLE_MB="${MIN_IDLE_MB:-5}"
# Seconds to let the server settle after `initialized` before sampling. Too
# short and we sample a half-initialised process, which reads low no matter what
# the code does — so this has a floor of its own.
SETTLE="${SETTLE:-10}"
MIN_SETTLE=5
if [ "$SETTLE" -lt "$MIN_SETTLE" ]; then
  echo "idle_memory: FAIL — SETTLE=$SETTLE is below ${MIN_SETTLE}s; a reading that early is meaningless" >&2
  exit 1
fi

if [ "$(uname -s)" != "Darwin" ]; then
  echo "idle_memory: skipped (needs macOS \`footprint\`; got $(uname -s))"
  exit 0
fi
if ! command -v footprint >/dev/null 2>&1; then
  echo "idle_memory: skipped (\`footprint\` not on PATH)"
  exit 0
fi
if [ ! -x "$CARTOG" ]; then
  echo "idle_memory: FAIL — cartog binary not found at $CARTOG (run: cargo build --release)" >&2
  exit 1
fi

WORK="$(mktemp -d)"
SERVE_PID=""
cleanup() {
  [ -n "$SERVE_PID" ] && kill "$SERVE_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

cd "$WORK"
git init -q .
# A real config so the consent gate is satisfied and serve starts non-degraded.
printf '[database]\npath = ".cartog/db.sqlite"\n' > .cartog.toml
printf 'def hello():\n    return 1\n' > sample.py
# Must succeed: a degraded (unindexed) server has a different memory profile,
# so passing on one would green-light the wrong state.
if ! "$CARTOG" index . >/dev/null 2>&1; then
  echo "idle_memory: FAIL — \`cartog index\` failed on the fixture; cannot measure a real server" >&2
  exit 1
fi

# Handshake only — no tools/call, so nothing should pull in a model.
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"bench","version":"1"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  sleep $((SETTLE + 15))
} | "$CARTOG" serve >/dev/null 2>&1 &
# In a pipeline `$!` is the LAST command, i.e. the serve process itself — not a
# parent to search under. Use it directly; never widen to a host-wide lookup,
# which would happily measure another project's long-lived `serve --watch`.
SERVE_PID=$!

sleep "$SETTLE"

PID="$SERVE_PID"
# Confirm the pid we are about to sample is really our serve, not a recycled one.
if ! ps -o command= -p "$PID" 2>/dev/null | grep -q "cartog serve"; then
  echo "idle_memory: FAIL — pid $PID is not a \`cartog serve\` (did it exit at startup?)" >&2
  exit 1
fi

FOOTPRINT_MB="$(footprint -p "$PID" 2>/dev/null \
  | grep -oE 'Footprint: [0-9]+ (KB|MB|GB)' | head -1 \
  | awk '{ if ($3=="GB") print $2*1024; else if ($3=="KB") print int($2/1024); else print $2 }')"

if [ -z "$FOOTPRINT_MB" ]; then
  echo "idle_memory: FAIL — could not read footprint for pid $PID (sampler broken)" >&2
  exit 1
fi

echo "idle_memory: serve idle footprint = ${FOOTPRINT_MB} MB (allowed ${MIN_IDLE_MB}-${MAX_IDLE_MB} MB)"

# Floor first: an implausibly small reading means the sampler measured the wrong
# thing, which must fail rather than silently "pass".
if [ "$FOOTPRINT_MB" -lt "$MIN_IDLE_MB" ]; then
  echo "idle_memory: FAIL — ${FOOTPRINT_MB} MB is below the ${MIN_IDLE_MB} MB floor; the sampler is measuring the wrong process." >&2
  exit 1
fi
if [ "$FOOTPRINT_MB" -gt "$MAX_IDLE_MB" ]; then
  echo "idle_memory: FAIL — ${FOOTPRINT_MB} MB exceeds the ${MAX_IDLE_MB} MB ceiling." >&2
  echo "  An idle serve that has answered no semantic query should not hold the" >&2
  echo "  cross-encoder (~162 MB). Check that the reranker is still built lazily" >&2
  echo "  (crates/cartog-mcp/src/lazy_provider.rs) rather than at server start." >&2
  exit 1
fi

echo "idle_memory: PASS"
