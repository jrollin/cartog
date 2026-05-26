//! Integration tests for `cartog push` / `cartog pull` against a floci
//! (AWS-local-emulator) container.
//!
//! These tests boot a fresh floci container per test (random port), exercise
//! the push/pull round-trip, and tear the container down on Drop. They are
//! gated on:
//!
//! 1. the `remote-s3` feature being built in (the default), AND
//! 2. `docker` + `aws` (AWS CLI) being available on `PATH` —
//!    otherwise the tests print SKIP and return early.
//!
//! The AWS CLI is used only to create the bucket; `rust-s3`'s create-bucket
//! request shape does not satisfy floci's parser, but every other operation
//! (PUT/GET/HEAD) round-trips fine. In production cartog never creates
//! buckets; users provision them via their cloud console or IaC.

#![cfg(all(unix, feature = "remote-s3"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const FLOCI_IMAGE: &str = "floci/floci";
const FLOCI_PORT_INSIDE: &str = "4566";

/// Boots a floci container on a random host port. On Drop, the container is
/// killed. The container starts in <100 ms once the image is cached; we wait
/// up to ~5 s for the HTTP listener to come up.
struct FlociContainer {
    name: String,
    port: u16,
}

impl FlociContainer {
    fn start() -> Option<Self> {
        // Use a unique container name per test invocation to avoid collisions
        // across parallel test runs.
        let name = format!(
            "cartog-floci-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let status = Command::new("docker")
            .args([
                "run",
                "--rm",
                "-d",
                "--name",
                &name,
                "-p",
                &format!("0:{FLOCI_PORT_INSIDE}"),
                FLOCI_IMAGE,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }

        // From this point on, the container is running. Every early return
        // path must kill it explicitly — `--rm` only fires on container exit,
        // and a leaked container ties up a host port until the test process
        // dies. A scope-guard would be tidier but adds a helper struct just
        // for this 60-line function; an inline `kill` macro keeps it local.
        macro_rules! abort {
            () => {{
                let _ = Command::new("docker")
                    .args(["kill", &name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                return None;
            }};
        }

        // Discover host port via `docker port`.
        let out = match Command::new("docker")
            .args(["port", &name, FLOCI_PORT_INSIDE])
            .output()
        {
            Ok(o) => o,
            Err(_) => abort!(),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let port: u16 = match stdout
            .lines()
            .find_map(|l| l.rsplit(':').next()?.trim().parse().ok())
        {
            Some(p) => p,
            None => abort!(),
        };

        // Wait for floci's HTTP listener.
        let endpoint = format!("http://localhost:{port}");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // Use `aws s3 ls` against the endpoint as the readiness probe.
            let probe = Command::new("aws")
                .args(["--endpoint-url", &endpoint, "s3", "ls"])
                .env("AWS_ACCESS_KEY_ID", "test")
                .env("AWS_SECRET_ACCESS_KEY", "test")
                .env("AWS_DEFAULT_REGION", "us-east-1")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if matches!(probe, Ok(s) if s.success()) {
                break;
            }
            if Instant::now() > deadline {
                // Container is up but listener never replied — kill and bail.
                abort!();
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        Some(Self { name, port })
    }

    fn endpoint(&self) -> String {
        format!("http://localhost:{}", self.port)
    }
}

impl Drop for FlociContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["kill", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Returns `Some(path)` if `name` is on PATH, else `None`.
fn which(name: &str) -> Option<PathBuf> {
    Command::new("which")
        .arg(name)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
}

/// Returns true (with a SKIP message printed) when an external dep is missing.
/// Tests should `return` early if this returns true.
fn skip_unless_deps_present() -> bool {
    for tool in ["docker", "aws"] {
        if which(tool).is_none() {
            eprintln!("SKIP: `{tool}` not on PATH");
            return true;
        }
    }
    false
}

fn create_bucket(endpoint: &str, bucket: &str) {
    let st = Command::new("aws")
        .args([
            "--endpoint-url",
            endpoint,
            "s3",
            "mb",
            &format!("s3://{bucket}"),
        ])
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run aws s3 mb");
    assert!(st.success(), "aws s3 mb failed");
}

/// Build a minimal real cartog DB by running `cartog index` on a small
/// throwaway repo. We don't fabricate SQLite files from scratch — push/pull
/// must work against actual cartog-produced DBs.
fn build_minimal_index(repo_dir: &Path, db_path: &Path) {
    std::fs::create_dir_all(repo_dir).unwrap();
    std::fs::write(repo_dir.join("hello.py"), "def greet():\n    return 'hi'\n").unwrap();
    // git init so cartog detects a repo root.
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo_dir)
        .status();

    let st = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args([
            "--db",
            &db_path.to_string_lossy(),
            "index",
            "--no-lsp",
            &repo_dir.to_string_lossy(),
        ])
        .env_remove("CARTOG_DB")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn cartog index");
    assert!(st.success(), "cartog index failed");
    assert!(
        db_path.exists(),
        "DB was not created at {}",
        db_path.display()
    );
}

#[test]
fn push_pull_roundtrip_against_floci() {
    if skip_unless_deps_present() {
        return;
    }
    let floci = match FlociContainer::start() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: could not start floci container");
            return;
        }
    };
    let endpoint = floci.endpoint();
    create_bucket(&endpoint, "cartog-roundtrip");

    let work = tempfile::TempDir::new().unwrap();
    let repo = work.path().join("repo");
    let src_db = work.path().join("src.sqlite");
    let dst_db = work.path().join("dst.sqlite");
    build_minimal_index(&repo, &src_db);

    let src_bytes = std::fs::read(&src_db).unwrap();

    let env = &[
        ("AWS_ACCESS_KEY_ID", "test"),
        ("AWS_SECRET_ACCESS_KEY", "test"),
        ("AWS_DEFAULT_REGION", "us-east-1"),
    ];

    // cartog needs `[remote].endpoint` to talk to floci; the CLI has no
    // `--endpoint` flag (and shouldn't — endpoint is per-deployment config,
    // not per-invocation). Generate a throwaway `.cartog.toml` and run
    // cartog from that directory so it walks up and finds it.
    let cfg_dir = work.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join(".cartog.toml"),
        format!(
            r#"[remote]
url = "s3://cartog-roundtrip/index.sqlite"
region = "us-east-1"
endpoint = "{endpoint}"
path_style = true
"#
        ),
    )
    .unwrap();
    // Make cfg_dir a git root so cartog walks up and finds the config.
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cfg_dir)
        .status();

    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &src_db.to_string_lossy(), "push"])
        .current_dir(&cfg_dir)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Independently of the round-trip test, assert that push wrote the
    // three headers pull now mandates. If push silently stopped emitting
    // them (or used different names), the round-trip would *still pass*
    // because pull would just produce a clean failure that the test only
    // detects via the assertion below — but no other test catches a
    // future regression where the metadata names drift apart.
    let head = Command::new("aws")
        .args([
            "--endpoint-url",
            &endpoint,
            "s3api",
            "head-object",
            "--bucket",
            "cartog-roundtrip",
            "--key",
            "index.sqlite",
        ])
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(head.status.success(), "head-object failed: {:?}", head);
    let head_json = String::from_utf8_lossy(&head.stdout);
    for header in ["sha256", "schema-version", "cartog-version"] {
        // AWS CLI flattens x-amz-meta-foo into Metadata.{foo} in JSON.
        // Just substring-match — the field name itself is the contract.
        assert!(
            head_json.contains(&format!("\"{header}\":")),
            "push did not set x-amz-meta-{header}; full head-object output:\n{head_json}"
        );
    }

    // Pull into a fresh dst_db.
    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &dst_db.to_string_lossy(), "pull"])
        .current_dir(&cfg_dir)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pull failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let dst_bytes = std::fs::read(&dst_db).unwrap();
    assert_eq!(src_bytes, dst_bytes, "round-tripped DB differs from source");
}

#[test]
fn pull_refuses_on_checksum_mismatch() {
    if skip_unless_deps_present() {
        return;
    }
    let floci = match FlociContainer::start() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: could not start floci container");
            return;
        }
    };
    let endpoint = floci.endpoint();
    create_bucket(&endpoint, "cartog-corrupt");

    let work = tempfile::TempDir::new().unwrap();
    let cfg_dir = work.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join(".cartog.toml"),
        format!(
            r#"[remote]
url = "s3://cartog-corrupt/index.sqlite"
region = "us-east-1"
endpoint = "{endpoint}"
path_style = true
"#
        ),
    )
    .unwrap();
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cfg_dir)
        .status();

    // Upload an object with NO x-amz-meta-sha256 — pull must refuse.
    let payload_path = work.path().join("payload");
    std::fs::write(&payload_path, b"not really a sqlite file").unwrap();
    let st = Command::new("aws")
        .args([
            "--endpoint-url",
            &endpoint,
            "s3",
            "cp",
            &payload_path.to_string_lossy(),
            "s3://cartog-corrupt/index.sqlite",
        ])
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .status()
        .unwrap();
    assert!(st.success());

    let dst_db = work.path().join("dst.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &dst_db.to_string_lossy(), "pull"])
        .current_dir(&cfg_dir)
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .output()
        .unwrap();
    assert!(!out.status.success(), "pull must fail on missing checksum");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sha256") || stderr.contains("checksum"),
        "expected checksum error, got: {stderr}"
    );

    // No file may remain at the destination.
    assert!(
        !dst_db.exists(),
        "destination DB must be absent after failed pull"
    );
    let partial = work.path().join("dst.sqlite.partial");
    assert!(!partial.exists(), "partial file must be cleaned up");
}

#[test]
fn anonymous_pull_on_missing_object_fails_cleanly() {
    // Exercises the `--no-sign-request` code path against a bucket that
    // exists but is empty. The contract: pull must fail (non-zero exit) and
    // must not leave behind a destination DB or a `.partial`. The previous
    // version of this test asserted only the cleanup half; this one also
    // asserts the failure half so a silent-success regression is caught.
    if skip_unless_deps_present() {
        return;
    }
    let floci = match FlociContainer::start() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: could not start floci container");
            return;
        }
    };
    let endpoint = floci.endpoint();
    create_bucket(&endpoint, "cartog-anon");

    let work = tempfile::TempDir::new().unwrap();
    let cfg_dir = work.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join(".cartog.toml"),
        format!(
            r#"[remote]
url = "s3://cartog-anon/missing-object.sqlite"
region = "us-east-1"
endpoint = "{endpoint}"
path_style = true
"#
        ),
    )
    .unwrap();
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cfg_dir)
        .status();

    let dst_db = work.path().join("dst.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args([
            "--db",
            &dst_db.to_string_lossy(),
            "pull",
            "--no-sign-request",
        ])
        .current_dir(&cfg_dir)
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "anonymous pull of a non-existent object must fail:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dst_db.exists(),
        "destination DB must not exist after a failed pull"
    );
    let partial = work.path().join("dst.sqlite.partial");
    assert!(
        !partial.exists(),
        "partial file must be cleaned up after a failed pull"
    );
}

/// Pulling an arbitrary SQLite file that has no cartog `metadata` table must
/// be refused even when the sha256 metadata matches the bytes — otherwise an
/// unrelated app's database (or a corrupted upload) could overwrite the
/// local cartog DB and break subsequent commands in confusing ways.
#[test]
fn pull_refuses_non_cartog_sqlite_with_valid_sha() {
    if skip_unless_deps_present() {
        return;
    }
    let floci = match FlociContainer::start() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: could not start floci container");
            return;
        }
    };
    let endpoint = floci.endpoint();
    create_bucket(&endpoint, "cartog-foreign-sqlite");

    let work = tempfile::TempDir::new().unwrap();

    // Build a real SQLite file that is NOT a cartog DB.
    let foreign_db = work.path().join("foreign.sqlite");
    {
        let conn = rusqlite::Connection::open(&foreign_db).unwrap();
        conn.execute("CREATE TABLE notes(content TEXT)", [])
            .unwrap();
        conn.execute("INSERT INTO notes VALUES ('hello')", [])
            .unwrap();
    }
    let bytes = std::fs::read(&foreign_db).unwrap();
    // Compute the matching sha256 so we attest the bytes correctly — the
    // guard must still refuse this on schema-version grounds.
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(&bytes);
    let sha = h
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    // Upload with sha256 + schema-version headers. Both are now required
    // by pull, so we set both to reach the "schema_version row missing in
    // the file" code path. The header claims the current schema version
    // (referenced from the crate constant so this test doesn't break on a
    // schema bump); the in-file row is missing entirely (this is not a
    // cartog DB), so the "not a cartog database" check fires.
    let claimed_v = cartog::db::CURRENT_SCHEMA_VERSION;
    let st = Command::new("aws")
        .args([
            "--endpoint-url",
            &endpoint,
            "s3",
            "cp",
            &foreign_db.to_string_lossy(),
            "s3://cartog-foreign-sqlite/index.sqlite",
            "--metadata",
            &format!("sha256={sha},schema-version={claimed_v}"),
        ])
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .status()
        .unwrap();
    assert!(st.success(), "aws s3 cp failed");

    let cfg_dir = work.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join(".cartog.toml"),
        format!(
            r#"[remote]
url = "s3://cartog-foreign-sqlite/index.sqlite"
region = "us-east-1"
endpoint = "{endpoint}"
path_style = true
"#
        ),
    )
    .unwrap();
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cfg_dir)
        .status();

    let dst_db = work.path().join("dst.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &dst_db.to_string_lossy(), "pull"])
        .current_dir(&cfg_dir)
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "pull of a non-cartog SQLite file must fail even with a matching sha256"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a cartog database"),
        "expected 'not a cartog database' refusal, got: {stderr}"
    );
    assert!(!dst_db.exists(), "destination must not be created");
    let partial = work.path().join("dst.sqlite.partial");
    assert!(!partial.exists(), "partial file must be cleaned up");
}

/// `cartog push` / `cartog pull` must refuse to run when `.cartog.toml`
/// exists but was rejected (credential pre-check, parse error, unknown
/// field). Without this guard, the security error printed at config-load
/// time would scroll off and the user would see a misleading "no remote
/// configured" downstream error. No docker required — the rejection
/// happens before any S3 call.
#[test]
fn push_refuses_when_config_was_rejected() {
    let work = tempfile::TempDir::new().unwrap();
    let cfg_dir = work.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    // A credential-shaped key gets the config rejected by the security
    // pre-check. The exact rejection reason doesn't matter for this test
    // — any cause that makes load_config return `Rejected` would do.
    std::fs::write(
        cfg_dir.join(".cartog.toml"),
        "[remote]\nurl = \"s3://b/k\"\naccess_key = \"AKIA...\"\n",
    )
    .unwrap();
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cfg_dir)
        .status();

    let db_path = work.path().join("db.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &db_path.to_string_lossy(), "push"])
        .current_dir(&cfg_dir)
        .env_remove("CARTOG_DB")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "push must fail when config was rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("configuration file") && stderr.contains("was rejected"),
        "expected explicit config-rejected error, got: {stderr}"
    );
    // The security error from config load should also be visible.
    assert!(
        stderr.contains("credential"),
        "expected the underlying security reason to be surfaced, got: {stderr}"
    );
}

/// Exercises the `path_style` auto-default. floci is a non-AWS endpoint, so
/// omitting `path_style` from `.cartog.toml` MUST still produce a working
/// pull (the implementation infers path-style from the non-AWS host). The
/// pre-patch code defaulted to virtual-host style and would have failed with
/// a DNS lookup against `<bucket>.localhost`.
#[test]
fn pull_without_explicit_path_style_uses_path_style_against_floci() {
    if skip_unless_deps_present() {
        return;
    }
    let floci = match FlociContainer::start() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: could not start floci container");
            return;
        }
    };
    let endpoint = floci.endpoint();
    create_bucket(&endpoint, "cartog-default-pathstyle");

    let work = tempfile::TempDir::new().unwrap();
    let repo = work.path().join("repo");
    let src_db = work.path().join("src.sqlite");
    build_minimal_index(&repo, &src_db);

    // Note the deliberate ABSENCE of `path_style = true` here — the auto-
    // default must infer it from the non-AWS endpoint.
    let cfg_dir = work.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join(".cartog.toml"),
        format!(
            r#"[remote]
url = "s3://cartog-default-pathstyle/index.sqlite"
region = "us-east-1"
endpoint = "{endpoint}"
"#
        ),
    )
    .unwrap();
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cfg_dir)
        .status();

    let env = [
        ("AWS_ACCESS_KEY_ID", "test"),
        ("AWS_SECRET_ACCESS_KEY", "test"),
        ("AWS_DEFAULT_REGION", "us-east-1"),
    ];

    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &src_db.to_string_lossy(), "push"])
        .current_dir(&cfg_dir)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push without explicit path_style failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let dst_db = work.path().join("dst.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &dst_db.to_string_lossy(), "pull"])
        .current_dir(&cfg_dir)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pull without explicit path_style failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&src_db).unwrap(),
        std::fs::read(&dst_db).unwrap()
    );
}

/// Helper: tampered upload setup. Builds a real cartog DB, mutates its
/// `schema_version` row to `file_v`, re-hashes, and uploads with the given
/// `header_v` metadata + a matching sha (so the integrity check passes and
/// pull reaches the schema-version logic). Returns the working directory
/// the caller should use as `current_dir` for `cartog pull`.
fn upload_with_schema_version(
    floci_endpoint: &str,
    bucket: &str,
    key: &str,
    file_v: u32,
    header_v: u32,
) -> tempfile::TempDir {
    let work = tempfile::TempDir::new().unwrap();
    let repo = work.path().join("repo");
    let src_db = work.path().join("src.sqlite");
    build_minimal_index(&repo, &src_db);

    // Mutate the schema_version row directly. This requires the DB to have
    // no live writers, which is true because build_minimal_index finishes
    // before this runs.
    {
        let conn = rusqlite::Connection::open(&src_db).unwrap();
        conn.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'schema_version'",
            [&file_v.to_string()],
        )
        .unwrap();
    }

    // Recompute sha256 of the mutated file so the header matches the body.
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(&src_db).unwrap();
    let mut h = Sha256::new();
    h.update(&bytes);
    let sha = h
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    let st = Command::new("aws")
        .args([
            "--endpoint-url",
            floci_endpoint,
            "s3",
            "cp",
            &src_db.to_string_lossy(),
            &format!("s3://{bucket}/{key}"),
            "--metadata",
            &format!("sha256={sha},schema-version={header_v},cartog-version=test"),
        ])
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .status()
        .unwrap();
    assert!(st.success(), "aws s3 cp failed");

    let cfg_dir = work.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join(".cartog.toml"),
        format!(
            r#"[remote]
url = "s3://{bucket}/{key}"
region = "us-east-1"
endpoint = "{floci_endpoint}"
path_style = true
"#
        ),
    )
    .unwrap();
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cfg_dir)
        .status();

    work
}

/// A DB whose `schema_version` row is greater than this cartog supports must
/// be refused with the "upgrade cartog" message — even when the metadata
/// header agrees (so the cross-check arm doesn't fire first).
#[test]
fn pull_refuses_future_schema_version() {
    if skip_unless_deps_present() {
        return;
    }
    let floci = match FlociContainer::start() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: could not start floci container");
            return;
        }
    };
    let endpoint = floci.endpoint();
    create_bucket(&endpoint, "cartog-future");

    // Pick a version that's plausibly future (current is 4 today; pick
    // CURRENT + a wide margin so this test stays valid as the schema
    // evolves without forcing test updates on every migration).
    let future_v = cartog::db::CURRENT_SCHEMA_VERSION + 100;
    let work = upload_with_schema_version(
        &endpoint,
        "cartog-future",
        "index.sqlite",
        future_v,
        future_v,
    );
    let cfg_dir = work.path().join("cfg");

    let dst_db = work.path().join("dst.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &dst_db.to_string_lossy(), "pull"])
        .current_dir(&cfg_dir)
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "pull of a future-version DB must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Upgrade cartog") || stderr.contains("supports up to"),
        "expected future-version refusal, got: {stderr}"
    );
    assert!(!dst_db.exists(), "destination must not be created");
}

/// When the object's `x-amz-meta-schema-version` header disagrees with the
/// file's `schema_version` row, pull must refuse. This catches partial
/// uploads and hand-edited S3 metadata. The fix in round 2 made both signals
/// required and cross-checked; this test pins the behavior so a future
/// patch can't quietly drop one of them.
#[test]
fn pull_refuses_header_vs_file_mismatch() {
    if skip_unless_deps_present() {
        return;
    }
    let floci = match FlociContainer::start() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: could not start floci container");
            return;
        }
    };
    let endpoint = floci.endpoint();
    create_bucket(&endpoint, "cartog-version-skew");

    // File says v4 (or whatever CURRENT is), header lies and says v3.
    let work = upload_with_schema_version(
        &endpoint,
        "cartog-version-skew",
        "index.sqlite",
        cartog::db::CURRENT_SCHEMA_VERSION,
        cartog::db::CURRENT_SCHEMA_VERSION.saturating_sub(1).max(1),
    );
    let cfg_dir = work.path().join("cfg");

    let dst_db = work.path().join("dst.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_cartog"))
        .args(["--db", &dst_db.to_string_lossy(), "pull"])
        .current_dir(&cfg_dir)
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "pull with header/file schema mismatch must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("schema-version mismatch"),
        "expected mismatch refusal, got: {stderr}"
    );
    assert!(!dst_db.exists(), "destination must not be created");
}
