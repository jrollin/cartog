#!/usr/bin/env bash
set -euo pipefail

# Unit tests for update_on_exit.sh (SessionEnd hook, transitional).
#
# Contract: only the pre-self-update cohort (<0.14.0) is upgraded by this
# hook. Modern binaries (>=0.14.0) are a noop — drift is the user's call
# via /cartog-install.
#
# Usage: bash skills/cartog/tests/test_update_on_exit.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
UPDATE_SCRIPT="$SKILL_DIR/scripts/update_on_exit.sh"
REAL_INSTALL="$SKILL_DIR/scripts/install.sh"

PASS=0
FAIL=0
TEST_DIR=""

setup() {
    TEST_DIR=$(mktemp -d)
    mkdir -p "$TEST_DIR/bin"
    export CARTOG_TEST_LOG="$TEST_DIR/commands.log"
    : > "$CARTOG_TEST_LOG"
    export CARTOG_LOG_DIR="$TEST_DIR/log"
    export CARTOG_LOCK_DIR="$TEST_DIR/rag-index.lock"
    export HOME="$TEST_DIR/home"
    mkdir -p "$HOME"
    write_plugin_json "0.14.3"
    export CARTOG_PLUGIN_JSON="$TEST_DIR/plugin.json"
}

teardown() {
    rmdir "${CARTOG_LOCK_DIR:-}" 2>/dev/null || true
    [ -n "$TEST_DIR" ] && rm -rf "$TEST_DIR"
    unset CARTOG_PLUGIN_JSON CARTOG_LOG_DIR CARTOG_TEST_LOG CARTOG_LOCK_DIR
}

assert_eq() {
    local label="$1" expected="$2" actual="$3"
    if [ "$expected" = "$actual" ]; then
        echo "  PASS: $label"; PASS=$((PASS + 1))
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
        echo "  PASS: $label"; PASS=$((PASS + 1))
    else
        echo "  FAIL: $label"
        echo "    expected to contain: $needle"
        echo "    actual: $haystack"
        FAIL=$((FAIL + 1))
    fi
}

write_plugin_json() {
    local version="$1"
    cat > "$TEST_DIR/plugin.json" <<JSON
{ "name": "cartog", "version": "$version" }
JSON
}

# Mock cartog: supports --version, logs other invocations.
create_mock_cartog() {
    local mock_version="${1:-0.14.1}"
    cat > "$TEST_DIR/bin/cartog" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "--version" ]; then
    echo "cartog $mock_version"
    exit 0
fi
echo "\$@" >> "$CARTOG_TEST_LOG"
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"
}

shadow_install_sh() {
    local exit_code="${1:-0}"
    local install_log="$TEST_DIR/install.log"
    : > "$install_log"
    cp "$REAL_INSTALL" "$TEST_DIR/install.sh.bak"
    cat > "$REAL_INSTALL" <<STUB
#!/usr/bin/env bash
printf 'install.sh args=[%s] exit=$exit_code\n' "\$*" >> "$install_log"
if [ "$exit_code" -ne 0 ]; then
    echo "install.sh: simulated failure" >&2
    exit $exit_code
fi
exit 0
STUB
    chmod +x "$REAL_INSTALL"
}

restore_install_sh() {
    [ -f "$TEST_DIR/install.sh.bak" ] && mv "$TEST_DIR/install.sh.bak" "$REAL_INSTALL"
}

run_update_on_exit() {
    (
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        bash "$UPDATE_SCRIPT" 2>&1
    )
}

session_log() {
    local f="${CARTOG_LOG_DIR:-}/session.log"
    [ -f "$f" ] && cat "$f" || true
}

# --- tests ---

test_missing_binary_is_noop() {
    echo "TEST: missing cartog binary exits cleanly without touching anything"
    setup

    local rc=0
    run_update_on_exit > /dev/null || rc=$?

    assert_eq "exits 0 silently" "0" "$rc"
    if [ -s "$CARTOG_TEST_LOG" ]; then
        echo "  FAIL: cartog commands logged when binary missing"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: no cartog commands run when binary missing"; PASS=$((PASS + 1))
    fi
    teardown
}

test_modern_binary_is_noop_even_when_outdated() {
    echo "TEST: modern binary (>=0.14.0) is a noop — drift left for /cartog-install"
    setup
    write_plugin_json "0.14.3"
    create_mock_cartog "0.14.0"
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    if grep -qx 'self update' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: 'self update' ran (transitional hook should not handle modern binaries)"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: 'self update' not invoked on modern binary"; PASS=$((PASS + 1))
    fi
    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: install.sh ran (transitional hook should not handle >=0.14.0)"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: install.sh not invoked on modern binary"; PASS=$((PASS + 1))
    fi
    teardown
}

test_modern_binary_in_sync_is_noop() {
    echo "TEST: modern binary in sync with plugin is a noop"
    setup
    write_plugin_json "0.14.3"
    create_mock_cartog "0.14.3"

    run_update_on_exit > /dev/null

    if grep -qx 'self update' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: 'self update' ran when versions matched"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: 'self update' not invoked when versions matched"; PASS=$((PASS + 1))
    fi
    teardown
}

test_legacy_binary_outdated_routes_to_install_sh() {
    echo "TEST: legacy binary (<0.14.0) outdated → install.sh upgrade with plugin version"
    setup
    write_plugin_json "0.14.3"
    create_mock_cartog "0.13.5"
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    local log
    log=$(session_log)
    assert_contains "log announces pre-self-update upgrade" \
        "Upgrading pre-self-update cartog 0.13.5 → 0.14.3" "$log"
    if [ -f "$TEST_DIR/install.log" ]; then
        echo "  PASS: install.sh invoked"; PASS=$((PASS + 1))
    else
        echo "  FAIL: install.sh not invoked"; FAIL=$((FAIL + 1))
    fi
    assert_contains "install.sh receives plugin version" \
        "args=[0.14.3]" "$(cat "$TEST_DIR/install.log")"
    if grep -qx 'self update' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: 'self update' called on legacy binary"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: 'self update' skipped on legacy binary"; PASS=$((PASS + 1))
    fi
    teardown
}

test_legacy_binary_in_sync_is_noop() {
    echo "TEST: legacy binary version == plugin is a noop (no install.sh call)"
    setup
    write_plugin_json "0.13.5"
    create_mock_cartog "0.13.5"
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: install.sh ran when legacy binary matched plugin"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: install.sh not invoked when versions matched"; PASS=$((PASS + 1))
    fi
    teardown
}

test_no_plugin_json_is_noop() {
    echo "TEST: missing plugin.json → noop (nothing to upgrade to)"
    setup
    rm -f "$TEST_DIR/plugin.json"
    create_mock_cartog "0.13.5"
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: install.sh ran without plugin.json"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: install.sh not invoked when PLUGIN_VERSION unknown"; PASS=$((PASS + 1))
    fi
    teardown
}

test_legacy_install_failure_records_last_error() {
    echo "TEST: install.sh failure on legacy upgrade writes last-error"
    setup
    write_plugin_json "0.14.3"
    create_mock_cartog "0.13.5"
    shadow_install_sh 17

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -f "$CARTOG_LOG_DIR/last-error" ]; then
        echo "  PASS: last-error file written on legacy upgrade failure"; PASS=$((PASS + 1))
    else
        echo "  FAIL: last-error file missing after legacy upgrade failure"; FAIL=$((FAIL + 1))
    fi
    teardown
}

test_skips_legacy_upgrade_when_rag_pipeline_running() {
    echo "TEST: legacy upgrade skipped when RAG pipeline lock is recent"
    setup
    write_plugin_json "0.14.3"
    create_mock_cartog "0.13.5"
    shadow_install_sh 0
    mkdir -p "$CARTOG_LOCK_DIR"   # fresh lock — pipeline "running"

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: install.sh ran while RAG pipeline lock was active"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: install.sh skipped while RAG pipeline lock active"; PASS=$((PASS + 1))
    fi
    local log
    log=$(session_log)
    assert_contains "log explains skip" "background pipeline still running" "$log"
    teardown
}

test_proceeds_when_rag_lock_is_stale() {
    echo "TEST: legacy upgrade proceeds when RAG lock is older than 1h"
    setup
    write_plugin_json "0.14.3"
    create_mock_cartog "0.13.5"
    shadow_install_sh 0
    mkdir -p "$CARTOG_LOCK_DIR"
    # Backdate to 2h ago — older than the 1h staleness threshold.
    touch -t "$(date -v-2H '+%Y%m%d%H%M.%S' 2>/dev/null || date -d '2 hours ago' '+%Y%m%d%H%M.%S' 2>/dev/null)" "$CARTOG_LOCK_DIR"

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -f "$TEST_DIR/install.log" ]; then
        echo "  PASS: install.sh ran (stale RAG lock ignored)"; PASS=$((PASS + 1))
    else
        echo "  FAIL: install.sh did not run despite stale RAG lock"; FAIL=$((FAIL + 1))
    fi
    teardown
}

# --- run ---

echo "=== update_on_exit.sh unit tests (transitional, <0.14 cohort) ==="
echo ""

test_missing_binary_is_noop
echo ""
test_modern_binary_is_noop_even_when_outdated
echo ""
test_modern_binary_in_sync_is_noop
echo ""
test_legacy_binary_outdated_routes_to_install_sh
echo ""
test_legacy_binary_in_sync_is_noop
echo ""
test_no_plugin_json_is_noop
echo ""
test_legacy_install_failure_records_last_error
echo ""
test_skips_legacy_upgrade_when_rag_pipeline_running
echo ""
test_proceeds_when_rag_lock_is_stale

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

[ "$FAIL" -eq 0 ] || exit 1
