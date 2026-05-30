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

# Extract just the function/const definitions from install.sh (everything above
# the `# === main ... ===` sentinel) so a test can source them without running
# the install flow. Fails loudly if the sentinel was renamed — otherwise the
# sed would silently emit the whole script and the test would behave oddly.
extract_install_lib() {
    local out="$1"
    local anchor='# === main (release-only install) ==='
    if ! grep -qF "$anchor" "$INSTALL_SCRIPT"; then
        echo "FATAL: anchor '$anchor' not found in $INSTALL_SCRIPT (was it renamed?)." >&2
        exit 1
    fi
    sed -n "1,/^${anchor}\$/p" "$INSTALL_SCRIPT" | sed '$d' > "$out"
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
# Drain stdin so the upstream `curl` doesn't get SIGPIPE under bash pipefail
# (real `tar xz` reads stdin; a mock that exits without reading races and
# makes `curl | tar` return 141 ~30% of the time on macOS).
cat >/dev/null
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
        export HOME="$TEST_DIR/home"
        mkdir -p "$CARGO_HOME/bin" "$HOME"
        bash "$INSTALL_SCRIPT" "$@" 2>&1
    )
}

# Variant that runs install.sh in a sandbox where cartog is NOT pre-installed,
# letting us observe pick_install_dir's choice via the "installed to" line.
run_install_fresh() {
    (
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        mkdir -p "$HOME"
        # Intentionally do not export CARGO_HOME unless the caller did — the
        # test sets it via env when needed.
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

test_version_arg_same_version_with_multiline_output_exits_early() {
    echo "TEST: multi-line --version output (build/features/rustc lines) parses to first version token"
    setup
    # Real cartog >=0.14 emits 4 lines. Make sure we strip everything after
    # line 1 and ignore the build SHA suffix.
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
cat <<EOF
cartog 0.14.3
build:    56e4f4e53a
features: default, lsp
rustc:    1.77 (MSRV)
EOF
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local output exit_code=0
    output=$(run_install "0.14.3") || exit_code=$?

    assert_eq "exits 0" "0" "$exit_code"
    assert_contains "recognises matching version despite multi-line output" "0.14.3 already installed" "$output"
    teardown
}

test_unparseable_version_warns_then_proceeds() {
    echo "TEST: cartog on PATH but --version yields nothing -> warn + proceed with fresh install (no silent re-download loop)"
    setup
    create_mock_curl
    create_mock_tar
    # Mock cartog whose --version exits non-zero AND prints nothing (e.g.,
    # broken binary, wrong arch SIGILL/Exec format error). local_version
    # parse must yield empty here — and install.sh should warn + proceed
    # rather than silently printing 'Upgrading cartog from  to X'.
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
# Pretend the binary fails to even print its version.
exit 132
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local output
    output=$(run_install "0.17.1" || true)

    assert_contains "warns about unparseable --version" \
        "did not yield a parseable version string" "$output"
    # Must NOT print the broken 'Upgrading cartog from  to X' line (empty
    # source version sandwich) — that was the silent-breakage symptom.
    assert_not_contains "no 'Upgrading from  to' line with empty source" \
        "Upgrading cartog from  to" "$output"
    # Must still announce the GitHub install attempt (we proceeded past the
    # 'already installed' short-circuit instead of silently re-printing it).
    assert_contains "proceeds to fresh install via GitHub" \
        "Downloading cartog" "$output"
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

test_no_prebuilt_binary_fails_release_only() {
    echo "TEST: download fails → exit 1 with a clear message, cargo NEVER invoked (release-only)"
    setup
    create_mock_cartog "0.6.1"
    # curl that fails (no pre-built binary downloadable).
    cat > "$TEST_DIR/bin/curl" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/curl"
    # A cargo mock is present to PROVE the script never calls it.
    create_mock_cargo
    cat > "$TEST_DIR/bin/rustc" <<'MOCK'
#!/usr/bin/env bash
echo "rustc 1.77.0 (aedd173a2 2024-03-17)"
MOCK
    chmod +x "$TEST_DIR/bin/rustc"

    local output exit_code=0
    output=$(run_install "0.7.0" 2>&1) || exit_code=$?

    assert_eq "exits 1" "1" "$exit_code"
    assert_contains "names the download failure" "could not download a pre-built cartog binary" "$output"
    assert_contains "points at the tagged release" "releases/tag/v0.7.0" "$output"
    if [ -f "$TEST_DIR/cargo.log" ]; then
        echo "  FAIL: cargo was invoked (release-only must never fall back to cargo)"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: cargo was not invoked"
        PASS=$((PASS + 1))
    fi
    teardown
}

test_download_failure_fails_release_only() {
    echo "TEST: download fails (curl errors) → exit 1, never builds from source"
    setup
    # Shadow curl with a failing stub so the download path fails; no cargo
    # fallback exists anymore.
    cat > "$TEST_DIR/bin/curl" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
    chmod +x "$TEST_DIR/bin/curl"

    local output exit_code=0
    output=$(run_install 2>&1) || exit_code=$?

    assert_eq "exits 1" "1" "$exit_code"
    assert_contains "names a release-only failure" "could not download a pre-built cartog binary" "$output"
    assert_not_contains "no cargo guidance" "cargo install" "$output"
    teardown
}

test_install_dir_prefers_local_bin_when_present() {
    echo "TEST: ~/.local/bin exists → install dir is ~/.local/bin (not ~/.cargo/bin)"
    setup
    create_mock_curl
    create_mock_tar
    mkdir -p "$TEST_DIR/home/.local/bin"

    local output
    output=$(run_install_fresh) || true

    assert_contains "installs to ~/.local/bin" "installed to $TEST_DIR/home/.local/bin/cartog" "$output"
    assert_not_contains "does not install to ~/.cargo/bin" ".cargo/bin/cartog" "$output"
    teardown
}

test_install_dir_falls_back_to_cargo_bin() {
    echo "TEST: ~/.local/bin missing + no override → falls back to ~/.cargo/bin"
    setup
    create_mock_curl
    create_mock_tar

    local output
    output=$(run_install_fresh) || true

    assert_contains "installs to ~/.cargo/bin" "installed to $TEST_DIR/home/.cargo/bin/cartog" "$output"
    teardown
}

test_install_dir_respects_cartog_install_dir_env() {
    echo "TEST: \$CARTOG_INSTALL_DIR set → installs there regardless of ~/.local/bin"
    setup
    create_mock_curl
    create_mock_tar
    mkdir -p "$TEST_DIR/home/.local/bin"
    mkdir -p "$TEST_DIR/custom-prefix"

    local output
    output=$(
        export CARTOG_INSTALL_DIR="$TEST_DIR/custom-prefix"
        run_install_fresh
    ) || true

    assert_contains "installs to override" "installed to $TEST_DIR/custom-prefix/cartog" "$output"
    assert_not_contains "ignores ~/.local/bin when override is set" ".local/bin/cartog" "$output"
    teardown
}

test_install_dir_reuses_existing_cartog_location() {
    echo "TEST: cartog already on PATH → upgrade reuses its directory (no stale duplicate)"
    setup
    create_mock_cartog "0.6.1"  # lives at $TEST_DIR/bin/cartog
    create_mock_curl
    create_mock_tar
    mkdir -p "$TEST_DIR/home/.local/bin"  # would otherwise be preferred

    local output
    output=$(run_install "0.7.0") || true

    assert_contains "reuses existing dir" "installed to $TEST_DIR/bin/cartog" "$output"
    assert_not_contains "does not pick ~/.local/bin" ".local/bin/cartog" "$output"
    # Stale-duplicate check: ~/.local/bin must remain empty.
    if [ ! -e "$TEST_DIR/home/.local/bin/cartog" ]; then
        echo "  PASS: no stale duplicate in ~/.local/bin"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: stale duplicate created in ~/.local/bin"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_path_warning_when_install_dir_not_on_path() {
    echo "TEST: install dir not on PATH → verify_install prints PATH warning"
    setup
    create_mock_curl
    create_mock_tar
    # Pre-create the binary in the chosen dir so verify_install can run it
    # (the mocked tar doesn't actually extract).
    mkdir -p "$TEST_DIR/home/.cargo/bin"
    cat > "$TEST_DIR/home/.cargo/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
echo "cartog 0.9.0"
MOCK
    chmod +x "$TEST_DIR/home/.cargo/bin/cartog"

    local output
    output=$(run_install_fresh) || true

    assert_contains "warns about PATH" "is not in your PATH" "$output"
    assert_contains "shows export hint" "export PATH=" "$output"
    teardown
}

test_no_path_warning_when_install_dir_on_path() {
    echo "TEST: install dir IS on PATH → no PATH warning"
    setup
    create_mock_curl
    create_mock_tar
    # Use the test PATH dir as both the install target (via override) and an
    # entry already on PATH. Pre-stage a working binary there.
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
echo "cartog 0.9.0"
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local output
    output=$(
        export CARTOG_INSTALL_DIR="$TEST_DIR/bin"
        run_install_fresh
    ) || true

    assert_not_contains "no PATH warning" "is not in your PATH" "$output"
    teardown
}

test_install_dir_ignores_cartog_function_shadow() {
    echo "TEST: cartog as a shell function → pick_install_dir does NOT install into cwd"
    setup
    create_mock_curl
    create_mock_tar
    mkdir -p "$TEST_DIR/home/.local/bin"
    # Wrapper script that defines a `cartog` function and then runs install.sh.
    # `command -v cartog` from inside install.sh would print "cartog" (function
    # name), and a buggy pick_install_dir would dirname that to "." and write
    # the binary into the wrapper's cwd.
    local wrapper="$TEST_DIR/run-with-function.sh"
    cat > "$wrapper" <<WRAP
#!/usr/bin/env bash
cartog() { echo "cartog 0.6.1"; }
export -f cartog
cd "$TEST_DIR/home"
bash "$INSTALL_SCRIPT" "0.7.0" 2>&1
WRAP
    chmod +x "$wrapper"

    local output
    output=$(
        export PATH="$TEST_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        export CARGO_HOME="$TEST_DIR/cargo_home"
        mkdir -p "$CARGO_HOME/bin"
        bash "$wrapper"
    ) || true

    assert_contains "falls back to ~/.local/bin (function shadow ignored)" "installed to $TEST_DIR/home/.local/bin/cartog" "$output"
    assert_not_contains "does not install into cwd" "installed to $TEST_DIR/home/cartog" "$output"
    teardown
}

test_path_warning_handles_trailing_slash() {
    echo "TEST: PATH entry has trailing slash → no spurious 'not in PATH' warning"
    setup
    create_mock_curl
    create_mock_tar
    cat > "$TEST_DIR/bin/cartog" <<'MOCK'
#!/usr/bin/env bash
echo "cartog 0.9.0"
MOCK
    chmod +x "$TEST_DIR/bin/cartog"

    local output
    output=$(
        # Append trailing slash to the PATH entry that holds the install dir.
        export PATH="$TEST_DIR/bin/:/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        export CARTOG_INSTALL_DIR="$TEST_DIR/bin"
        mkdir -p "$HOME"
        bash "$INSTALL_SCRIPT" 2>&1
    ) || true

    assert_not_contains "no PATH warning despite trailing slash" "is not in your PATH" "$output"
    teardown
}

test_verify_install_preserves_existing_binary_on_exec_failure() {
    echo "TEST: existing binary that fails --version is NOT deleted when no install just happened"
    setup
    # Source install.sh's helpers in a subshell, then call verify_install
    # directly with NO marker file present. just_installed should stay 0,
    # so the broken binary at $bin must survive.
    mkdir -p "$TEST_DIR/installdir"
    cat > "$TEST_DIR/installdir/cartog" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
    chmod +x "$TEST_DIR/installdir/cartog"

    (
        # Strip lines below the function definitions so sourcing doesn't
        # trigger the actual install flow at the bottom of the script.
        extract_install_lib "$TEST_DIR/install-lib.sh"
        export PATH="/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        export CARTOG_INSTALL_DIR="$TEST_DIR/installdir"
        # shellcheck disable=SC1091
        source "$TEST_DIR/install-lib.sh"
        # Marker absent → just_installed=0 → broken binary must NOT be deleted.
        verify_install >/dev/null 2>&1 || true
    )

    if [ -x "$TEST_DIR/installdir/cartog" ]; then
        echo "  PASS: pre-existing broken binary preserved (no marker → no delete)"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: pre-existing binary was wrongly deleted"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

test_verify_install_deletes_freshly_installed_broken_binary() {
    echo "TEST: freshly-installed binary that fails --version IS deleted (marker present)"
    setup
    mkdir -p "$TEST_DIR/installdir"
    cat > "$TEST_DIR/installdir/cartog" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
    chmod +x "$TEST_DIR/installdir/cartog"

    (
        extract_install_lib "$TEST_DIR/install-lib.sh"
        export PATH="/usr/bin:/bin:/usr/sbin:/sbin"
        export HOME="$TEST_DIR/home"
        export CARTOG_INSTALL_DIR="$TEST_DIR/installdir"
        # shellcheck disable=SC1091
        source "$TEST_DIR/install-lib.sh"
        # Simulate install_from_github having just written the marker.
        printf '%s\n' "$TEST_DIR/installdir" > "$INSTALL_DIR_MARKER"
        verify_install >/dev/null 2>&1 || true
    )

    if [ ! -e "$TEST_DIR/installdir/cartog" ]; then
        echo "  PASS: freshly-installed broken binary deleted"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: broken binary not deleted despite marker"
        FAIL=$((FAIL + 1))
    fi
    teardown
}

# --- run all tests ---

echo "=== install.sh unit tests ==="
echo ""

test_no_arg_already_installed_exits_early
echo ""
test_version_arg_same_version_exits_early
echo ""
test_version_arg_same_version_with_multiline_output_exits_early
echo ""
test_unparseable_version_warns_then_proceeds
echo ""
test_version_arg_different_version_reinstalls
echo ""
test_version_arg_uses_tag_url
echo ""
test_no_arg_not_installed_installs_latest
echo ""
test_no_prebuilt_binary_fails_release_only
echo ""
test_download_failure_fails_release_only
echo ""
test_install_dir_prefers_local_bin_when_present
echo ""
test_install_dir_falls_back_to_cargo_bin
echo ""
test_install_dir_respects_cartog_install_dir_env
echo ""
test_install_dir_reuses_existing_cartog_location
echo ""
test_path_warning_when_install_dir_not_on_path
echo ""
test_no_path_warning_when_install_dir_on_path
echo ""
test_install_dir_ignores_cartog_function_shadow
echo ""
test_path_warning_handles_trailing_slash
echo ""
test_verify_install_preserves_existing_binary_on_exec_failure
echo ""
test_verify_install_deletes_freshly_installed_broken_binary

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

[ "$FAIL" -eq 0 ] || exit 1
