#!/usr/bin/env bash
set -euo pipefail

# Unit tests for ensure_indexed.sh
# Uses mocked cartog and install.sh to verify phase ordering and the
# install / self-update branches.
#
# Usage: bash skills/cartog/tests/test_ensure_indexed.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENSURE_SCRIPT="$SKILL_DIR/scripts/ensure_indexed.sh"
REAL_INSTALL="$SKILL_DIR/scripts/install.sh"

PASS=0
FAIL=0
TEST_DIR=""

# --- helpers ---

setup() {
    TEST_DIR=$(mktemp -d)
    mkdir -p "$TEST_DIR/bin" "$TEST_DIR/workdir"
    export CARTOG_TEST_LOG="$TEST_DIR/commands.log"
    : > "$CARTOG_TEST_LOG"
    export CARTOG_LOCK_DIR="$TEST_DIR/rag-index.lock"
    export CARTOG_LOG_DIR="$TEST_DIR/log"
    # Default plugin.json fixture — tests can override via write_plugin_json.
    write_plugin_json "0.14.1"
    export CARTOG_PLUGIN_JSON="$TEST_DIR/plugin.json"
    # Default `.cartog.toml` so tests exercise the indexed-project path.
    # Tests that exercise the missing-toml gate must remove this file
    # (or override CARTOG_AUTO_INIT) explicitly.
    : > "$TEST_DIR/workdir/.cartog.toml"
}

teardown() {
    local i=0
    while [ -d "${CARTOG_LOCK_DIR:-}" ] && [ "$i" -lt 30 ]; do
        sleep 0.1
        i=$((i + 1))
    done
    rmdir "${CARTOG_LOCK_DIR:-}" 2>/dev/null || true
    [ -n "$TEST_DIR" ] && rm -rf "$TEST_DIR"
    unset CARTOG_PLUGIN_JSON
    unset CARTOG_LOG_DIR
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

write_plugin_json() {
    local version="$1"
    cat > "$TEST_DIR/plugin.json" <<JSON
{ "name": "cartog", "version": "$version" }
JSON
}

# Mock cartog: logs every invocation; supports --version, index, rag setup,
# rag index, and `self update`. self_update_exit lets us simulate failures.
create_mock_cartog() {
    local mock_version="${1:-0.14.1}"
    local rag_setup_exit="${2:-0}"
    local rag_setup_stderr="${3:-}"
    local self_update_exit="${4:-0}"
    # $5 (optional): a pending_update target_version reported by
    # `self version --json`, so drift tests can exercise the pending-aware path.
    local pending_target="${5:-}"
    cat > "$TEST_DIR/bin/cartog" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "--version" ]; then
    echo "cartog $mock_version"
    exit 0
fi
if [ "\$1" = "self" ] && [ "\$2" = "version" ]; then
    # Mirror the real binary's serde_json::to_string_pretty output (one key per
    # line, nested block) so the hook's sed parse is exercised against the
    # shape it sees in production, not a single-line shortcut.
    if [ -n "$pending_target" ]; then
        cat <<JSON
{
  "version": "$mock_version",
  "target": "some-triple",
  "install_source": "release-tarball",
  "last_update_check": null,
  "pending_update": {
    "target_version": "$pending_target",
    "armed_from": "$mock_version"
  }
}
JSON
    else
        cat <<JSON
{
  "version": "$mock_version",
  "target": "some-triple",
  "install_source": "release-tarball",
  "last_update_check": null
}
JSON
    fi
    exit 0
fi
echo "\$@" >> "$CARTOG_TEST_LOG"
if [ "\$1" = "index" ]; then
    exit 0
elif [ "\$1" = "rag" ] && [ "\$2" = "setup" ]; then
    if [ -n "$rag_setup_stderr" ]; then echo "$rag_setup_stderr" >&2; fi
    exit $rag_setup_exit
elif [ "\$1" = "rag" ] && [ "\$2" = "index" ]; then
    sleep 0.1
    exit 0
elif [ "\$1" = "self" ] && [ "\$2" = "update" ]; then
    if [ "$self_update_exit" -ne 0 ]; then
        echo "self update mock failure" >&2
    else
        echo "cartog updated"
    fi
    exit $self_update_exit
fi
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"
}

# Replace install.sh in the skill scripts dir with a stub for the duration
# of one test. The stub creates a mock cartog binary on first run, simulating
# a successful bootstrap. Pass exit=non-zero to simulate install failure.
shadow_install_sh() {
    local exit_code="${1:-0}"
    local installed_version="${2:-0.14.1}"
    local install_log="$TEST_DIR/install.log"
    : > "$install_log"
    cp "$REAL_INSTALL" "$TEST_DIR/install.sh.bak"
    cat > "$REAL_INSTALL" <<STUB
#!/usr/bin/env bash
# Log args verbatim so tests can assert pinning behavior.
printf 'install.sh args=[%s] exit=$exit_code\n' "\$*" >> "$install_log"
if [ "$exit_code" -ne 0 ]; then
    echo "install.sh: simulated failure" >&2
    exit $exit_code
fi
cat > "$TEST_DIR/bin/cartog" <<INNER
#!/usr/bin/env bash
if [ "\\\$1" = "--version" ]; then echo "cartog $installed_version"; exit 0; fi
echo "\\\$@" >> "$CARTOG_TEST_LOG"
if [ "\\\$1" = "rag" ] && [ "\\\$2" = "index" ]; then sleep 0.1; fi
exit 0
INNER
chmod +x "$TEST_DIR/bin/cartog"
exit 0
STUB
    chmod +x "$REAL_INSTALL"
}

restore_install_sh() {
    if [ -f "$TEST_DIR/install.sh.bak" ]; then
        mv "$TEST_DIR/install.sh.bak" "$REAL_INSTALL"
    fi
}

run_ensure_indexed() {
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    (
        # Hermetic PATH: only the test bin + minimal system core (for stat/date/sed/etc.).
        # Excluding $PATH prevents a developer-installed `cartog` (e.g. ~/.cargo/bin/cartog)
        # from leaking in and making "missing binary" tests pass spuriously.
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        mkdir -p "$HOME"
        cd "$workdir"
        bash "$ENSURE_SCRIPT" 2>&1
    )
}

# Wait until the background pipeline finishes so log assertions are stable.
wait_for_rag_index() {
    local i=0
    while ! grep -q '^rag index ' "$CARTOG_TEST_LOG" 2>/dev/null && [ "$i" -lt 50 ]; do
        sleep 0.1
        i=$((i + 1))
    done
    # Also wait for the lock to release so subsequent tests don't race.
    i=0
    while [ -d "${CARTOG_LOCK_DIR:-}" ] && [ "$i" -lt 50 ]; do
        sleep 0.1
        i=$((i + 1))
    done
}

# Read the background session log (stdout/stderr from the background pipeline).
session_log() {
    local log_file="${CARTOG_LOG_DIR:-}/session.log"
    [ -f "$log_file" ] && cat "$log_file" || true
}

# --- tests: indexing phases (versions in sync, no install/update path) ---

test_fresh_index_shows_building() {
    echo "TEST: fresh index (no db) shows 'Building'"
    setup
    create_mock_cartog "0.14.1"
    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index
    assert_contains "shows 'Building'" "No cartog index found. Building..." "$output"
    teardown
}

test_existing_index_shows_updating() {
    echo "TEST: existing .cartog/db.sqlite shows 'Updating'"
    setup
    create_mock_cartog "0.14.1"
    mkdir -p "$TEST_DIR/workdir/.cartog"
    touch "$TEST_DIR/workdir/.cartog/db.sqlite"
    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index
    assert_contains "shows 'Updating'" "Updating cartog index..." "$output"
    teardown
}

test_legacy_db_file_shows_updating() {
    echo "TEST: legacy .cartog.db at root shows 'Updating'"
    setup
    create_mock_cartog "0.14.1"
    mkdir -p "$TEST_DIR/workdir"
    touch "$TEST_DIR/workdir/.cartog.db"
    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index
    assert_contains "shows 'Updating' for legacy" "Updating cartog index..." "$output"
    teardown
}

test_phase_order() {
    echo "TEST: commands run in correct order (foreground index, then background rag setup → rag index)"
    setup
    create_mock_cartog "0.14.1"

    run_ensure_indexed > /dev/null
    wait_for_rag_index

    # Foreground: index runs first.
    local line1
    line1=$(sed -n '1p' "$CARTOG_TEST_LOG")
    assert_eq "foreground: cartog index ." "index ." "$line1"

    # Background pipeline runs rag setup before rag index. Their relative position
    # in CARTOG_TEST_LOG is what matters (same mock writes both serially).
    local setup_line index_line
    setup_line=$(grep -nx 'rag setup' "$CARTOG_TEST_LOG" | head -1 | cut -d: -f1)
    index_line=$(grep -nx 'rag index .' "$CARTOG_TEST_LOG" | head -1 | cut -d: -f1)
    if [ -n "$setup_line" ] && [ -n "$index_line" ] && [ "$setup_line" -lt "$index_line" ]; then
        echo "  PASS: background: rag setup before rag index"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: rag setup ($setup_line) should precede rag index ($index_line)"
        FAIL=$((FAIL + 1))
    fi

    # SessionStart never invokes self update — that's the SessionEnd hook's job.
    if grep -qx 'self update' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: 'self update' ran during SessionStart; should be deferred to SessionEnd"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: 'self update' not invoked during SessionStart"
        PASS=$((PASS + 1))
    fi
    teardown
}

test_rag_setup_failure_continues() {
    echo "TEST: rag setup failure is logged but rag index still runs"
    setup
    create_mock_cartog "0.14.1" 1 "Error: model download failed"

    run_ensure_indexed > /dev/null
    wait_for_rag_index

    local log
    log=$(session_log)
    assert_contains "log notes B1 failure" "B1 failed" "$log"
    if grep -qx 'rag index .' "$CARTOG_TEST_LOG"; then
        echo "  PASS: rag index still runs after rag setup failure"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: rag index did not run after rag setup failure"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_rag_setup_stderr_in_session_log() {
    echo "TEST: rag setup stderr is captured in session log (not foreground stdout)"
    setup
    create_mock_cartog "0.14.1" 1 "Error: disk full"

    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index

    assert_not_contains "stderr NOT in foreground output" "Error: disk full" "$output"
    local log
    log=$(session_log)
    assert_contains "stderr captured in session log" "Error: disk full" "$log"
    teardown
}

test_session_log_created() {
    echo "TEST: session log directory and file are created"
    setup
    create_mock_cartog "0.14.1"

    run_ensure_indexed > /dev/null
    wait_for_rag_index

    if [ -f "$CARTOG_LOG_DIR/session.log" ]; then
        echo "  PASS: session log file exists at $CARTOG_LOG_DIR/session.log"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: session log not found at $CARTOG_LOG_DIR/session.log"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_last_error_surfaces_next_session() {
    echo "TEST: last-error file from previous session is surfaced and cleared"
    setup
    create_mock_cartog "0.14.1"
    mkdir -p "$CARTOG_LOG_DIR"
    echo "previous failure detail" > "$CARTOG_LOG_DIR/last-error"

    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index

    assert_contains "surfaces previous error" "previous failure detail" "$output"
    assert_contains "shows error header" "Previous cartog background task failed" "$output"
    if [ ! -f "$CARTOG_LOG_DIR/last-error" ]; then
        echo "  PASS: last-error file cleared after surfacing"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: last-error file still exists after surfacing"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_background_failure_writes_last_error() {
    echo "TEST: background pipeline failure writes last-error file"
    setup
    # rag setup AND rag index both fail
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "cartog 0.14.1"; exit 0; fi
echo "$@" >> "$CARTOG_TEST_LOG"
case "$1 $2" in
    "rag setup") echo "setup boom" >&2; exit 1 ;;
    "rag index") echo "index boom" >&2; exit 1 ;;
esac
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    run_ensure_indexed > /dev/null
    wait_for_rag_index

    if [ -f "$CARTOG_LOG_DIR/last-error" ]; then
        echo "  PASS: last-error file created on background failure"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: last-error file missing after background failure"
        FAIL=$((FAIL + 1))
    fi
    local last_error
    last_error=$(cat "$CARTOG_LOG_DIR/last-error" 2>/dev/null || echo "")
    assert_contains "last-error references session log" "session.log" "$last_error"
    teardown
}

test_background_rag_index() {
    echo "TEST: rag index runs in background (script returns before it finishes)"
    setup
    # Long-running rag index — script must return WELL before it finishes.
    # Use a generous gap (mock sleeps 10s, assertion <5s) so macOS bash
    # startup noise doesn't push elapsed onto the integer boundary.
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "cartog 0.14.1"; exit 0; fi
echo "$@" >> "$CARTOG_TEST_LOG"
if [ "$1" = "rag" ] && [ "$2" = "index" ]; then sleep 10; fi
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local start end elapsed
    start=$(date +%s)
    run_ensure_indexed > /dev/null
    end=$(date +%s)
    elapsed=$((end - start))

    if [ "$elapsed" -lt 5 ]; then
        echo "  PASS: script returned before background rag index finished (${elapsed}s < 5s)"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: script blocked on rag index (${elapsed}s >= 5s)"
        FAIL=$((FAIL + 1))
    fi
    # Clean up: kill background rag index and release lock so teardown is fast.
    pkill -f "rag index" 2>/dev/null || true
    rmdir "${CARTOG_LOCK_DIR:-}" 2>/dev/null || true
    teardown
}

test_index_runs_in_foreground() {
    echo "TEST: cartog index is recorded before script returns (proves foreground)"
    setup
    create_mock_cartog "0.14.1"

    run_ensure_indexed > /dev/null
    # Do NOT call wait_for_rag_index — we want to see what was logged synchronously.
    # The index command should already be in the log at this point.
    if grep -qx 'index .' "$CARTOG_TEST_LOG"; then
        echo "  PASS: 'index .' present immediately after script returned"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: 'index .' missing from log right after return — index not foreground?"
        FAIL=$((FAIL + 1))
    fi

    wait_for_rag_index
    teardown
}

test_lock_prevents_concurrent_background_pipeline() {
    echo "TEST: lock prevents concurrent background pipeline (second run skips)"
    setup
    create_mock_cartog "0.14.1"
    mkdir "$CARTOG_LOCK_DIR"

    local output
    output=$(run_ensure_indexed)

    assert_contains "skips background pipeline" "background pipeline already running" "$output"
    # Only the foreground 'index .' should have been recorded — no rag setup or rag index.
    local line_count
    line_count=$(wc -l < "$CARTOG_TEST_LOG" | tr -d ' ')
    assert_eq "only foreground index logged (no background pipeline)" "1" "$line_count"
    rmdir "$CARTOG_LOCK_DIR" 2>/dev/null || true
    teardown
}

test_lock_cleaned_after_rag_index() {
    echo "TEST: lock directory is removed after rag index completes"
    setup
    create_mock_cartog "0.14.1"

    run_ensure_indexed > /dev/null
    wait_for_rag_index

    if [ ! -d "$CARTOG_LOCK_DIR" ]; then
        echo "  PASS: lock directory cleaned up after completion"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: lock directory still exists after rag index finished"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_stale_lock_removed() {
    echo "TEST: stale lock (>1 hour) is removed and background pipeline proceeds"
    setup
    create_mock_cartog "0.14.1"

    mkdir "$CARTOG_LOCK_DIR"
    touch -t "$(date -v-2H '+%Y%m%d%H%M.%S' 2>/dev/null || date -d '2 hours ago' '+%Y%m%d%H%M.%S' 2>/dev/null)" "$CARTOG_LOCK_DIR"

    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index

    assert_contains "detects stale lock" "Removing stale cartog background lock" "$output"
    assert_contains "starts background pipeline" "cartog background tasks started" "$output"
    if grep -qx 'rag index .' "$CARTOG_TEST_LOG"; then
        echo "  PASS: rag index runs after stale lock removal"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: rag index did not run after stale lock removal"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_output_messages() {
    echo "TEST: foreground output mentions background PID and index-ready status"
    setup
    create_mock_cartog "0.14.1"

    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index

    assert_contains "mentions background PID" "cartog background tasks started" "$output"
    assert_contains "mentions index ready" "cartog index ready" "$output"
    teardown
}

# --- tests: missing binary → background install.sh ---
#
# Contract: hook returns fast (exit 0), install runs in the background
# subshell, install + index + RAG chain logged to session.log. Failures
# land in last-error and surface on the next session. MCP cannot work
# this session — /cartog-install is the explicit repair verb.

test_missing_binary_forks_install_in_background() {
    echo "TEST: missing cartog binary forks install in background (hook exits fast)"
    setup
    write_plugin_json "0.14.1"
    shadow_install_sh 0 "0.14.1"

    local output rc
    output=$(run_ensure_indexed) && rc=0 || rc=$?
    wait_for_rag_index
    restore_install_sh

    assert_eq "hook exits 0 (install runs in background)" "0" "$rc"
    assert_contains "announces background install" "Installing in background" "$output"
    assert_contains "points at /cartog-install" "/cartog-install" "$output"
    assert_file_exists "install.sh ran" "$TEST_DIR/install.log"
    # Regression guard for fix #1: install.sh must be pinned to PLUGIN_VERSION,
    # not invoked bare (which would install the latest GitHub release and
    # cause drift on session 2).
    assert_contains "install.sh pinned to plugin version" \
        "args=[0.14.1]" "$(cat "$TEST_DIR/install.log")"
    # Background pipeline writes session.log; install.sh + index logged there.
    local log
    log=$(session_log)
    assert_contains "install logged" "B0: install.sh" "$log"
    # With .cartog.toml present (default in setup), B1 should have run.
    if grep -qx 'index .' "$CARTOG_TEST_LOG"; then
        echo "  PASS: B1 (cartog index) ran after install (toml present)"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: B1 (cartog index) did not run despite toml present"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_missing_binary_no_toml_skips_index_phases() {
    echo "TEST: missing binary + no .cartog.toml → install runs but B1/B2/B3 are skipped (fix #4)"
    setup
    write_plugin_json "0.14.1"
    rm -f "$TEST_DIR/workdir/.cartog.toml"
    shadow_install_sh 0 "0.14.1"

    local rc
    run_ensure_indexed >/dev/null && rc=0 || rc=$?
    wait_for_rag_index
    restore_install_sh

    assert_eq "hook exits 0" "0" "$rc"
    assert_file_exists "install.sh still ran (binary needs to land)" "$TEST_DIR/install.log"
    # Crucially: no cartog index / rag setup / rag index commands ran —
    # the install pipeline must respect the no-toml gate.
    if grep -qx 'index .' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: cartog index ran despite no .cartog.toml (gate bypassed)"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: B1 skipped — no-toml gate respected by install pipeline"
        PASS=$((PASS + 1))
    fi
    if grep -q '^rag' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: rag commands ran despite no .cartog.toml"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: B2/B3 skipped — no-toml gate respected"
        PASS=$((PASS + 1))
    fi
    local log
    log=$(session_log)
    assert_contains "session log mentions no-toml gate" "no-toml gate active" "$log"
    teardown
}

test_missing_binary_lock_held_records_last_error() {
    echo "TEST: missing binary + lock held → hook exits 0 AND writes last-error (fix #5)"
    setup
    shadow_install_sh 0 "0.14.1"
    # Pre-existing lock simulates a concurrent or crashed-but-recent session.
    mkdir "$CARTOG_LOCK_DIR"

    local output rc
    output=$(run_ensure_indexed 2>&1) && rc=0 || rc=$?
    restore_install_sh

    assert_eq "hook exits 0" "0" "$rc"
    assert_contains "user-visible warning about concurrent session" \
        "Another cartog session is already" "$output"
    assert_file_exists "last-error written so next session surfaces it" \
        "$CARTOG_LOG_DIR/last-error"
    # No install.sh ran (lock held the fork off).
    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: install.sh ran despite lock contention"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: install.sh skipped (lock held)"
        PASS=$((PASS + 1))
    fi
    rmdir "$CARTOG_LOCK_DIR" 2>/dev/null || true
    teardown
}

test_fork_background_rejects_undefined_function() {
    echo "TEST: fork_background fails loudly when dispatched function is undefined (fix #6)"
    setup
    create_mock_cartog "0.14.1"

    # Invoke fork_background directly through the script's source so we can
    # exercise the declare -F guard without changing the public flow. Run in
    # a subshell, source the script up to the function definitions, then
    # call fork_background with a typo'd name.
    local err rc
    err=$(
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        mkdir -p "$HOME" "$TEST_DIR/workdir"
        cd "$TEST_DIR/workdir"
        # Strip the foreground-execution tail (after "--- Foreground execution starts here ---")
        # so we can source the function definitions without running them.
        awk '/^# --- Foreground execution starts here ---$/{exit} 1' "$ENSURE_SCRIPT" > "$TEST_DIR/fns.sh"
        # shellcheck disable=SC1090
        source "$TEST_DIR/fns.sh"
        fork_background run_typo_does_not_exist 2>&1 >/dev/null
    ) && rc=0 || rc=$?

    assert_eq "fork_background returns non-zero on undefined function" "2" "$rc"
    assert_contains "stderr names the missing function" "run_typo_does_not_exist" "$err"
    assert_file_exists "last-error written for the typo" \
        "$CARTOG_LOG_DIR/last-error"
    teardown
}

test_missing_binary_install_failure_records_last_error() {
    echo "TEST: install.sh failure in background writes last-error"
    setup
    shadow_install_sh 17

    local rc
    run_ensure_indexed >/dev/null && rc=0 || rc=$?
    wait_for_rag_index
    restore_install_sh

    # Hook still exits 0 — background failure is a soft signal.
    assert_eq "hook exits 0 even on background install failure" "0" "$rc"
    assert_file_exists "last-error written on background failure" \
        "$CARTOG_LOG_DIR/last-error"
    # No cartog commands ran (binary was never installed).
    if [ -s "$CARTOG_TEST_LOG" ]; then
        echo "  FAIL: cartog commands ran despite install failure"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: no cartog commands ran after install failure"
        PASS=$((PASS + 1))
    fi
    teardown
}

# --- tests: PATH probe after install.sh writes to a non-PATH directory ---

# Helper: stub install.sh to drop the binary into a target dir (off the test
# PATH), simulating install.sh's pick_install_dir choosing ~/.local/bin or a
# CARTOG_INSTALL_DIR override.
shadow_install_sh_to_dir() {
    local target_dir="$1"
    local installed_version="${2:-0.14.1}"
    local install_log="$TEST_DIR/install.log"
    : > "$install_log"
    cp "$REAL_INSTALL" "$TEST_DIR/install.sh.bak"
    cat > "$REAL_INSTALL" <<STUB
#!/usr/bin/env bash
printf 'install.sh args=[%s] target=$target_dir\n' "\$*" >> "$install_log"
mkdir -p "$target_dir"
cat > "$target_dir/cartog" <<INNER
#!/usr/bin/env bash
if [ "\\\$1" = "--version" ]; then echo "cartog $installed_version"; exit 0; fi
echo "\\\$@" >> "$CARTOG_TEST_LOG"
if [ "\\\$1" = "rag" ] && [ "\\\$2" = "index" ]; then sleep 0.1; fi
exit 0
INNER
chmod +x "$target_dir/cartog"
exit 0
STUB
    chmod +x "$REAL_INSTALL"
}

test_install_to_local_bin_recovered_via_path_probe() {
    echo "TEST: install.sh drops binary in ~/.local/bin → background pipeline probes PATH and indexes"
    setup
    write_plugin_json "0.14.1"
    # Default setup() already created $TEST_DIR/workdir/.cartog.toml, so the
    # background pipeline's do_index=1 path runs B1 after install.sh + PATH
    # probe — that's exactly what this test exercises.
    shadow_install_sh_to_dir "$TEST_DIR/home/.local/bin" "0.14.1"

    local rc
    run_ensure_indexed >/dev/null && rc=0 || rc=$?
    wait_for_rag_index
    restore_install_sh

    assert_eq "hook exits 0" "0" "$rc"
    assert_file_exists "install.sh ran" "$TEST_DIR/install.log"
    # install.sh was called with the pinned version (fix #1 regression guard).
    assert_contains "install.sh pinned to plugin version" \
        "args=[0.14.1]" "$(cat "$TEST_DIR/install.log")"
    # The background pipeline should have indexed once cartog landed on PATH.
    if grep -qx 'index .' "$CARTOG_TEST_LOG"; then
        echo "  PASS: background pipeline indexed via PATH probe"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: background pipeline did not index (PATH probe failed?)"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_install_to_unreachable_dir_records_last_error() {
    echo "TEST: install.sh writes binary outside probe candidates → background pipeline records last-error"
    setup
    write_plugin_json "0.14.1"
    # Default `.cartog.toml` from setup() is fine — the pipeline aborts at
    # the PATH probe (B0 fails to find cartog on PATH) before B1 runs, so
    # toml state doesn't matter here. The probe-failure last-error is the
    # asserted outcome.
    # Drop in a dir nothing probes (not ~/.local/bin, not ~/.cargo/bin, no override).
    shadow_install_sh_to_dir "$TEST_DIR/totally-isolated/bin" "0.14.1"

    local rc
    run_ensure_indexed >/dev/null && rc=0 || rc=$?
    wait_for_rag_index
    restore_install_sh

    assert_eq "hook still exits 0" "0" "$rc"
    assert_file_exists "last-error written on PATH probe failure" \
        "$CARTOG_LOG_DIR/last-error"
    local log
    log=$(session_log)
    assert_contains "session log explains PATH problem" \
        "still not on PATH" "$log"
    teardown
}

# --- tests: drift warning (passive — actual update happens in SessionEnd hook) ---

test_drift_warning_emitted_when_versions_differ() {
    echo "TEST: drift warning emitted on STDOUT (visible to user) when installed < plugin"
    setup
    write_plugin_json "0.14.3"
    create_mock_cartog "0.14.1"

    local output
    output=$(run_ensure_indexed 2>&1)
    wait_for_rag_index

    assert_contains "warns about version drift" "out of sync with plugin 0.14.3" "$output"
    assert_contains "points at /cartog-install" "run /cartog-install to update" "$output"

    # Visibility guarantee: the notice MUST be on stdout. Claude Code injects a
    # SessionStart hook's stdout into the model's context (so the user sees it)
    # but DISCARDS stderr at exit 0. Capture stdout only (stderr → /dev/null)
    # and assert the line is still there — a regression to `>&2` would make the
    # warning invisible and fail here.
    setup
    write_plugin_json "0.14.3"
    create_mock_cartog "0.14.1"
    local stdout_only
    stdout_only=$(
        (
            export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
            export HOME="$TEST_DIR/home"
            mkdir -p "$HOME"
            mkdir -p "$TEST_DIR/workdir"
            cd "$TEST_DIR/workdir"
            bash "$ENSURE_SCRIPT" 2>/dev/null
        )
    )
    wait_for_rag_index
    assert_contains "drift notice is on stdout (not the discarded stderr)" \
        "out of sync with plugin 0.14.3" "$stdout_only"

    # Crucially: SessionStart must NOT actually update.
    if grep -qx 'self update' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: 'self update' ran during SessionStart drift warning"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: 'self update' not invoked (drift only warned, not acted on)"
        PASS=$((PASS + 1))
    fi
    teardown
}

test_drift_warning_silent_when_versions_match() {
    echo "TEST: no drift warning when installed == plugin version"
    setup
    write_plugin_json "0.14.1"
    create_mock_cartog "0.14.1"

    local output
    output=$(run_ensure_indexed 2>&1)
    wait_for_rag_index

    assert_not_contains "no drift warning" "out of sync with plugin" "$output"
    teardown
}

test_drift_warning_silent_when_binary_newer_than_pin() {
    echo "TEST: no drift warning when installed binary is NEWER than the plugin pin"
    setup
    # Installed 0.21.0 (e.g. a manual install) is ahead of the plugin's 0.14.3.
    # The pin is a floor, not a ceiling — do NOT nag the user to "update".
    write_plugin_json "0.14.3"
    create_mock_cartog "0.21.0"

    local output
    output=$(run_ensure_indexed 2>&1)
    wait_for_rag_index

    assert_not_contains "no drift warning when ahead of pin" "out of sync with plugin" "$output"
    assert_not_contains "does not nag /cartog-install when ahead" "run /cartog-install to update" "$output"
    teardown
}

test_drift_warning_silent_when_no_plugin_json() {
    echo "TEST: no drift warning when plugin.json is missing"
    setup
    rm -f "$TEST_DIR/plugin.json"
    create_mock_cartog "0.14.1"

    local output
    output=$(run_ensure_indexed 2>&1)
    wait_for_rag_index

    assert_not_contains "no drift warning" "out of sync with plugin" "$output"
    teardown
}

test_drift_warning_pending_aware_when_armed() {
    echo "TEST: drift line says 'applied when session ends' when a matching update is armed"
    setup
    write_plugin_json "0.14.3"
    # Installed 0.14.1, but a deferred update to 0.14.3 (== plugin) is armed.
    create_mock_cartog "0.14.1" 0 "" 0 "0.14.3"

    local output
    output=$(run_ensure_indexed 2>&1)
    wait_for_rag_index

    assert_contains "announces pending apply" "will be applied when this session ends" "$output"
    assert_not_contains "does not nag /cartog-install" "run /cartog-install to update" "$output"
    teardown
}

test_drift_warning_acknowledges_stale_armed_after_repin() {
    echo "TEST: drift line acknowledges a stale armed target when the plugin pin moved"
    setup
    # Plugin re-pinned to 0.21.0, but an older deferred update to 0.20.0 is armed.
    write_plugin_json "0.21.0"
    create_mock_cartog "0.19.0" 0 "" 0 "0.20.0"

    local output
    output=$(run_ensure_indexed 2>&1)
    wait_for_rag_index

    assert_contains "names the stale armed target" "deferred update to 0.20.0 armed" "$output"
    assert_contains "names the new pin" "plugin now wants 0.21.0" "$output"
    assert_contains "points at re-arm" "/cartog-install to re-arm" "$output"
    teardown
}

test_last_update_surfaced_and_cleared() {
    echo "TEST: last-update breadcrumb is surfaced and cleared"
    setup
    create_mock_cartog "0.14.1"
    mkdir -p "$CARTOG_LOG_DIR"
    echo "cartog updated to 0.14.3." > "$CARTOG_LOG_DIR/last-update"

    local output
    output=$(run_ensure_indexed 2>&1)
    wait_for_rag_index

    assert_contains "surfaces completed update" "cartog updated to 0.14.3." "$output"
    if [ ! -f "$CARTOG_LOG_DIR/last-update" ]; then
        echo "  PASS: last-update file cleared after surfacing"; PASS=$((PASS + 1))
    else
        echo "  FAIL: last-update file still exists after surfacing"; FAIL=$((FAIL + 1))
    fi
    teardown
}

# --- tests: .cartog.toml DB path resolution ---
#
# These tests inject `echo "DB_FILE=$DB_FILE"` right before phase 1 (the
# `cartog index .` line) so we can capture the resolved DB path without
# running the indexing phases.
run_ensure_indexed_print_db() {
    local workdir="$1"
    shift
    (
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        mkdir -p "$HOME"
        "$@"
        cd "$workdir"
        # Patch: insert echo just before phase 1. The real line is
        # `CARTOG_PROGRESS=1 cartog index . || index_rc=$?` (the env prefix
        # opts into the non-TTY progress heartbeat); earlier forms were
        # `cartog index . || index_rc=$?` and bare `cartog index .`. The
        # optional `(CARTOG_PROGRESS=1 )?` prefix keeps the patch matching
        # across all of them.
        # The replacement needs a literal newline — `\n` in a sed RHS is a
        # GNU extension and is taken literally by BSD/macOS sed. Use a
        # backslash-continued newline inside the -e argument, which is POSIX.
        sed \
            -e 's#^\(CARTOG_PROGRESS=1 \)\{0,1\}cartog index \. || index_rc=\$?$#echo "DB_FILE=$DB_FILE"\
exit 0#' \
            -e 's#^\(CARTOG_PROGRESS=1 \)\{0,1\}cartog index \.$#echo "DB_FILE=$DB_FILE"\
exit 0#' \
            "$ENSURE_SCRIPT" | bash 2>&1
    )
}

test_toml_cwd_database_path() {
    echo "TEST: .cartog.toml in cwd sets DB_FILE from database.path"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    cat > "$workdir/.cartog.toml" <<'TOML'
[database]
path = "/custom/my.db"
TOML

    local output
    output=$(run_ensure_indexed_print_db "$workdir")

    assert_contains "uses toml path" "DB_FILE=/custom/my.db" "$output"
    teardown
}

test_toml_git_root_database_path() {
    echo "TEST: .cartog.toml at git root sets DB_FILE"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir/subdir"

    cat > "$TEST_DIR/bin/git" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "rev-parse" ] && [ "\$2" = "--show-toplevel" ]; then
    echo "$workdir"; exit 0
fi
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    cat > "$workdir/.cartog.toml" <<'TOML'
[database]
path = "/root-level/cartog.db"
TOML

    local output
    output=$(run_ensure_indexed_print_db "$workdir/subdir")

    assert_contains "uses git root toml" "DB_FILE=/root-level/cartog.db" "$output"
    teardown
}

test_toml_tilde_expansion() {
    echo "TEST: .cartog.toml path with ~/ expands to HOME"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    cat > "$workdir/.cartog.toml" <<'TOML'
[database]
path = "~/projects/my.db"
TOML

    local output
    output=$(run_ensure_indexed_print_db "$workdir")

    assert_contains "tilde expanded" "DB_FILE=$TEST_DIR/home/projects/my.db" "$output"
    teardown
}

test_cartog_db_env_overrides_toml() {
    echo "TEST: CARTOG_DB env var overrides .cartog.toml"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    cat > "$workdir/.cartog.toml" <<'TOML'
[database]
path = "/toml/path.db"
TOML

    local output
    output=$(run_ensure_indexed_print_db "$workdir" export CARTOG_DB="/env/override.db")

    assert_contains "env overrides toml" "DB_FILE=/env/override.db" "$output"
    teardown
}

test_no_toml_falls_back_to_git_root() {
    echo "TEST: no .cartog.toml falls back to git root .cartog/db.sqlite"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"

    cat > "$TEST_DIR/bin/git" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "rev-parse" ] && [ "\$2" = "--show-toplevel" ]; then
    echo "$workdir"; exit 0
fi
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    local output
    output=$(run_ensure_indexed_print_db "$workdir")

    assert_contains "falls back to git root" "DB_FILE=$workdir/.cartog/db.sqlite" "$output"
    teardown
}

test_legacy_root_db_used_when_only_legacy_exists() {
    echo "TEST: legacy .cartog.db at git root is picked up when new layout missing"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    touch "$workdir/.cartog.db"

    cat > "$TEST_DIR/bin/git" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "rev-parse" ] && [ "\$2" = "--show-toplevel" ]; then
    echo "$workdir"; exit 0
fi
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    local output
    output=$(run_ensure_indexed_print_db "$workdir")

    assert_contains "legacy root path" "DB_FILE=$workdir/.cartog.db" "$output"
    teardown
}

test_new_layout_wins_over_legacy() {
    echo "TEST: .cartog/db.sqlite wins when both layouts exist"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir/.cartog"
    touch "$workdir/.cartog.db"
    touch "$workdir/.cartog/db.sqlite"

    cat > "$TEST_DIR/bin/git" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "rev-parse" ] && [ "\$2" = "--show-toplevel" ]; then
    echo "$workdir"; exit 0
fi
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    local output
    output=$(run_ensure_indexed_print_db "$workdir")

    assert_contains "new layout wins" "DB_FILE=$workdir/.cartog/db.sqlite" "$output"
    teardown
}

# --- tests: fresh-repo gate (no .cartog.toml) ---
#
# Non-TTY sessions (CI, piped, hooks running under harness): exit silently
# without indexing. This is the contract — auto-indexing with defaults
# would write to a location the user didn't pick.
#
# TTY sessions: print a hint pointing at `cartog init`, no indexing.
#
# CARTOG_AUTO_INIT=1: bypass everything and index with defaults (opt-in).

test_no_toml_non_tty_exits_silently() {
    echo "TEST: missing .cartog.toml + non-TTY stdin -> silent exit (no index, no install)"
    setup
    create_mock_cartog "0.14.1"
    rm -f "$TEST_DIR/workdir/.cartog.toml"

    local output
    output=$(run_ensure_indexed)

    # No deferral hint (TTY-only), no index, no background pipeline.
    if echo "$output" | grep -q "Run \`cartog init\`"; then
        echo "  FAIL: TTY-only deferral hint fired on non-TTY session" >&2
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: no deferral hint on non-TTY"
        PASS=$((PASS + 1))
    fi
    if [ -s "$CARTOG_TEST_LOG" ]; then
        echo "  FAIL: cartog commands ran on non-TTY missing-toml session" >&2
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: no cartog commands ran"
        PASS=$((PASS + 1))
    fi
    teardown
}

test_no_toml_auto_init_env_proceeds_with_index() {
    echo "TEST: missing .cartog.toml + CARTOG_AUTO_INIT=1 -> indexes anyway (bypass)"
    setup
    create_mock_cartog "0.14.1"
    rm -f "$TEST_DIR/workdir/.cartog.toml"

    local output
    output=$(CARTOG_AUTO_INIT=1 run_ensure_indexed)
    wait_for_rag_index
    assert_contains "indexes when CARTOG_AUTO_INIT=1" \
        "cartog index ready" "$output"
    if echo "$output" | grep -q "Run \`cartog init\`"; then
        echo "  FAIL: deferral hint fired when CARTOG_AUTO_INIT=1" >&2
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: CARTOG_AUTO_INIT=1 suppresses deferral"
        PASS=$((PASS + 1))
    fi
    teardown
}

test_toml_present_indexes_normally() {
    echo "TEST: .cartog.toml present at git root -> indexes (no deferral)"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    echo "# user config" > "$workdir/.cartog.toml"

    cat > "$TEST_DIR/bin/git" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "rev-parse" ] && [ "\$2" = "--show-toplevel" ]; then
    echo "$workdir"; exit 0
fi
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index
    assert_contains "indexes when toml present" "cartog index ready" "$output"
    if echo "$output" | grep -q "Run \`cartog init\`"; then
        echo "  FAIL: deferral hint fired even though .cartog.toml exists" >&2
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: .cartog.toml presence skips the gate"
        PASS=$((PASS + 1))
    fi
    teardown
}

# --- tests: stress / edge cases (audit follow-up) ---

# 1. Two concurrent ensure_indexed invocations in the same project must not
#    both run the background pipeline. The lock dir mediates: one wins, the
#    other reports "already running".
test_concurrent_ensure_indexed_invocations() {
    echo "TEST: two concurrent runs — exactly one launches the background pipeline"
    setup
    # Mock cartog with a slow rag setup so the first invocation holds the lock
    # while the second one races.
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "cartog 0.14.1"; exit 0; fi
echo "$@" >> "$CARTOG_TEST_LOG"
if [ "$1" = "rag" ] && [ "$2" = "setup" ]; then sleep 2; fi
if [ "$1" = "rag" ] && [ "$2" = "index" ]; then sleep 0.1; fi
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local out_a="$TEST_DIR/run_a.out" out_b="$TEST_DIR/run_b.out"
    run_ensure_indexed > "$out_a" &
    local pid_a=$!
    # Small skew to ensure A reaches the mkdir lock first; B should observe it.
    sleep 0.2
    run_ensure_indexed > "$out_b" &
    local pid_b=$!
    wait "$pid_a" "$pid_b"
    wait_for_rag_index

    local a b
    a=$(cat "$out_a"); b=$(cat "$out_b")
    # Exactly one of the two outputs must mention "background pipeline already running".
    local skipped=0
    echo "$a" | grep -qF "background pipeline already running" && skipped=$((skipped + 1))
    echo "$b" | grep -qF "background pipeline already running" && skipped=$((skipped + 1))
    assert_eq "exactly one invocation skipped the background pipeline" "1" "$skipped"
    # Foreground index ran in BOTH invocations (idempotent).
    local index_count
    index_count=$(grep -c '^index \.$' "$CARTOG_TEST_LOG" || true)
    assert_eq "foreground index ran in both invocations" "2" "$index_count"
    teardown
}

# 2. ~/.cache/cartog not writable -> log dir falls back to /tmp without aborting.
test_readonly_cache_dir_fallback() {
    echo "TEST: unwritable XDG_CACHE_HOME falls back to /tmp for session log"
    setup
    create_mock_cartog "0.14.1"
    local tmp_log="/tmp/session.log"
    rm -f "$tmp_log" "/tmp/last-error"
    # cd $TEST_DIR (not $TEST_DIR/workdir) — ensure .cartog.toml is present there
    # so the no-toml gate doesn't short-circuit before the log-fallback path.
    : > "$TEST_DIR/.cartog.toml"

    local output
    output=$(
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        export XDG_CACHE_HOME="/dev/null"
        unset CARTOG_LOG_DIR
        mkdir -p "$HOME"
        cd "$TEST_DIR"
        bash "$ENSURE_SCRIPT" 2>&1
    )
    # Wait for the fallback /tmp/session.log to be written by the background pipeline.
    local i=0
    while [ ! -f "$tmp_log" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
    rmdir /tmp/cartog-rag-index.lock 2>/dev/null || true

    assert_contains "script completed despite unwritable cache" "cartog index ready" "$output"
    assert_file_exists "fallback session.log under /tmp" "$tmp_log"
    rm -f "$tmp_log" "/tmp/last-error"
    teardown
}

# 3. Malformed .cartog.toml does not crash the resolver; falls back to git-root.
test_malformed_toml_falls_back_safely() {
    echo "TEST: malformed .cartog.toml -> falls back to git-root default path"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    # Intentionally broken TOML: section header never closes, no quotes.
    cat > "$workdir/.cartog.toml" <<'TOML'
[database
path = unterminated
TOML

    cat > "$TEST_DIR/bin/git" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "rev-parse" ] && [ "\$2" = "--show-toplevel" ]; then
    echo "$workdir"; exit 0
fi
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    local output
    output=$(run_ensure_indexed_print_db "$workdir")

    # Resolver could not extract a path, so it falls back to git-root default.
    assert_contains "fallback to git-root default" \
        "DB_FILE=$workdir/.cartog/db.sqlite" "$output"
    teardown
}

# 4. TOML points the DB at a directory the user cannot write to. The script
#    must surface the index failure (LAST_ERROR_FILE) and still launch the
#    background pipeline — regression guard for the BLOCKER fix.
test_toml_db_path_points_to_unwritable_dir() {
    echo "TEST: TOML db path on unwritable dir — index fails, background still launches"
    setup
    # Mock cartog: index exits non-zero, rag setup + rag index succeed.
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "cartog 0.14.1"; exit 0; fi
echo "$@" >> "$CARTOG_TEST_LOG"
if [ "$1" = "index" ]; then
    echo "permission denied: /forbidden/db.sqlite" >&2
    exit 2
fi
if [ "$1" = "rag" ] && [ "$2" = "index" ]; then sleep 0.1; fi
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    cat > "$workdir/.cartog.toml" <<'TOML'
[database]
path = "/forbidden/db.sqlite"
TOML

    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index

    assert_contains "index failure surfaced on stderr" \
        "cartog index failed" "$output"
    assert_file_exists "last-error written by foreground failure" \
        "$CARTOG_LOG_DIR/last-error"
    # Crucial: background pipeline must have run.
    if grep -q '^rag setup' "$CARTOG_TEST_LOG" && grep -q '^rag index ' "$CARTOG_TEST_LOG"; then
        echo "  PASS: background pipeline ran despite foreground index failure"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: background pipeline did not run after index failure"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

# 5. CARTOG_DB env var must take priority over .cartog.toml database.path.
test_cartog_db_env_vs_toml_priority() {
    echo "TEST: CARTOG_DB env wins over .cartog.toml on conflict"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    cat > "$workdir/.cartog.toml" <<'TOML'
[database]
path = "/tmp/from-toml.db"
TOML

    local output
    output=$(run_ensure_indexed_print_db "$workdir" export CARTOG_DB="/tmp/from-env.db")

    assert_contains "env path wins" "DB_FILE=/tmp/from-env.db" "$output"
    # TOML path must NOT appear as the resolved DB.
    if echo "$output" | grep -qF "DB_FILE=/tmp/from-toml.db"; then
        echo "  FAIL: TOML path leaked through despite CARTOG_DB env"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: TOML path not used when CARTOG_DB env is set"
        PASS=$((PASS + 1))
    fi
    teardown
}

# 6. Not in a git repo, but a .cartog.toml sits in cwd. Resolver must find it
#    via the "." branch of the resolve loop.
test_no_git_repo_with_toml_in_cwd() {
    echo "TEST: no git repo + .cartog.toml in cwd -> TOML path is honoured"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    cat > "$workdir/.cartog.toml" <<'TOML'
[database]
path = "/non-git/cwd.db"
TOML
    # git rev-parse fails — simulate "not a git repo".
    cat > "$TEST_DIR/bin/git" <<'MOCK'
#!/usr/bin/env bash
echo "fatal: not a git repository" >&2
exit 128
MOCK
    chmod +x "$TEST_DIR/bin/git"

    local output
    output=$(run_ensure_indexed_print_db "$workdir")

    assert_contains "TOML in cwd resolved without git" "DB_FILE=/non-git/cwd.db" "$output"
    teardown
}

# Regression guard for fix #8: setting CARTOG_DB used to skip the GIT_ROOT
# resolver, so the no-toml gate run from a subdir would miss a git-root
# .cartog.toml and exit silently on non-TTY sessions.
test_cartog_db_set_does_not_skip_git_root_toml() {
    echo "TEST: CARTOG_DB set + cwd in git subdir -> git-root .cartog.toml still satisfies the gate"
    setup
    create_mock_cartog "0.14.1"
    local repo="$TEST_DIR/workdir"
    local subdir="$repo/src"
    mkdir -p "$subdir"
    # Remove the default workdir toml — we want the toml ONLY at the git
    # root, not at the cwd, to prove GIT_ROOT is resolved even with CARTOG_DB.
    rm -f "$repo/.cartog.toml"
    echo "# user config" > "$repo/.cartog.toml"

    cat > "$TEST_DIR/bin/git" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "rev-parse" ] && [ "\$2" = "--show-toplevel" ]; then
    echo "$repo"; exit 0
fi
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    local output
    output=$(
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        export CARTOG_DB="/explicit/path.db"
        mkdir -p "$HOME"
        cd "$subdir"
        bash "$ENSURE_SCRIPT" 2>&1
    )
    wait_for_rag_index

    # Gate must NOT have fired: we should see indexing happen, not a silent exit.
    assert_contains "indexing happened from subdir despite CARTOG_DB" \
        "cartog index ready" "$output"
    if echo "$output" | grep -q "Run \`cartog init\`"; then
        echo "  FAIL: no-toml hint fired despite git-root toml present"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: no-toml hint did not fire (gate found git-root toml)"
        PASS=$((PASS + 1))
    fi
    teardown
}

# 7. .cartog/db.sqlite is a symlink to elsewhere. The "Updating cartog index"
#    branch (not "Building") must fire — `[ -f ... ]` follows symlinks.
test_symlinked_db_path() {
    echo "TEST: .cartog/db.sqlite as a symlink — 'Updating' message, not 'Building'"
    setup
    create_mock_cartog "0.14.1"
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir/.cartog"
    # Real DB file lives elsewhere; .cartog/db.sqlite points at it.
    local real_db="$TEST_DIR/real-db.sqlite"
    touch "$real_db"
    ln -s "$real_db" "$workdir/.cartog/db.sqlite"

    cat > "$TEST_DIR/bin/git" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "rev-parse" ] && [ "\$2" = "--show-toplevel" ]; then
    echo "$workdir"; exit 0
fi
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    local output
    output=$(
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        mkdir -p "$HOME"
        cd "$workdir"
        bash "$ENSURE_SCRIPT" 2>&1
    )
    wait_for_rag_index

    assert_contains "symlinked DB is treated as existing" "Updating cartog index" "$output"
    if echo "$output" | grep -qF "No cartog index found. Building"; then
        echo "  FAIL: symlink target ignored — script printed 'Building'"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: symlink target detected; no 'Building' message"
        PASS=$((PASS + 1))
    fi
    # Symlink must still exist and point at the same target after the run.
    if [ -L "$workdir/.cartog/db.sqlite" ] && [ "$(readlink "$workdir/.cartog/db.sqlite")" = "$real_db" ]; then
        echo "  PASS: symlink preserved after index"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: symlink replaced or removed"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

# 8. Foreground `cartog index` fails — the background pipeline must still spawn.
#    Companion to test 4 but with no TOML, so the failure mode is purely about
#    index exit-code handling, not path resolution.
test_foreground_index_failure_still_launches_background() {
    echo "TEST: foreground index non-zero — background B1/B2 still run"
    setup
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "cartog 0.14.1"; exit 0; fi
echo "$@" >> "$CARTOG_TEST_LOG"
if [ "$1" = "index" ]; then exit 3; fi
if [ "$1" = "rag" ] && [ "$2" = "index" ]; then sleep 0.1; fi
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index

    assert_contains "index failure reported" "cartog index failed (exit 3)" "$output"
    # B1 + B2 must both have run.
    if grep -q '^rag setup' "$CARTOG_TEST_LOG"; then
        echo "  PASS: B1 (rag setup) ran after index failure"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: B1 (rag setup) did not run after index failure"
        FAIL=$((FAIL + 1))
    fi
    if grep -q '^rag index ' "$CARTOG_TEST_LOG"; then
        echo "  PASS: B2 (rag index) ran after index failure"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: B2 (rag index) did not run after index failure"
        FAIL=$((FAIL + 1))
    fi
    assert_file_exists "foreground failure recorded in last-error" \
        "$CARTOG_LOG_DIR/last-error"
    teardown
}

# 9. CARTOG_AUTO_INIT=1 + TTY should bypass the deferral gate, not be ANDed.
#    The gate is `[ no toml ] && [ no auto-init ] && [ tty ]`, so AUTO_INIT
#    alone must be sufficient to bypass even when the other two conditions hold.
test_auto_init_env_bypasses_gate_with_tty() {
    echo "TEST: CARTOG_AUTO_INIT=1 bypasses defer gate even when stdin is a TTY"
    setup
    create_mock_cartog "0.14.1"
    # No .cartog.toml, no git repo (so GIT_ROOT=""). setup() seeds a default
    # .cartog.toml in workdir — remove it so the gate condition is actually
    # exercised; otherwise the test passes via the toml-present branch
    # instead of via the AUTO_INIT bypass it claims to validate.
    local workdir="$TEST_DIR/workdir"
    mkdir -p "$workdir"
    rm -f "$workdir/.cartog.toml"
    cat > "$TEST_DIR/bin/git" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/git"

    # Attach /dev/tty to stdin if available (most CI environments don't provide one).
    # We approximate "TTY present" by feeding a here-string and relying on the
    # auto-init bypass — `[ -t 0 ]` will be false here, BUT the bypass should
    # fire on AUTO_INIT regardless. This also documents the OR semantics.
    local output
    output=$(
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        export CARTOG_AUTO_INIT=1
        mkdir -p "$HOME"
        cd "$workdir"
        bash "$ENSURE_SCRIPT" 2>&1
    )
    wait_for_rag_index

    assert_contains "AUTO_INIT bypass led to index ready" "cartog index ready" "$output"
    if echo "$output" | grep -qF "Run \`cartog init\`"; then
        echo "  FAIL: AUTO_INIT=1 did not suppress deferral hint"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: AUTO_INIT=1 suppressed deferral hint"
        PASS=$((PASS + 1))
    fi
    teardown
}

# 10. With an MCP `cartog serve` PID present (no peer-guard at index time),
#     ensure_indexed must still run the foreground index and the background
#     pipeline. Validates the "MCP alive + manual index" path SKILL.md promises.
test_concurrent_writer_during_active_mcp() {
    echo "TEST: simulated MCP peer alive — index + background pipeline still run"
    setup
    create_mock_cartog "0.14.1"
    # Create a fake state dir with a live PID file so peer_alive() (used by
    # update_on_exit) would see a running peer. ensure_indexed itself has NO
    # peer guard — this test asserts that absence: indexing proceeds anyway.
    local state_dir
    case "$(uname -s)" in
        Darwin) state_dir="$TEST_DIR/home/Library/Application Support/io.cartog.cartog" ;;
        Linux)  state_dir="$TEST_DIR/home/.local/state/cartog" ;;
        *)      state_dir="$TEST_DIR/home/.local/state/cartog" ;;
    esac
    mkdir -p "$state_dir"
    # PID 1 (init) is always alive — perfect liveness proxy on POSIX.
    echo "1" > "$state_dir/serve.pid"

    local output
    output=$(run_ensure_indexed)
    wait_for_rag_index

    assert_contains "foreground index ran with MCP peer alive" \
        "cartog index ready" "$output"
    # B1 + B2 must have run too.
    if grep -q '^rag setup' "$CARTOG_TEST_LOG" && grep -q '^rag index ' "$CARTOG_TEST_LOG"; then
        echo "  PASS: background pipeline ran with MCP peer alive"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: background pipeline blocked by MCP peer (regression)"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

# --- run all tests ---

echo "=== ensure_indexed.sh unit tests ==="
echo ""

test_fresh_index_shows_building
echo ""
test_existing_index_shows_updating
echo ""
test_legacy_db_file_shows_updating
echo ""
test_phase_order
echo ""
test_index_runs_in_foreground
echo ""
test_rag_setup_failure_continues
echo ""
test_rag_setup_stderr_in_session_log
echo ""
test_session_log_created
echo ""
test_last_error_surfaces_next_session
echo ""
test_background_failure_writes_last_error
echo ""
test_background_rag_index
echo ""
test_lock_prevents_concurrent_background_pipeline
echo ""
test_lock_cleaned_after_rag_index
echo ""
test_stale_lock_removed
echo ""
test_output_messages
echo ""
test_missing_binary_forks_install_in_background
echo ""
test_missing_binary_no_toml_skips_index_phases
echo ""
test_missing_binary_lock_held_records_last_error
echo ""
test_fork_background_rejects_undefined_function
echo ""
test_missing_binary_install_failure_records_last_error
echo ""
test_install_to_local_bin_recovered_via_path_probe
echo ""
test_install_to_unreachable_dir_records_last_error
echo ""
test_drift_warning_emitted_when_versions_differ
echo ""
test_drift_warning_silent_when_versions_match
echo ""
test_drift_warning_silent_when_binary_newer_than_pin
echo ""
test_drift_warning_silent_when_no_plugin_json
echo ""
test_drift_warning_pending_aware_when_armed
echo ""
test_drift_warning_acknowledges_stale_armed_after_repin
echo ""
test_last_update_surfaced_and_cleared
echo ""
test_toml_cwd_database_path
echo ""
test_toml_git_root_database_path
echo ""
test_toml_tilde_expansion
echo ""
test_cartog_db_env_overrides_toml
echo ""
test_no_toml_falls_back_to_git_root
echo ""
test_legacy_root_db_used_when_only_legacy_exists
echo ""
test_new_layout_wins_over_legacy
echo ""
test_no_toml_non_tty_exits_silently
echo ""
test_no_toml_auto_init_env_proceeds_with_index
echo ""
test_toml_present_indexes_normally
echo ""
test_concurrent_ensure_indexed_invocations
echo ""
test_readonly_cache_dir_fallback
echo ""
test_malformed_toml_falls_back_safely
echo ""
test_toml_db_path_points_to_unwritable_dir
echo ""
test_cartog_db_env_vs_toml_priority
echo ""
test_no_git_repo_with_toml_in_cwd
echo ""
test_cartog_db_set_does_not_skip_git_root_toml
echo ""
test_symlinked_db_path
echo ""
test_foreground_index_failure_still_launches_background
echo ""
test_auto_init_env_bypasses_gate_with_tty
echo ""
test_concurrent_writer_during_active_mcp

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

[ "$FAIL" -eq 0 ] || exit 1
