#!/usr/bin/env bash
set -euo pipefail

# Unit tests for update_on_exit.sh (SessionEnd hook).
#
# Contract:
#   >= 0.14.0 — runs `cartog self update --apply-pending` to apply any
#               deferred update armed in-session. Exit 6 (peer still running)
#               is an expected no-error retry; 2/4/5 write last-error.
#   <  0.14.0 — transitional: upgraded via install.sh pinned to plugin version.
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

# Mock cartog: supports --version, logs other invocations, and — crucially —
# models clap's flag handling so the deferred-update capability gate is exercised
# honestly. `self update --help` advertises --apply-pending only for versions
# >= 0.20.0 (where the flags actually landed); for older versions, passing
# --defer/--apply-pending exits 2 with an "unexpected argument" message, exactly
# as the real binary does. Without this, a mock that swallows every flag hides
# the very bug these tests guard against.
# $2 (optional): exit code the mock returns for a SUPPORTED `self update`
#   invocation (--apply-pending on >=0.20.0, or plain `self update` on the
#   0.14..0.20 band), so tests can drive the hook's exit-code mapping. Default 0.
# $3 (optional): exit code for `self update --help`. Default 0. Set non-zero to
#   model a binary whose help probe FAILS even though its text mentions the flags
#   — the capability gate must then route to the installer, not the deferred path.
create_mock_cartog() {
    local mock_version="${1:-0.20.1}"
    local update_exit="${2:-0}"
    local help_exit="${3:-0}"
    cat > "$TEST_DIR/bin/cartog" <<MOCK
#!/usr/bin/env bash
mock_version="$mock_version"
help_exit="$help_exit"
# semver: 0 iff installed >= 0.20.0 (deferred flags present)
has_deferred() {
    local IFS=.
    local -a v
    read -ra v <<< "\${mock_version%%-*}"
    local maj="\${v[0]:-0}" min="\${v[1]:-0}"
    [ "\$maj" -gt 0 ] 2>/dev/null && return 0
    [ "\$min" -ge 20 ] 2>/dev/null && return 0
    return 1
}
if [ "\$1" = "--version" ]; then
    echo "cartog \$mock_version"
    exit 0
fi
if [ "\$1 \$2 \$3" = "self update --help" ]; then
    echo "Upgrade cartog in place (or check for an update with --check)"
    echo "  --check    Report whether an update is available"
    echo "  --quiet    Suppress all output"
    if has_deferred; then
        echo "  --defer          Arm a deferred update"
        echo "  --apply-pending  Apply a previously-armed deferred update"
    fi
    exit \$help_exit
fi
echo "\$@" >> "$CARTOG_TEST_LOG"
# Reject deferred flags on binaries that predate them (clap exit 2).
if ! has_deferred; then
    case "\$*" in
        *--defer*|*--apply-pending*)
            echo "error: unexpected argument '\$3' found" >&2
            exit 2
            ;;
    esac
fi
case "\$1 \$2 \$3" in
    "self update --apply-pending")
        # Mirror the real binary's exit-6 stderr so the hook's log assertions see
        # a realistic composite (binary message + hook echo), not just the hook's
        # own string. The binary's exact wording is guarded in Rust by the
        # peer_running_message tests.
        if [ "$update_exit" = "6" ]; then
            echo "cartog: cartog is still running in 2 other sessions (blocking lock: serve-deadbeef, PID 4242); deferred update kept and retries at the next session boundary — close the other sessions if you want it to land sooner" >&2
        fi
        exit $update_exit
        ;;
    "self update --quiet")         exit $update_exit ;;
esac
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

test_modern_binary_runs_apply_pending() {
    echo "TEST: deferred-capable binary (>=0.20.0) runs self update --apply-pending"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 0
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    if grep -qx 'self update --apply-pending' "$CARTOG_TEST_LOG"; then
        echo "  PASS: 'self update --apply-pending' invoked on modern binary"; PASS=$((PASS + 1))
    else
        echo "  FAIL: 'self update --apply-pending' not invoked on modern binary"; FAIL=$((FAIL + 1))
    fi
    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: install.sh ran (apply-pending path must not call install.sh)"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: install.sh not invoked on modern binary"; PASS=$((PASS + 1))
    fi
    teardown
}

test_modern_binary_drift_auto_arms_pin() {
    echo "TEST: drifted deferred-capable binary auto-arms the pin (--defer --to PIN) so a passive user converges"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 0   # installed 0.20.0 < pin 0.20.3 → drifted

    run_update_on_exit > /dev/null

    if grep -q 'self update --defer --to 0.20.3' "$CARTOG_TEST_LOG"; then
        echo "  PASS: auto-armed the pin on drift"; PASS=$((PASS + 1))
    else
        echo "  FAIL: did not auto-arm the pin on drift"; FAIL=$((FAIL + 1))
    fi
    # And still runs apply-pending afterward.
    if grep -qx 'self update --apply-pending' "$CARTOG_TEST_LOG"; then
        echo "  PASS: apply-pending still invoked after auto-arm"; PASS=$((PASS + 1))
    else
        echo "  FAIL: apply-pending not invoked after auto-arm"; FAIL=$((FAIL + 1))
    fi
    teardown
}

test_modern_binary_in_sync_does_not_auto_arm() {
    echo "TEST: deferred-capable binary == pin does NOT auto-arm (no spurious --defer)"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.3" 0   # installed == pin → not drifted

    run_update_on_exit > /dev/null

    if grep -q 'self update --defer' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: auto-armed when already at the pin"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: no auto-arm when at the pin"; PASS=$((PASS + 1))
    fi
    teardown
}

# --- regression: the 0.14.0..0.20.0 band (has `self update`, lacks the
#     deferred flags). Routing these into the apply-pending path made every
#     --defer/--apply-pending call fail with clap exit 2, mislabelled
#     "transient, will retry" — an unrecoverable loop. They must instead upgrade
#     via the bundled install.sh pinned to PLUGIN_VERSION (NOT a plain
#     `cartog self update`, which fetches the LATEST release and overshoots the
#     pin, and carries peer-lock/smoke exit codes that don't fit a session-end
#     swap). ---

test_predeferred_binary_upgrades_via_installer() {
    echo "TEST: 0.14..0.20 binary (no --apply-pending) upgrades via install.sh, never firing self update/--defer/--apply-pending"
    setup
    write_plugin_json "0.20.0"
    create_mock_cartog "0.19.0" 0   # has `self update`, lacks deferred flags
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -f "$TEST_DIR/install.log" ]; then
        echo "  PASS: install.sh invoked"; PASS=$((PASS + 1))
    else
        echo "  FAIL: install.sh not invoked"; FAIL=$((FAIL + 1))
    fi
    # Pin-exact: install.sh must receive PLUGIN_VERSION, not be left to fetch
    # the latest release (the overshoot bug).
    assert_contains "install.sh receives the pinned version" \
        "args=[0.20.0]" "$(cat "$TEST_DIR/install.log" 2>/dev/null || echo "")"
    # The probe runs `self update --help`, but no MUTATING `self update` must fire.
    if grep -qE 'self update (--quiet|--apply-pending|--defer)' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: fired a mutating self update the binary cannot use safely"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: no mutating self update fired (probe-only)"; PASS=$((PASS + 1))
    fi
    teardown
}

test_predeferred_binary_no_false_transient_error() {
    echo "TEST: 0.14..0.20 binary never writes a 'transient, will retry' loop for an unsupported flag"
    setup
    write_plugin_json "0.20.0"
    create_mock_cartog "0.19.0" 0
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    # The pre-fix bug: --apply-pending exited 2, mapped to a "transient; will
    # retry next session" last-error that never resolves. A clean installer
    # upgrade must leave no last-error at all.
    if [ -f "$CARTOG_LOG_DIR/last-error" ]; then
        local err
        err=$(cat "$CARTOG_LOG_DIR/last-error" 2>/dev/null || echo "")
        echo "  FAIL: wrote a last-error on a successful installer upgrade: $err"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: no spurious last-error on successful installer upgrade"; PASS=$((PASS + 1))
    fi
    teardown
}

test_predeferred_binary_in_sync_is_noop() {
    echo "TEST: 0.14..0.20 binary already at the pin does not upgrade"
    setup
    write_plugin_json "0.19.0"
    create_mock_cartog "0.19.0" 0
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: install.sh ran when already at the pin"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: no installer upgrade when at the pin"; PASS=$((PASS + 1))
    fi
    if grep -qE 'self update (--quiet|--apply-pending|--defer)' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: fired a mutating self update when at the pin"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: no mutating self update at the pin"; PASS=$((PASS + 1))
    fi
    teardown
}

test_predeferred_binary_skipped_when_rag_lock_active() {
    echo "TEST: 0.14..0.20 installer upgrade skipped while RAG pipeline lock is recent"
    setup
    write_plugin_json "0.20.0"
    create_mock_cartog "0.19.0" 0
    shadow_install_sh 0
    mkdir -p "$CARTOG_LOCK_DIR"   # fresh lock — pipeline "running"

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: installer upgrade ran while RAG pipeline lock was active"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: installer upgrade skipped while RAG pipeline lock active"; PASS=$((PASS + 1))
    fi
    assert_contains "log explains skip" "background pipeline still running" "$(session_log)"
    teardown
}

test_failed_help_probe_routes_to_installer() {
    echo "TEST: a non-zero 'self update --help' routes to installer even if its text mentions --apply-pending"
    setup
    write_plugin_json "0.21.0"
    # Help text mentions --apply-pending but the probe command exits non-zero
    # (broken binary). The gate must not trust the text. Two guards make this
    # hold: pipefail propagates the failure, and supports_deferred_update's
    # explicit `|| return 1` survives even if pipefail were ever removed.
    create_mock_cartog "0.20.0" 0 1   # help_exit=1
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -f "$TEST_DIR/install.log" ]; then
        echo "  PASS: failed probe routed to install.sh"; PASS=$((PASS + 1))
    else
        echo "  FAIL: failed probe did not route to install.sh"; FAIL=$((FAIL + 1))
    fi
    if grep -q -- '--apply-pending\|--defer' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: trusted help text and fired the deferred path on a failed probe"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: did not fire the deferred path on a failed probe"; PASS=$((PASS + 1))
    fi
    teardown
}

test_apply_pending_exit_6_is_not_error() {
    echo "TEST: apply-pending exit 6 (peer running) is not an error"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 6

    run_update_on_exit > /dev/null

    if [ -f "$CARTOG_LOG_DIR/last-error" ]; then
        echo "  FAIL: exit 6 wrote last-error (should be a silent retry)"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: exit 6 did not write last-error"; PASS=$((PASS + 1))
    fi
    assert_contains "log explains the retry" "retries at the next session boundary" "$(session_log)"
    # The binary's stderr must survive into the log: it carries the only
    # identification of what blocked the swap (#154). The hook writes no
    # last-error for exit 6, so losing this leaves nothing to grep.
    assert_contains "log keeps the blocking slot" "serve-deadbeef" "$(session_log)"
    assert_contains "log keeps the blocking pid" "4242" "$(session_log)"
    # The hook's own line must stay neutral rather than asserting a peer count
    # that contradicts the binary's message.
    if printf '%s' "$(session_log)" | grep -q 'Other cartog processes still running'; then
        echo "  FAIL: hook line re-asserts a peer count that may contradict the binary"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: hook line stays neutral about how many peers"; PASS=$((PASS + 1))
    fi
    teardown
}

test_apply_pending_exit_4_writes_last_error() {
    echo "TEST: apply-pending exit 4 (checksum) writes last-error"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 4

    run_update_on_exit > /dev/null

    if [ -f "$CARTOG_LOG_DIR/last-error" ]; then
        echo "  PASS: terminal failure wrote last-error"; PASS=$((PASS + 1))
    else
        echo "  FAIL: last-error missing after terminal apply-pending failure"; FAIL=$((FAIL + 1))
    fi
    teardown
}

test_apply_pending_exit_7_smoke_writes_actionable_last_error() {
    echo "TEST: apply-pending exit 7 (smoke) writes an actionable, non-transient last-error"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 7

    run_update_on_exit > /dev/null

    if [ -f "$CARTOG_LOG_DIR/last-error" ]; then
        echo "  PASS: smoke failure wrote last-error"; PASS=$((PASS + 1))
    else
        echo "  FAIL: last-error missing after smoke failure"; FAIL=$((FAIL + 1))
    fi
    local err
    err=$(cat "$CARTOG_LOG_DIR/last-error" 2>/dev/null || echo "")
    assert_contains "names a manual next step" "cartog self update" "$err"
    if echo "$err" | grep -qi "transient"; then
        echo "  FAIL: smoke failure must NOT be labelled transient"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: smoke failure not labelled transient"; PASS=$((PASS + 1))
    fi
    teardown
}

test_apply_pending_exit_3_cargo_writes_cargo_command() {
    echo "TEST: apply-pending exit 3 (cargo) writes the cargo-install command, not a generic failure"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 3

    run_update_on_exit > /dev/null

    local err
    err=$(cat "$CARTOG_LOG_DIR/last-error" 2>/dev/null || echo "")
    assert_contains "names the cargo command" "cargo install cartog --force" "$err"
    teardown
}

test_apply_pending_exit_2_writes_transient_last_error() {
    echo "TEST: apply-pending exit 2 (network) writes a transient last-error"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 2

    run_update_on_exit > /dev/null

    local err
    err=$(cat "$CARTOG_LOG_DIR/last-error" 2>/dev/null || echo "")
    assert_contains "labels the failure transient" "transient" "$err"
    teardown
}

test_apply_pending_exit_0_no_last_error() {
    echo "TEST: apply-pending exit 0 writes no last-error and logs success"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 0

    run_update_on_exit > /dev/null

    if [ -f "$CARTOG_LOG_DIR/last-error" ]; then
        echo "  FAIL: exit 0 wrote a last-error"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: exit 0 wrote no last-error"; PASS=$((PASS + 1))
    fi
    assert_contains "logs applied/nothing-pending" "Deferred update applied" "$(session_log)"
    teardown
}

test_apply_marker_cleared_on_clean_exit() {
    echo "TEST: apply-in-progress marker is removed after a clean apply-pending"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 0

    run_update_on_exit > /dev/null

    if [ -f "$CARTOG_LOG_DIR/apply-in-progress" ]; then
        echo "  FAIL: marker left behind after clean apply"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: marker cleared after clean apply"; PASS=$((PASS + 1))
    fi
    teardown
}

test_apply_marker_cleared_on_handled_failure() {
    echo "TEST: apply-in-progress marker is removed even when apply-pending fails (exit 4)"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 4

    run_update_on_exit > /dev/null

    if [ -f "$CARTOG_LOG_DIR/apply-in-progress" ]; then
        echo "  FAIL: marker left behind after a handled failure (would masquerade as a kill)"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: marker cleared after handled failure"; PASS=$((PASS + 1))
    fi
    teardown
}

# SIGKILL can't be trapped, so the rm never runs and the marker survives. Mock
# blocks on the swap; kill the hook mid-swap and assert the marker is left.
test_apply_marker_survives_kill_mid_swap() {
    echo "TEST: apply-in-progress marker survives a SIGKILL during the swap"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 0
    # Make the apply-pending call hang so we can kill the hook while it runs.
    # `exec sleep` so the sleep REPLACES the mock process (no orphaned grandchild
    # to leak) and records its own PID for an exact reap. Short sleep bounds the
    # worst-case orphan window if the reap below ever misses.
    cat > "$TEST_DIR/bin/cartog" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "--version" ]; then echo "cartog 0.20.0"; exit 0; fi
if [ "\$1 \$2 \$3" = "self update --help" ]; then
    echo "  --apply-pending  Apply a previously-armed deferred update"
    exit 0
fi
if [ "\$1 \$2 \$3" = "self update --apply-pending" ]; then
    echo \$\$ > "$TEST_DIR/swap.pid"
    exec sleep 5   # stand-in for a slow swap; killed by the test mid-flight
fi
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    (
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        exec bash "$UPDATE_SCRIPT" > /dev/null 2>&1
    ) &
    local hook_pid=$!
    disown "$hook_pid" 2>/dev/null || true   # stop the shell reporting "Killed: 9"

    # Wait (bounded) for the marker, i.e. the swap has started.
    local waited=0
    while [ ! -f "$CARTOG_LOG_DIR/apply-in-progress" ] && [ "$waited" -lt 50 ]; do
        sleep 0.1
        waited=$((waited + 1))
    done

    kill -KILL "$hook_pid" 2>/dev/null || true
    pkill -KILL -P "$hook_pid" 2>/dev/null || true
    # Reap the swap process by its recorded PID (exec made it the mock's PID).
    [ -f "$TEST_DIR/swap.pid" ] && kill -KILL "$(cat "$TEST_DIR/swap.pid")" 2>/dev/null || true
    # Reap the hook (bounded): the job is disowned, so poll instead of `wait`.
    local reap=0
    while kill -0 "$hook_pid" 2>/dev/null && [ "$reap" -lt 20 ]; do
        sleep 0.05
        reap=$((reap + 1))
    done

    if [ -f "$CARTOG_LOG_DIR/apply-in-progress" ]; then
        echo "  PASS: marker survived the mid-swap kill"; PASS=$((PASS + 1))
    else
        echo "  FAIL: marker missing after a mid-swap kill (interrupted swap would be silent)"; FAIL=$((FAIL + 1))
    fi
    teardown
}

test_apply_pending_skipped_when_rag_lock_active() {
    echo "TEST: apply-pending skipped when RAG pipeline lock is recent"
    setup
    write_plugin_json "0.20.3"
    create_mock_cartog "0.20.0" 0
    mkdir -p "$CARTOG_LOCK_DIR"   # fresh lock — pipeline "running"

    run_update_on_exit > /dev/null

    if grep -qx 'self update --apply-pending' "$CARTOG_TEST_LOG"; then
        echo "  FAIL: apply-pending ran while RAG pipeline lock was active"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: apply-pending skipped while RAG pipeline lock active"; PASS=$((PASS + 1))
    fi
    assert_contains "log explains skip" "background pipeline still running" "$(session_log)"
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
    assert_contains "log announces installer upgrade" \
        "Upgrading cartog 0.13.5 → 0.14.3 via install.sh" "$log"
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

test_legacy_binary_newer_than_pin_is_not_downgraded() {
    echo "TEST: legacy binary NEWER than plugin pin is not downgraded (semver gate, not string !=)"
    setup
    # Installed 0.13.9 (legacy: <0.14) but ahead of the plugin's 0.13.5.
    write_plugin_json "0.13.5"
    create_mock_cartog "0.13.9"
    shadow_install_sh 0

    run_update_on_exit > /dev/null
    restore_install_sh

    if [ -s "$TEST_DIR/install.log" ]; then
        echo "  FAIL: install.sh ran — a newer legacy binary must not be downgraded"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: install.sh not invoked when installed > pin"; PASS=$((PASS + 1))
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

echo "=== update_on_exit.sh unit tests ==="
echo ""

test_missing_binary_is_noop
echo ""
test_modern_binary_runs_apply_pending
echo ""
test_modern_binary_drift_auto_arms_pin
echo ""
test_modern_binary_in_sync_does_not_auto_arm
echo ""
test_predeferred_binary_upgrades_via_installer
echo ""
test_predeferred_binary_no_false_transient_error
echo ""
test_predeferred_binary_in_sync_is_noop
echo ""
test_predeferred_binary_skipped_when_rag_lock_active
echo ""
test_failed_help_probe_routes_to_installer
echo ""
test_apply_pending_exit_6_is_not_error
echo ""
test_apply_pending_exit_4_writes_last_error
echo ""
test_apply_pending_exit_7_smoke_writes_actionable_last_error
echo ""
test_apply_pending_exit_3_cargo_writes_cargo_command
echo ""
test_apply_pending_exit_2_writes_transient_last_error
echo ""
test_apply_pending_exit_0_no_last_error
echo ""
test_apply_pending_skipped_when_rag_lock_active
echo ""
test_apply_marker_cleared_on_clean_exit
echo ""
test_apply_marker_cleared_on_handled_failure
echo ""
test_apply_marker_survives_kill_mid_swap
echo ""
test_legacy_binary_outdated_routes_to_install_sh
echo ""
test_legacy_binary_in_sync_is_noop
echo ""
test_legacy_binary_newer_than_pin_is_not_downgraded
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
