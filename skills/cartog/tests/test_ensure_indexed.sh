#!/usr/bin/env bash
set -euo pipefail

# Unit tests for ensure_indexed.sh
# Uses mocked cartog commands to verify phase ordering and behavior.
#
# Usage: bash skills/cartog/tests/test_ensure_indexed.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENSURE_SCRIPT="$SKILL_DIR/scripts/ensure_indexed.sh"

PASS=0
FAIL=0
TEST_DIR=""

# --- helpers ---

setup() {
    TEST_DIR=$(mktemp -d)
    mkdir -p "$TEST_DIR/bin"
    # log file tracks command invocations in order
    export CARTOG_TEST_LOG="$TEST_DIR/commands.log"
    : > "$CARTOG_TEST_LOG"
    # Clean up lock from any previous test
    rmdir /tmp/cartog-rag-index.lock 2>/dev/null || true
}

teardown() {
    rmdir /tmp/cartog-rag-index.lock 2>/dev/null || true
    [ -n "$TEST_DIR" ] && rm -rf "$TEST_DIR"
}

assert_eq() {
    local label="$1" expected="$2" actual="$3"
    if [ "$expected" = "$actual" ]; then
        echo "  PASS: $label"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $label"
        echo "    expected: $expected"
        echo "    actual:   $actual"
        FAIL=$((FAIL + 1))
    fi
}

assert_contains() {
    local label="$1" needle="$2" haystack="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        echo "  PASS: $label"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $label"
        echo "    expected to contain: $needle"
        echo "    actual: $haystack"
        FAIL=$((FAIL + 1))
    fi
}

assert_not_contains() {
    local label="$1" needle="$2" haystack="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        echo "  FAIL: $label"
        echo "    expected NOT to contain: $needle"
        echo "    actual: $haystack"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: $label"
        PASS=$((PASS + 1))
    fi
}

assert_file_exists() {
    local label="$1" path="$2"
    if [ -f "$path" ]; then
        echo "  PASS: $label"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $label"
        echo "    file not found: $path"
        FAIL=$((FAIL + 1))
    fi
}

# Create a mock cartog binary that logs invocations
create_mock_cartog() {
    local exit_rag_setup="${1:-0}"
    local rag_setup_stderr="${2:-}"
    local mock_version="${3:-0.6.1}"
    cat > "$TEST_DIR/bin/cartog" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "--version" ]; then
    echo "cartog $mock_version"
    exit 0
fi
echo "\$@" >> "$CARTOG_TEST_LOG"

# Simulate different subcommands
if [ "\$1" = "index" ]; then
    exit 0
elif [ "\$1" = "rag" ] && [ "\$2" = "setup" ]; then
    if [ -n "$rag_setup_stderr" ]; then
        echo "$rag_setup_stderr" >&2
    fi
    exit $exit_rag_setup
elif [ "\$1" = "rag" ] && [ "\$2" = "index" ]; then
    sleep 0.1
    exit 0
fi
MOCK
    chmod +x "$TEST_DIR/bin/cartog"
}

# Create a mock curl that returns a fake GitHub release response
create_mock_curl() {
    local latest_version="${1:-0.7.0}"
    local exit_code="${2:-0}"
    cat > "$TEST_DIR/bin/curl" <<MOCK
#!/usr/bin/env bash
if [ "$exit_code" -ne 0 ]; then
    exit $exit_code
fi
echo '{ "tag_name": "v$latest_version" }'
MOCK
    chmod +x "$TEST_DIR/bin/curl"
}

# Run ensure_indexed.sh with mocked cartog and curl, in a temp workdir
run_ensure_indexed() {
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    (
        export PATH="$TEST_DIR/bin:$PATH"
        export HOME="$TEST_DIR/home"
        mkdir -p "$HOME"
        cd "$workdir"
        bash "$ENSURE_SCRIPT" 2>&1
    )
}

# --- tests ---

test_fresh_index_shows_building() {
    echo "TEST: fresh index (no .cartog.db) shows 'Building'"
    setup
    create_mock_cartog

    local output
    output=$(run_ensure_indexed)

    assert_contains "shows 'Building'" "No cartog index found. Building..." "$output"
    teardown
}

test_existing_index_shows_updating() {
    echo "TEST: existing .cartog.db shows 'Updating'"
    setup
    create_mock_cartog
    touch "$TEST_DIR/workdir/.cartog.db" 2>/dev/null || { mkdir -p "$TEST_DIR/workdir" && touch "$TEST_DIR/workdir/.cartog.db"; }

    local output
    output=$(run_ensure_indexed)

    assert_contains "shows 'Updating'" "Updating cartog index..." "$output"
    teardown
}

test_phase_order() {
    echo "TEST: commands run in correct order (index, rag setup, rag index)"
    setup
    create_mock_cartog

    run_ensure_indexed > /dev/null
    # Wait briefly for background process to log
    sleep 0.3

    local line1 line2 line3
    line1=$(sed -n '1p' "$CARTOG_TEST_LOG")
    line2=$(sed -n '2p' "$CARTOG_TEST_LOG")
    line3=$(sed -n '3p' "$CARTOG_TEST_LOG")

    assert_eq "phase 1: cartog index ." "index ." "$line1"
    assert_eq "phase 2: cartog rag setup" "rag setup" "$line2"
    assert_eq "phase 3: cartog rag index ." "rag index ." "$line3"
    teardown
}

test_rag_setup_failure_continues() {
    echo "TEST: rag setup failure shows warning but continues to rag index"
    setup
    create_mock_cartog 1 "Error: model download failed"

    local output
    output=$(run_ensure_indexed)
    sleep 0.3

    assert_contains "shows warning" "Warning: cartog rag setup failed" "$output"
    # rag index should still run in background
    local line3
    line3=$(sed -n '3p' "$CARTOG_TEST_LOG")
    assert_eq "rag index still runs" "rag index ." "$line3"
    teardown
}

test_rag_setup_stderr_visible() {
    echo "TEST: rag setup stderr is visible (not redirected to log file)"
    setup
    create_mock_cartog 1 "Error: disk full"

    local output
    output=$(run_ensure_indexed)

    # stderr should flow through to the user (visible in AI editors)
    assert_contains "stderr visible in output" "Error: disk full" "$output"
    teardown
}

test_background_rag_index() {
    echo "TEST: rag index runs in background (script returns before it finishes)"
    setup
    # Make rag index take longer
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
echo "$@" >> "$CARTOG_TEST_LOG"
if [ "$1" = "rag" ] && [ "$2" = "index" ]; then
    sleep 2
    exit 0
fi
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local start end elapsed
    start=$(date +%s)
    run_ensure_indexed > /dev/null
    end=$(date +%s)
    elapsed=$((end - start))

    # Script should return in < 2s (rag index sleeps 2s in background)
    if [ "$elapsed" -lt 2 ]; then
        echo "  PASS: script returned before background rag index finished (${elapsed}s < 2s)"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: script blocked on rag index (${elapsed}s >= 2s)"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_lock_prevents_concurrent_rag_index() {
    echo "TEST: lock prevents concurrent rag index (second run skips)"
    setup
    create_mock_cartog

    # Pre-create the lock directory to simulate an already-running rag index
    mkdir /tmp/cartog-rag-index.lock

    local output
    output=$(run_ensure_indexed)
    sleep 0.3

    assert_contains "skips rag index" "RAG embedding already running" "$output"
    # Only index + rag setup should have run (no rag index)
    local line_count
    line_count=$(wc -l < "$CARTOG_TEST_LOG" | tr -d ' ')
    assert_eq "only 2 commands logged (no rag index)" "2" "$line_count"
    teardown
}

test_lock_cleaned_after_rag_index() {
    echo "TEST: lock directory is removed after rag index completes"
    setup
    create_mock_cartog

    run_ensure_indexed > /dev/null
    # Wait for background rag index to finish (mock sleeps 0.1s)
    sleep 0.5

    if [ ! -d /tmp/cartog-rag-index.lock ]; then
        echo "  PASS: lock directory cleaned up after completion"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: lock directory still exists after rag index finished"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_stale_lock_removed() {
    echo "TEST: stale lock (>1 hour) is removed and rag index proceeds"
    # Wait for any background rag index from previous tests to finish and release lock
    sleep 0.5
    setup
    create_mock_cartog

    # Create a lock directory and backdate it to 2 hours ago
    mkdir /tmp/cartog-rag-index.lock
    touch -t "$(date -v-2H '+%Y%m%d%H%M.%S' 2>/dev/null || date -d '2 hours ago' '+%Y%m%d%H%M.%S' 2>/dev/null)" /tmp/cartog-rag-index.lock

    local output
    output=$(run_ensure_indexed)
    sleep 0.3

    assert_contains "detects stale lock" "Removing stale RAG lock" "$output"
    assert_contains "starts rag index" "RAG embedding started in background" "$output"
    # All 3 phases should have run
    local line3
    line3=$(sed -n '3p' "$CARTOG_TEST_LOG")
    assert_eq "rag index runs after stale lock removal" "rag index ." "$line3"
    teardown
}

test_output_messages() {
    echo "TEST: output includes RAG background PID and status message"
    setup
    create_mock_cartog

    local output
    output=$(run_ensure_indexed)

    assert_contains "mentions background PID" "RAG embedding started in background" "$output"
    assert_contains "mentions FTS5+reranker ready" "FTS5 + reranker" "$output"
    teardown
}

# --- version check tests ---

test_no_cache_triggers_fetch() {
    echo "TEST: no cache file triggers GitHub API fetch and creates cache"
    setup
    create_mock_cartog 0 "" "0.6.1"
    create_mock_curl "0.7.0"

    local output
    output=$(run_ensure_indexed)

    assert_contains "prints update notice" "New cartog version available: 0.7.0 (installed: 0.6.1)" "$output"
    assert_file_exists "cache file created" "$TEST_DIR/home/.cache/cartog/latest_version"
    teardown
}

test_fresh_cache_skips_fetch() {
    echo "TEST: fresh cache (<24h) skips GitHub API fetch"
    setup
    create_mock_cartog 0 "" "0.6.1"
    # Do NOT create mock curl — if curl is called, the test would fail or use real curl
    # Instead, pre-populate cache and create a curl that would return a different version
    create_mock_curl "0.9.0"
    mkdir -p "$TEST_DIR/home/.cache/cartog"
    echo "0.7.0 $(date +%s)" > "$TEST_DIR/home/.cache/cartog/latest_version"

    local output
    output=$(run_ensure_indexed)

    # Should use cached 0.7.0, not the curl 0.9.0
    assert_contains "uses cached version" "New cartog version available: 0.7.0 (installed: 0.6.1)" "$output"
    teardown
}

test_stale_cache_triggers_fetch() {
    echo "TEST: stale cache (>24h) triggers fresh GitHub API fetch"
    setup
    create_mock_cartog 0 "" "0.6.1"
    create_mock_curl "0.8.0"
    mkdir -p "$TEST_DIR/home/.cache/cartog"
    local old_ts=$(( $(date +%s) - 90000 ))  # 25 hours ago
    echo "0.7.0 $old_ts" > "$TEST_DIR/home/.cache/cartog/latest_version"

    local output
    output=$(run_ensure_indexed)

    # Should fetch fresh and get 0.8.0 from mock curl
    assert_contains "uses fetched version" "New cartog version available: 0.8.0 (installed: 0.6.1)" "$output"
    teardown
}

test_same_version_no_notice() {
    echo "TEST: same version prints no update notice"
    setup
    create_mock_cartog 0 "" "0.7.0"
    create_mock_curl "0.7.0"

    local output
    output=$(run_ensure_indexed)

    assert_not_contains "no update notice" "New cartog version available" "$output"
    teardown
}

test_newer_installed_no_notice() {
    echo "TEST: installed version newer than latest prints no notice"
    setup
    create_mock_cartog 0 "" "0.8.0"
    create_mock_curl "0.7.0"

    local output
    output=$(run_ensure_indexed)

    assert_not_contains "no update notice" "New cartog version available" "$output"
    teardown
}

test_fetch_failure_continues() {
    echo "TEST: GitHub API failure silently continues to indexing"
    setup
    create_mock_cartog 0 "" "0.6.1"
    create_mock_curl "" 1

    local output
    output=$(run_ensure_indexed)
    sleep 0.3

    assert_not_contains "no update notice" "New cartog version available" "$output"
    # Indexing should still proceed
    local line1
    line1=$(sed -n '1p' "$CARTOG_TEST_LOG")
    assert_eq "indexing still runs" "index ." "$line1"
    teardown
}

# --- run all tests ---

echo "=== ensure_indexed.sh unit tests ==="
echo ""

test_fresh_index_shows_building
echo ""
test_existing_index_shows_updating
echo ""
test_phase_order
echo ""
test_rag_setup_failure_continues
echo ""
test_rag_setup_stderr_visible
echo ""
test_background_rag_index
echo ""
test_lock_prevents_concurrent_rag_index
echo ""
test_lock_cleaned_after_rag_index
echo ""
test_stale_lock_removed
echo ""
test_output_messages
echo ""
test_no_cache_triggers_fetch
echo ""
test_fresh_cache_skips_fetch
echo ""
test_stale_cache_triggers_fetch
echo ""
test_same_version_no_notice
echo ""
test_newer_installed_no_notice
echo ""
test_fetch_failure_continues

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

[ "$FAIL" -eq 0 ] || exit 1
