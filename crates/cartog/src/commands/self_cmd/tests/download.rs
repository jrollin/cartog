//! Tests for download, checksum verification, swap, and smoke test.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::commands::self_cmd::*;

#[test]
fn archive_name_drops_version_to_match_release_workflow() {
    assert_eq!(
        archive_name_for("aarch64-apple-darwin"),
        "cartog-aarch64-apple-darwin.tar.gz"
    );
    assert_eq!(
        archive_name_for("x86_64-pc-windows-msvc"),
        "cartog-x86_64-pc-windows-msvc.zip"
    );
}

#[test]
fn parse_sha256sums_finds_named_entry() {
    let text = "\
abcd1234  cartog-aarch64-apple-darwin.tar.gz
deadbeef *cartog-x86_64-unknown-linux-gnu.tar.gz
# comment line
0123 cartog-x86_64-pc-windows-msvc.zip
";
    assert_eq!(
        parse_sha256sums(text, "cartog-aarch64-apple-darwin.tar.gz"),
        Some("abcd1234".to_string())
    );
    assert_eq!(
        parse_sha256sums(text, "cartog-x86_64-unknown-linux-gnu.tar.gz"),
        Some("deadbeef".to_string()),
        "binary-mode `*` prefix should be stripped"
    );
    assert_eq!(
        parse_sha256sums(text, "cartog-missing.tar.gz"),
        None,
        "absent entries should return None"
    );
}

#[test]
fn compute_sha256_matches_known_vector() {
    // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    assert_eq!(
        compute_sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    assert_eq!(
        compute_sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// is_leap / utc_breakdown / rfc3339_now tests live in `time_fmt::tests`.

#[test]
fn backup_path_appends_dot_old() {
    let bin = Path::new("/usr/local/bin/cartog");
    assert_eq!(
        backup_path_for(bin),
        PathBuf::from("/usr/local/bin/cartog.old")
    );
    // Windows-style suffix is preserved.
    let win = Path::new(r"C:\Program Files\cartog\cartog.exe");
    assert_eq!(
        backup_path_for(win),
        PathBuf::from(r"C:\Program Files\cartog\cartog.exe.old")
    );
}

#[test]
fn bin_name_matches_target_os() {
    if cfg!(windows) {
        assert_eq!(bin_name_in_archive(), "cartog.exe");
    } else {
        assert_eq!(bin_name_in_archive(), "cartog");
    }
}

/// Sync + close before chmod to avoid Linux ETXTBSY on fast spawn.
#[cfg(unix)]
fn write_exec_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_data().unwrap();
    }
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// Spin until exec(2) on the script no longer hits ETXTBSY (Linux
/// flags the inode briefly post-write).
#[cfg(unix)]
fn wait_for_exec_ready(bin: &Path) {
    for attempt in 0..10 {
        match std::process::Command::new(bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(20 * (attempt + 1)));
            }
            Err(e) => panic!("unexpected spawn error from {bin:?}: {e}"),
        }
    }
}

#[cfg(unix)]
#[test]
fn smoke_test_passes_on_zero_exit() {
    let dir = tempfile::TempDir::new().unwrap();
    let bin = write_exec_script(dir.path(), "ok", "#!/bin/sh\nexit 0\n");
    wait_for_exec_ready(&bin);
    smoke_test(&bin).expect("zero-exit binary must pass");
}

#[cfg(unix)]
#[test]
fn smoke_test_fails_on_non_zero_exit() {
    let dir = tempfile::TempDir::new().unwrap();
    let bin = write_exec_script(dir.path(), "fail", "#!/bin/sh\nexit 7\n");
    wait_for_exec_ready(&bin);
    let err = smoke_test(&bin).expect_err("non-zero exit must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("exited with"), "got: {msg}");
}

#[cfg(unix)]
#[test]
fn smoke_test_kills_a_hung_binary_after_timeout() {
    // Passes the ceiling as an argument rather than out-sleeping the production
    // constant.
    //
    // The old version slept 30s to out-wait a 5s ceiling and asserted the
    // watchdog fired within 15s — its runtime AND its assertion were pinned to
    // a production value it did not control. Raising SMOKE_TEST_TIMEOUT to 30s
    // (a healthy but slow binary was being rejected) made the sleep and the
    // ceiling equal, turning this into a coin flip.
    //
    // An env-var override was the next attempt and was worse: process-global,
    // so a 300ms ceiling leaked into `smoke_test_passes_on_zero_exit` and made
    // *that* healthy binary time out. A parameter cannot leak, and needs no
    // `#[serial]`.
    let timeout = Duration::from_millis(300);

    let dir = tempfile::TempDir::new().unwrap();
    // Sleeps far longer than the 300ms ceiling, so the deadline branch is the
    // only way this can return.
    let bin = write_exec_script(dir.path(), "hang", "#!/bin/sh\nsleep 30\n");
    wait_for_exec_ready(&bin);

    let start = std::time::Instant::now();
    let err = smoke_test_within(&bin, timeout).expect_err("hanging binary must time out");
    let elapsed = start.elapsed();

    let msg = format!("{err:#}");
    assert!(
        msg.contains("did not exit"),
        "expected timeout message, got: {msg}"
    );
    // Generous multiple of the 300ms ceiling — enough that machine load cannot
    // flip it, while still far below the child's 30s sleep, which is what
    // proves the watchdog killed the child rather than the child exiting.
    assert!(
        elapsed < Duration::from_secs(10),
        "smoke_test should have killed the child shortly after its {timeout:?} \
         ceiling, took {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn sweep_removes_old_staging_dirs_and_keeps_fresh() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::TempDir::new().unwrap();
    let stale = dir.path().join(".cartog-update-stale");
    let fresh = dir.path().join(".cartog-update-fresh");
    let unrelated = dir.path().join("not-a-staging-dir");
    std::fs::create_dir(&stale).unwrap();
    std::fs::create_dir(&fresh).unwrap();
    std::fs::create_dir(&unrelated).unwrap();

    // Backdate the stale dir's mtime via utimes(2) — filetime is not in
    // deps and `tempfile` doesn't expose mtime mutation.
    let two_hours_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 2 * 3600;
    let path_c = std::ffi::CString::new(stale.as_os_str().as_encoded_bytes()).unwrap();
    let times = [
        libc::timeval {
            tv_sec: two_hours_ago,
            tv_usec: 0,
        },
        libc::timeval {
            tv_sec: two_hours_ago,
            tv_usec: 0,
        },
    ];
    // SAFETY: utimes is POSIX; both args are valid pointers and `times`
    // points to a 2-element array as required.
    let rc = unsafe { libc::utimes(path_c.as_ptr(), times.as_ptr()) };
    assert_eq!(rc, 0, "utimes failed");
    let m = std::fs::metadata(&stale).unwrap();
    assert!(m.mtime() < two_hours_ago + 60);

    sweep_stale_staging_dirs(dir.path());

    assert!(!stale.exists(), "stale staging dir must be swept");
    assert!(fresh.exists(), "fresh staging dir must survive");
    assert!(unrelated.exists(), "non-cartog dirs must not be touched");
}
