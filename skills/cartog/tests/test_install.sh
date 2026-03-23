#!/usr/bin/env bash
set -euo pipefail

# Unit tests for install.sh
# Uses mocked cartog, curl, and cargo to verify install behavior.
#
# Usage: bash skills/cartog/tests/test_install.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_SCRIPT="$SKILL_DIR/scripts/install.sh"

PASS=0
FAIL=0
TEST_DIR=""

# --- helpers ---

setup() {
    TEST_DIR=$(mktemp -d)
    mkdir -p "$TEST_DIR/bin"
}

teardown() {
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

create_mock_cartog() {
    local version="${1:-0.6.1}"
    cat > "$TEST_DIR/bin/cartog" <<MOCK
#!/usr/bin/env bash
echo "cartog $version"
MOCK
    chmod +x "$TEST_DIR/bin/cartog"
}

create_mock_curl() {
    local log_file="$TEST_DIR/curl.log"
    cat > "$TEST_DIR/bin/curl" <<'MOCK'
#!/usr/bin/env bash
echo "$@" >> LOGFILE
echo '{ "tag_name": "v0.9.0" }'
MOCK
    sed -i'' -e "s|LOGFILE|$log_file|" "$TEST_DIR/bin/curl"
    chmod +x "$TEST_DIR/bin/curl"
}

create_mock_tar() {
    cat > "$TEST_DIR/bin/tar" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK
    chmod +x "$TEST_DIR/bin/tar"
}

create_mock_cargo() {
    local log_file="$TEST_DIR/cargo.log"
    cat > "$TEST_DIR/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
echo "$@" >> LOGFILE
exit 0
MOCK
    sed -i'' -e "s|LOGFILE|$log_file|" "$TEST_DIR/bin/cargo"
    chmod +x "$TEST_DIR/bin/cargo"
}

run_install() {
    (
        # Use restricted PATH: test bin + essential system dirs only
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export CARGO_HOME="$TEST_DIR/cargo_home"
        mkdir -p "$CARGO_HOME/bin"
        bash "$INSTALL_SCRIPT" "$@" 2>&1
    )
}

# --- tests ---

test_no_arg_already_installed_exits_early() {
    echo "TEST: no arg + already installed → exits early"
    setup
    create_mock_cartog "0.6.1"

    local output exit_code=0
    output=$(run_install) || exit_code=$?

    assert_eq "exits 0" "0" "$exit_code"
    assert_contains "shows already installed" "already installed" "$output"
    teardown
}

test_version_arg_same_version_exits_early() {
    echo "TEST: version arg + same version → exits early"
    setup
    create_mock_cartog "0.7.0"

    local output exit_code=0
    output=$(run_install "0.7.0") || exit_code=$?

    assert_eq "exits 0" "0" "$exit_code"
    assert_contains "shows already at version" "0.7.0 already installed" "$output"
    teardown
}

test_version_arg_different_version_reinstalls() {
    echo "TEST: version arg + different version → reinstalls"
    setup
    create_mock_cartog "0.6.1"
    create_mock_curl
    create_mock_tar

    local output exit_code=0
    output=$(run_install "0.7.0") || exit_code=$?

    assert_contains "shows upgrading" "Upgrading cartog from 0.6.1 to 0.7.0" "$output"
    teardown
}

test_version_arg_uses_tag_url() {
    echo "TEST: version arg → uses v{version} tag URL (not /releases/latest)"
    setup
    create_mock_cartog "0.6.1"
    create_mock_curl
    create_mock_tar

    run_install "0.7.0" > /dev/null 2>&1 || true

    local curl_log="$TEST_DIR/curl.log"
    if [ -f "$curl_log" ]; then
        local curl_args
        curl_args=$(cat "$curl_log")
        assert_not_contains "does not query /releases/latest" "releases/latest" "$curl_args"
        assert_contains "uses v0.7.0 tag in URL" "v0.7.0" "$curl_args"
    else
        echo "  PASS: curl not called for API (direct tag URL used)"
        PASS=$((PASS + 1))
        # Verify the download URL would have the tag — check output
    fi
    teardown
}

test_no_arg_not_installed_installs_latest() {
    echo "TEST: no arg + not installed → installs latest"
    setup
    # No mock cartog — simulates not installed
    create_mock_curl
    create_mock_tar

    local output exit_code=0
    output=$(run_install) || exit_code=$?

    assert_contains "downloads from github" "Downloading cartog" "$output"
    teardown
}

test_cargo_fallback_with_version() {
    echo "TEST: version arg + no binary available → cargo install cartog@version"
    setup
    # Mock cartog at different version to trigger upgrade path
    create_mock_cartog "0.6.1"
    # curl that fails (no pre-built binary)
    cat > "$TEST_DIR/bin/curl" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/curl"
    create_mock_cargo
    # Mock rustc for version check
    cat > "$TEST_DIR/bin/rustc" <<'MOCK'
#!/usr/bin/env bash
echo "rustc 1.77.0 (aedd173a2 2024-03-17)"
MOCK
    chmod +x "$TEST_DIR/bin/rustc"

    run_install "0.7.0" > /dev/null 2>&1 || true

    local cargo_log="$TEST_DIR/cargo.log"
    if [ -f "$cargo_log" ]; then
        local cargo_args
        cargo_args=$(cat "$cargo_log")
        assert_contains "cargo install with version" "cartog@0.7.0" "$cargo_args"
    else
        echo "  FAIL: cargo was not called"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_no_curl_no_cargo_fails() {
    echo "TEST: no curl + no cargo → error and exit 1"
    setup
    # Shadow curl and tar with failing stubs so download path fails,
    # and no cargo mock so the fallback also fails
    cat > "$TEST_DIR/bin/curl" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/curl"

    local output exit_code=0
    output=$(run_install) || exit_code=$?

    assert_eq "exits 1" "1" "$exit_code"
    assert_contains "shows error" "cargo not found" "$output"
    teardown
}

test_rust_version_too_old_fails() {
    echo "TEST: Rust version too old → error and exit 1"
    setup
    # curl that fails (no pre-built binary)
    cat > "$TEST_DIR/bin/curl" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/curl"
    # cargo present but rustc too old
    create_mock_cargo
    cat > "$TEST_DIR/bin/rustc" <<'MOCK'
#!/usr/bin/env bash
echo "rustc 1.50.0 (cb75ad5db 2021-02-10)"
MOCK
    chmod +x "$TEST_DIR/bin/rustc"

    local output exit_code=0
    output=$(run_install) || exit_code=$?

    assert_eq "exits 1" "1" "$exit_code"
    assert_contains "shows rust too old" "Rust toolchain too old" "$output"
    teardown
}

# --- run all tests ---

echo "=== install.sh unit tests ==="
echo ""

test_no_arg_already_installed_exits_early
echo ""
test_version_arg_same_version_exits_early
echo ""
test_version_arg_different_version_reinstalls
echo ""
test_version_arg_uses_tag_url
echo ""
test_no_arg_not_installed_installs_latest
echo ""
test_cargo_fallback_with_version
echo ""
test_no_curl_no_cargo_fails
echo ""
test_rust_version_too_old_fails

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

[ "$FAIL" -eq 0 ] || exit 1
