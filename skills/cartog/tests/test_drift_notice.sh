#!/usr/bin/env bash
set -euo pipefail

# Unit tests for drift_notice.sh (user-visible SessionStart drift notice).
#
# Contract: prints ONE JSON object with `systemMessage` + `additionalContext`
# when installed < plugin pin, the project is cartog-configured, nothing is
# armed for the pin, and the pair was not announced in the last 24h.
# Otherwise prints nothing and exits 0.
#
# Usage: bash skills/cartog/tests/test_drift_notice.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
NOTICE_SCRIPT="$SKILL_DIR/scripts/drift_notice.sh"

PASS=0
FAIL=0
TEST_DIR=""

setup() {
    TEST_DIR=$(mktemp -d)
    mkdir -p "$TEST_DIR/bin" "$TEST_DIR/workdir" "$TEST_DIR/home"
    export CARTOG_LOG_DIR="$TEST_DIR/log"
    write_plugin_json "0.30.0"
    export CARTOG_PLUGIN_JSON="$TEST_DIR/plugin.json"
    # Configured project by default; gate tests remove this.
    : > "$TEST_DIR/workdir/.cartog.toml"
    unset CARTOG_NO_UPDATE_CHECK CARTOG_AUTO_INIT CARTOG_DB || true
}

teardown() {
    [ -n "$TEST_DIR" ] && rm -rf "$TEST_DIR"
    unset CARTOG_PLUGIN_JSON CARTOG_LOG_DIR CARTOG_NO_UPDATE_CHECK CARTOG_AUTO_INIT CARTOG_DB || true
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

assert_silent() {
    local label="$1" output="$2"
    assert_eq "$label" "" "$output"
}

write_plugin_json() {
    cat > "$TEST_DIR/plugin.json" <<JSON
{ "name": "cartog", "version": "$1" }
JSON
}

# Mock cartog: $1 version, $2 install_source, $3 pending target (optional),
# $4 = "legacy" to model a binary without `self version --json`.
create_mock_cartog() {
    local version="$1" source="${2:-release-tarball}" pending="${3:-}" legacy="${4:-}"
    cat > "$TEST_DIR/bin/cartog" <<MOCK
#!/usr/bin/env bash
if [ "\$1" = "--version" ]; then echo "cartog $version"; exit 0; fi
if [ "\$1" = "self" ] && [ "\$2" = "version" ]; then
    if [ -n "$legacy" ]; then echo "error: unrecognized subcommand" >&2; exit 2; fi
    if [ -n "$pending" ]; then
        printf '{\n  "version": "%s",\n  "install_source": "%s",\n  "pending_update": {\n    "target_version": "%s"\n  }\n}\n' "$version" "$source" "$pending"
    else
        printf '{\n  "version": "%s",\n  "install_source": "%s"\n}\n' "$version" "$source"
    fi
    exit 0
fi
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/cartog"
}

# Mock a broken/half-swapped binary: every invocation exits non-zero and prints
# nothing, including `--version`.
create_broken_cartog() {
    local code="${1:-2}"
    cat > "$TEST_DIR/bin/cartog" <<MOCK
#!/usr/bin/env bash
exit $code
MOCK
    chmod +x "$TEST_DIR/bin/cartog"
}

run_notice() {
    (
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        cd "$TEST_DIR/workdir"
        bash "$NOTICE_SCRIPT" 2>&1 </dev/null
    )
}

# Validate the output is exactly one JSON object with the two expected fields
# (python3 is on every macOS/Linux dev box; skip the strict check without it).
assert_valid_json() {
    local label="$1" output="$2"
    if ! command -v python3 >/dev/null 2>&1; then
        echo "  SKIP: $label (python3 not available)"; return 0
    fi
    if printf '%s' "$output" | python3 -c '
import json, sys
doc = json.loads(sys.stdin.read())
assert doc["systemMessage"]
assert doc["hookSpecificOutput"]["hookEventName"] == "SessionStart"
assert doc["hookSpecificOutput"]["additionalContext"]
'; then
        echo "  PASS: $label"; PASS=$((PASS + 1))
    else
        echo "  FAIL: $label"; echo "    output: $output"; FAIL=$((FAIL + 1))
    fi
}

# --- emits ---

test_emits_generic_notice_when_drifted() {
    echo "TEST: drifted release-tarball binary → one JSON notice naming /cartog-install"
    setup
    create_mock_cartog "0.29.0"
    local out; out=$(run_notice)
    assert_valid_json "output is one JSON object" "$out"
    assert_contains "names both versions" "cartog 0.29.0 is older than the cartog plugin (0.30.0)" "$out"
    assert_contains "says it auto-updates" "updates automatically when this session ends" "$out"
    assert_contains "offers the manual path" "/cartog-install" "$out"
    assert_eq "exactly one line" "1" "$(printf '%s\n' "$out" | wc -l | tr -d ' ')"
    teardown
}

test_emits_cargo_command_for_cargo_binary() {
    echo "TEST: drifted cargo binary → notice gives the cargo command, not /cartog-install"
    setup
    create_mock_cartog "0.29.0" "cargo"
    local out; out=$(run_notice)
    assert_valid_json "output is one JSON object" "$out"
    assert_contains "names the cargo command" "cargo install cartog --force" "$out"
    assert_contains "explains why" "cannot auto-update" "$out"
    if echo "$out" | grep -qF "/cartog-install"; then
        echo "  FAIL: dead-ends at /cartog-install"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: no /cartog-install for cargo"; PASS=$((PASS + 1))
    fi
    teardown
}

test_emits_for_legacy_binary_without_self_version() {
    echo "TEST: binary without 'self version --json' falls back to --version and still notifies"
    setup
    create_mock_cartog "0.12.0" "" "" "legacy"
    local out; out=$(run_notice)
    assert_contains "uses --version fallback" "cartog 0.12.0 is older than the cartog plugin (0.30.0)" "$out"
    teardown
}

test_emits_when_armed_target_is_stale() {
    echo "TEST: armed for an OLD pin (plugin moved on) still notifies"
    setup
    create_mock_cartog "0.28.0" "release-tarball" "0.29.0"
    local out; out=$(run_notice)
    assert_contains "notifies despite stale arm" "is older than the cartog plugin (0.30.0)" "$out"
    teardown
}

test_configured_by_existing_db_without_toml() {
    echo "TEST: no .cartog.toml but an existing .cartog/db.sqlite counts as configured"
    setup
    rm -f "$TEST_DIR/workdir/.cartog.toml"
    mkdir -p "$TEST_DIR/workdir/.cartog"; : > "$TEST_DIR/workdir/.cartog/db.sqlite"
    create_mock_cartog "0.29.0"
    local out; out=$(run_notice)
    assert_contains "DB file grants the gate" "is older than the cartog plugin" "$out"
    teardown
}

test_configured_by_toml_at_git_root_from_subdir() {
    echo "TEST: .cartog.toml at the git root is found from a subdirectory"
    setup
    rm -f "$TEST_DIR/workdir/.cartog.toml"
    mkdir -p "$TEST_DIR/workdir/.git" "$TEST_DIR/workdir/sub/deeper"
    : > "$TEST_DIR/workdir/.cartog.toml"
    create_mock_cartog "0.29.0"
    local out
    out=$( (export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"; export HOME="$TEST_DIR/home"; cd "$TEST_DIR/workdir/sub/deeper"; bash "$NOTICE_SCRIPT" 2>&1 </dev/null) )
    assert_contains "walks up to the git root" "is older than the cartog plugin" "$out"
    teardown
}

test_configured_by_auto_init_env() {
    echo "TEST: CARTOG_AUTO_INIT grants the gate without toml or DB"
    setup
    rm -f "$TEST_DIR/workdir/.cartog.toml"
    export CARTOG_AUTO_INIT=1
    create_mock_cartog "0.29.0"
    local out; out=$(run_notice)
    assert_contains "AUTO_INIT grants the gate" "is older than the cartog plugin" "$out"
    teardown
}

# --- silent ---

test_silent_when_in_sync() {
    echo "TEST: installed == pin → silent"
    setup
    create_mock_cartog "0.30.0"
    assert_silent "no output when in sync" "$(run_notice)"
    teardown
}

test_silent_when_ahead_of_pin() {
    echo "TEST: installed > pin (deliberate manual install) → silent"
    setup
    create_mock_cartog "0.31.0"
    assert_silent "no output when ahead" "$(run_notice)"
    teardown
}

test_an_armed_pin_does_not_by_itself_suppress_the_notice() {
    echo "TEST: an armed pin never suppresses on its own; only the 24h marker does"
    setup
    create_mock_cartog "0.29.0" "release-tarball" "0.30.0"
    # `ensure_indexed.sh` arms during this same startup and sibling hooks are
    # unordered, so an arm present at read time may be one the user has never
    # been told about. Suppressing on it dropped the notice in 2 of 5 races.
    assert_contains "notifies even though an update is armed for the pin" \
        "is older than the cartog plugin" "$(run_notice)"
    # The marker, not the arm, is what keeps the next startup quiet.
    assert_silent "the marker suppresses the repeat" "$(run_notice)"
    teardown
}

test_an_update_armed_but_stuck_over_a_day_is_re_announced() {
    echo "TEST: an armed update that has not applied in 24h gets re-announced"
    setup
    create_mock_cartog "0.29.0" "release-tarball" "0.30.0"
    run_notice >/dev/null
    # Still armed, still not applied, a day later: a wedged apply or a peer that
    # never closes. Gating suppression on the arm would mute this forever, which
    # is precisely when the user needs telling again.
    touch -t 202001010000 "$CARTOG_LOG_DIR/drift-notified-0.29.0-0.30.0"
    assert_contains "re-announces a stuck armed update" \
        "is older than the cartog plugin" "$(run_notice)"
    teardown
}

test_silent_when_project_not_configured() {
    echo "TEST: no toml, no DB, no env → silent (unconfigured repo gets no cartog output)"
    setup
    rm -f "$TEST_DIR/workdir/.cartog.toml"
    create_mock_cartog "0.29.0"
    assert_silent "no output for an unconfigured project" "$(run_notice)"
    teardown
}

test_silent_when_kill_switch_set() {
    echo "TEST: CARTOG_NO_UPDATE_CHECK set → silent"
    setup
    export CARTOG_NO_UPDATE_CHECK=1
    create_mock_cartog "0.29.0"
    assert_silent "kill switch honored" "$(run_notice)"
    teardown
}

test_silent_when_binary_missing() {
    echo "TEST: no cartog on PATH → silent (ensure_indexed.sh owns the install path)"
    setup
    assert_silent "no output without a binary" "$(run_notice)"
    teardown
}

# --- a broken binary must degrade to silence, never to a blocking hook ---

test_broken_binary_exits_zero_and_silent() {
    echo "TEST: a cartog that fails every probe -> hook is silent AND exits 0"
    setup
    # Exit 2 is Claude Code's blocking-error code for a hook; under `pipefail` the
    # binary's status used to propagate out of the --version assignment and kill
    # the script before its emptiness guard, producing an empty exit-2 hook.
    for code in 1 2 127; do
        create_broken_cartog "$code"
        local out rc
        out=$(run_notice) && rc=0 || rc=$?
        assert_eq "binary exit $code -> hook exit 0" "0" "$rc"
        assert_silent "binary exit $code -> no output" "$out"
    done
    teardown
}

test_silent_when_no_plugin_json() {
    echo "TEST: unreadable plugin.json → silent"
    setup
    export CARTOG_PLUGIN_JSON="$TEST_DIR/does-not-exist.json"
    create_mock_cartog "0.29.0"
    assert_silent "no output without a pin" "$(run_notice)"
    teardown
}

test_cartog_db_pointing_at_a_missing_file_is_not_consent() {
    echo "TEST: CARTOG_DB set but the file does not exist -> silent (matches ensure_indexed.sh)"
    setup
    rm -f "$TEST_DIR/workdir/.cartog.toml"
    export CARTOG_DB="$TEST_DIR/nope/db.sqlite"
    create_mock_cartog "0.29.0"
    assert_silent "a dangling CARTOG_DB grants no consent" "$(run_notice)"
    teardown
}

test_cartog_db_pointing_at_a_real_file_is_consent() {
    echo "TEST: CARTOG_DB naming an existing DB -> notifies"
    setup
    rm -f "$TEST_DIR/workdir/.cartog.toml"
    : > "$TEST_DIR/real.sqlite"
    export CARTOG_DB="$TEST_DIR/real.sqlite"
    create_mock_cartog "0.29.0"
    assert_contains "an existing CARTOG_DB grants consent" "is older than the cartog plugin" "$(run_notice)"
    teardown
}

test_unwritable_cache_dir_is_silent_and_never_writes_tmp() {
    echo "TEST: no private cache dir -> silent, and NOTHING written to shared /tmp"
    setup
    create_mock_cartog "0.29.0"
    # A regular file where the cache dir should be: `mkdir -p` fails.
    local blocked="$TEST_DIR/not-a-dir"
    : > "$blocked"
    local before after out
    before=$(find /tmp -maxdepth 1 -name 'drift-notified-*' 2>/dev/null | wc -l | tr -d ' ')
    out=$( cd "$TEST_DIR/workdir" \
        && PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin" HOME="$TEST_DIR/home" \
           CARTOG_LOG_DIR="$blocked" bash "$NOTICE_SCRIPT" 2>&1 </dev/null )
    after=$(find /tmp -maxdepth 1 -name 'drift-notified-*' 2>/dev/null | wc -l | tr -d ' ')
    # Falling back to /tmp would put a predictable, world-writable path in play:
    # another local user can pre-create it as a symlink and have `: > "$marker"`
    # truncate the target. With nowhere private to dedupe, stay silent instead.
    assert_silent "no notice without a private cache dir" "$out"
    assert_eq "nothing written to shared /tmp" "$before" "$after"
    teardown
}

# --- dedupe ---

test_second_run_within_24h_is_silent() {
    echo "TEST: same (installed, pin) pair announced <24h ago → silent"
    setup
    create_mock_cartog "0.29.0"
    local first second
    first=$(run_notice)
    second=$(run_notice)
    assert_contains "first run notifies" "is older than the cartog plugin" "$first"
    assert_silent "second run is silent" "$second"
    teardown
}

test_stale_marker_re_notifies() {
    echo "TEST: marker older than 24h → notifies again"
    setup
    create_mock_cartog "0.29.0"
    run_notice >/dev/null
    touch -t 202001010000 "$CARTOG_LOG_DIR/drift-notified-0.29.0-0.30.0"
    local out; out=$(run_notice)
    assert_contains "re-notifies after 24h" "is older than the cartog plugin" "$out"
    teardown
}

test_concurrent_windows_emit_exactly_one_notice() {
    echo "TEST: N windows started at once emit ONE notice (atomic claim, not check-then-write)"
    setup
    create_mock_cartog "0.29.0"
    local i
    for i in 1 2 3 4 5 6; do
        ( cd "$TEST_DIR/workdir" \
          && PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin" HOME="$TEST_DIR/home" \
             bash "$NOTICE_SCRIPT" > "$TEST_DIR/out$i" 2>&1 ) &
    done
    wait
    local emitted
    emitted=$(grep -l systemMessage "$TEST_DIR"/out* 2>/dev/null | wc -l | tr -d ' ')
    assert_eq "exactly one of six concurrent runs notifies" "1" "$emitted"
    # A crashed claim must not mute the notice forever, so nothing may be left behind.
    local leftover
    leftover=$(find "$CARTOG_LOG_DIR" -maxdepth 1 -name '*.claim' 2>/dev/null | wc -l | tr -d ' ')
    assert_eq "no claim directory left behind" "0" "$leftover"
    teardown
}

test_stale_claim_does_not_mute_the_notice_forever() {
    echo "TEST: a claim left by a killed run is reaped after an hour, not honored forever"
    setup
    create_mock_cartog "0.29.0"
    mkdir -p "$CARTOG_LOG_DIR"
    local claim="$CARTOG_LOG_DIR/drift-notified-0.29.0-0.30.0.claim"
    mkdir -p "$claim"
    # Fresh claim: another window is mid-notice, so stay quiet.
    assert_silent "a fresh claim suppresses this run" "$(run_notice)"
    # Aged past the reap window: the owner is gone, so recover.
    touch -t 202001010000 "$claim"
    assert_contains "a stale claim is reaped" "is older than the cartog plugin" "$(run_notice)"
    teardown
}

test_new_pin_re_notifies_and_drops_old_marker() {
    echo "TEST: plugin pin moves → fresh notice; the previous pair's marker is removed"
    setup
    create_mock_cartog "0.29.0"
    run_notice >/dev/null
    write_plugin_json "0.31.0"
    local out; out=$(run_notice)
    assert_contains "re-notifies for the new pin" "cartog plugin (0.31.0)" "$out"
    if [ -f "$CARTOG_LOG_DIR/drift-notified-0.29.0-0.30.0" ]; then
        echo "  FAIL: old marker left behind"; FAIL=$((FAIL + 1))
    else
        echo "  PASS: old marker removed"; PASS=$((PASS + 1))
    fi
    [ -f "$CARTOG_LOG_DIR/drift-notified-0.29.0-0.31.0" ] && { echo "  PASS: new marker written"; PASS=$((PASS + 1)); } || { echo "  FAIL: new marker missing"; FAIL=$((FAIL + 1)); }
    teardown
}

test_emits_generic_notice_when_drifted
echo ""
test_emits_cargo_command_for_cargo_binary
echo ""
test_emits_for_legacy_binary_without_self_version
echo ""
test_emits_when_armed_target_is_stale
echo ""
test_configured_by_existing_db_without_toml
echo ""
test_configured_by_toml_at_git_root_from_subdir
echo ""
test_configured_by_auto_init_env
echo ""
test_silent_when_in_sync
echo ""
test_silent_when_ahead_of_pin
echo ""
test_an_armed_pin_does_not_by_itself_suppress_the_notice
echo ""
test_an_update_armed_but_stuck_over_a_day_is_re_announced
echo ""
test_silent_when_project_not_configured
echo ""
test_silent_when_kill_switch_set
echo ""
test_silent_when_binary_missing
echo ""
test_silent_when_no_plugin_json
echo ""
test_broken_binary_exits_zero_and_silent
echo ""
test_cartog_db_pointing_at_a_missing_file_is_not_consent
echo ""
test_cartog_db_pointing_at_a_real_file_is_consent
echo ""
test_unwritable_cache_dir_is_silent_and_never_writes_tmp
echo ""
test_second_run_within_24h_is_silent
echo ""
test_stale_marker_re_notifies
echo ""
test_new_pin_re_notifies_and_drops_old_marker
echo ""
test_concurrent_windows_emit_exactly_one_notice
echo ""
test_stale_claim_does_not_mute_the_notice_forever

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

[ "$FAIL" -eq 0 ] || exit 1
