//! Opt-in S3-compatible index sync (`cartog push` / `cartog pull`).
//!
//! Compiled under the `remote-s3` feature, ON by default. Users who want a
//! minimal binary disable it with `--no-default-features --features lsp`.
//!
//! Credentials come exclusively from the AWS environment chain (env vars,
//! profile, IMDS). `.cartog.toml` `[remote]` rejects credential-shaped keys
//! at parse time — see `crate::config::RemoteConfig`.

use anyhow::{bail, Result};
use std::path::Path;

use crate::config::CartogConfig;
#[cfg(feature = "remote-s3")]
use crate::config::RemoteConfig;

// -----------------------------------------------------------------------------
// Helpers shared by push/pull/doctor — only compiled with the feature on.
// -----------------------------------------------------------------------------

#[cfg(feature = "remote-s3")]
use anyhow::{anyhow, Context};

#[cfg(feature = "remote-s3")]
const META_SHA256: &str = "sha256";
#[cfg(feature = "remote-s3")]
const META_SCHEMA_VERSION: &str = "schema-version";
#[cfg(feature = "remote-s3")]
const META_CARTOG_VERSION: &str = "cartog-version";

/// Parse an `s3://bucket/key` URL into `(bucket, key)`.
///
/// Both segments are required and non-empty. Anything else (missing scheme,
/// missing key, trailing slash with no key, embedded credentials) is rejected
/// with a clear error message — we never want to silently push to a bucket-
/// root key or fall back to a guessed default.
#[cfg(feature = "remote-s3")]
pub fn parse_s3_url(url: &str) -> Result<(String, String)> {
    let rest = url
        .strip_prefix("s3://")
        .ok_or_else(|| anyhow!("remote URL must start with `s3://` (got {url:?})"))?;

    if rest.contains('@') {
        bail!("remote URL must not embed credentials; use the AWS env/profile chain");
    }

    let (bucket, key) = rest
        .split_once('/')
        .ok_or_else(|| anyhow!("remote URL must include an object key (got {url:?})"))?;

    if bucket.is_empty() {
        bail!("remote URL has empty bucket name");
    }
    if key.is_empty() {
        bail!("remote URL has empty object key");
    }
    Ok((bucket.to_string(), key.to_string()))
}

/// Stream a file through SHA-256 in fixed-size chunks. Used on push (over the
/// DB) and on pull (over the freshly downloaded `.partial` file) to verify
/// the round-trip without loading the whole file into memory — the index can
/// be hundreds of MB on large repos.
#[cfg(feature = "remote-s3")]
pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read {} for hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

/// Lower-case hex encoding without pulling the `hex` crate into the
/// minimal-features build. SHA-256 hashes are 32 bytes → 64 hex chars; the
/// allocation is bounded.
#[cfg(feature = "remote-s3")]
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

/// Resolve the effective `s3://...` URL: CLI override wins over config.
#[cfg(feature = "remote-s3")]
fn resolve_remote_url(
    remote_cfg: Option<&RemoteConfig>,
    cli_override: Option<&str>,
) -> Result<String> {
    if let Some(u) = cli_override {
        return Ok(u.to_string());
    }
    let from_cfg = remote_cfg.and_then(|r| r.url.as_deref());
    from_cfg
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no remote configured: pass --remote s3://bucket/key or set [remote].url in .cartog.toml"))
}

#[cfg(feature = "remote-s3")]
mod imp {
    use super::*;
    use cartog_db::{checkpoint_wal, read_schema_version_at, CURRENT_SCHEMA_VERSION};
    use cartog_process_lock::find_active_locks;
    use s3::bucket::Bucket;
    use s3::creds::Credentials;
    use s3::region::Region;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// True when `err` is a cross-device-link error from `std::fs::rename`.
    ///
    /// `io::ErrorKind::CrossesDevices` is still unstable, so we match on the
    /// raw OS error code: `EXDEV` is 18 on Linux and macOS;
    /// `ERROR_NOT_SAME_DEVICE` is 17 on Windows. Anything else is a genuine
    /// failure the caller should surface, not silently fall back on.
    fn is_cross_device_error(err: &std::io::Error) -> bool {
        match err.raw_os_error() {
            #[cfg(unix)]
            Some(code) => code == 18, // EXDEV
            #[cfg(windows)]
            Some(code) => code == 17, // ERROR_NOT_SAME_DEVICE
            _ => false,
        }
    }

    /// RAII guard for the `.partial` download file.
    ///
    /// Deletes the file on Drop unless [`PartialGuard::disarm`] is called.
    /// This makes cleanup bulletproof across every fail path in pull —
    /// network errors, checksum mismatch, schema-version refusal, panic in
    /// the tokio runtime — without each branch having to remember to call
    /// `remove_file`. Disarm is called exactly once: right before the
    /// atomic rename that promotes `.partial` to the real DB.
    struct PartialGuard {
        path: PathBuf,
        armed: bool,
    }

    impl PartialGuard {
        fn new(path: PathBuf) -> Self {
            Self { path, armed: true }
        }
        fn disarm(mut self) {
            self.armed = false;
        }
    }

    impl Drop for PartialGuard {
        fn drop(&mut self) {
            if self.armed && self.path.exists() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    /// Build an S3 [`Bucket`] from cartog's [`RemoteConfig`] + a parsed
    /// `(bucket, key)` plus the anonymous flag.
    ///
    /// Region / endpoint / path-style come from `[remote]`; credentials come
    /// from the AWS env chain (or anonymous when `no_sign_request`).
    pub(super) fn build_bucket(
        bucket_name: &str,
        remote_cfg: Option<&RemoteConfig>,
        no_sign_request: bool,
    ) -> Result<Box<Bucket>> {
        let region = match (
            remote_cfg.and_then(|r| r.region.clone()),
            remote_cfg.and_then(|r| r.endpoint.clone()),
        ) {
            (Some(region), Some(endpoint)) => Region::Custom { region, endpoint },
            (Some(region), None) => region
                .parse::<Region>()
                .with_context(|| format!("invalid AWS region {region:?}"))?,
            (None, Some(endpoint)) => Region::Custom {
                region: "us-east-1".to_string(),
                endpoint,
            },
            // Fall back to the AWS env/profile region. rust-s3's parse will
            // raise a descriptive error if nothing resolves.
            (None, None) => std::env::var("AWS_REGION")
                .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                .ok()
                .and_then(|r| r.parse::<Region>().ok())
                .ok_or_else(|| {
                    anyhow!(
                        "no AWS region resolvable: set [remote].region in .cartog.toml \
                         or AWS_REGION in the environment"
                    )
                })?,
        };

        let creds = if no_sign_request {
            Credentials::anonymous().context("build anonymous creds")?
        } else {
            // Walks the AWS chain: env → profile → IMDS.
            Credentials::default().context(
                "resolve AWS credentials (env/profile/IMDS) — pass --no-sign-request for anonymous access",
            )?
        };

        let mut bucket =
            Bucket::new(bucket_name, region, creds).context("construct S3 bucket client")?;

        // `path_style` resolution:
        //  - Explicit user setting always wins (`Some(true)` or `Some(false)`).
        //  - Otherwise: auto-enable when `endpoint` points at a NON-AWS host.
        //    MinIO, Cloudflare R2, and floci all require path-style and the
        //    failure mode without it is a cryptic DNS error against
        //    `<bucket>.<endpoint>`. Real AWS endpoints (FIPS, accelerate,
        //    dualstack, VPC interface) are virtual-hosted and would break
        //    under path-style — those must NOT be flipped automatically.
        //  - If neither `path_style` nor `endpoint` is set: virtual-hosted
        //    (the AWS default).
        let path_style = remote_cfg
            .and_then(|r| r.path_style)
            .unwrap_or_else(|| match remote_cfg.and_then(|r| r.endpoint.as_deref()) {
                Some(ep) => !is_aws_endpoint(ep),
                None => false,
            });
        if path_style {
            bucket = bucket.with_path_style();
        }
        Ok(bucket)
    }

    /// Returns true if the endpoint URL points at an AWS-operated S3 host.
    /// Matches `*.amazonaws.com` and `*.amazonaws.com.cn` (China partition);
    /// also matches GovCloud (`s3-fips.*.amazonaws.com`) since it's a
    /// subdomain of `amazonaws.com`. Case-insensitive.
    ///
    /// Deliberately narrow: we only need to keep `path_style` OFF when the
    /// user explicitly points at an AWS-managed endpoint. Anything else
    /// (MinIO, R2, floci, on-prem) defaults to path-style.
    pub(super) fn is_aws_endpoint(endpoint: &str) -> bool {
        // Lower-case first so the scheme-strip is case-insensitive.
        let lower = endpoint.to_ascii_lowercase();
        let host = lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"))
            .unwrap_or(&lower)
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            // Trailing-dot FQDNs (`s3.amazonaws.com.`) are valid DNS but
            // would slip past the suffix check below. Normalize them away
            // before matching.
            .trim_end_matches('.');
        host.ends_with(".amazonaws.com") || host.ends_with(".amazonaws.com.cn")
    }

    /// Resolve the per-DB state dir + slots that `cartog serve` / `cartog watch`
    /// would acquire. Used to refuse a push or pull while a peer is using the DB.
    fn peers_for_db(db_path: &Path) -> Vec<String> {
        let dir = match crate::state::default_state_dir() {
            Some(d) => d,
            None => return vec![],
        };
        let serve_slot = crate::state::slot_for_db("serve", db_path);
        let watch_slot = crate::state::slot_for_db("watch", db_path);
        find_active_locks(&dir)
            .into_iter()
            .filter(|l| l.slot == serve_slot || l.slot == watch_slot)
            .map(|l| format!("{} (pid {})", l.slot, l.pid))
            .collect()
    }

    pub(super) fn push_index(
        db_path: &Path,
        remote_cfg: Option<&RemoteConfig>,
        cli_override: Option<&str>,
        json: bool,
    ) -> Result<()> {
        let url = resolve_remote_url(remote_cfg, cli_override)?;
        let (bucket_name, key) = parse_s3_url(&url)?;

        // 1) Refuse push while a peer is writing.
        let peers = peers_for_db(db_path);
        if !peers.is_empty() {
            bail!(
                "cannot push: peer cartog process is using this DB ({}). \
                 Stop it (or accept stale push) before retrying.",
                peers.join(", ")
            );
        }

        if !db_path.exists() {
            bail!(
                "no local DB at {} — run `cartog index` first",
                db_path.display()
            );
        }

        // 2) Truncate WAL so the file is self-contained.
        checkpoint_wal(db_path)
            .with_context(|| format!("checkpoint WAL on {}", db_path.display()))?;

        // 3) Hash + schema lookup.
        let sha = sha256_file(db_path)?;
        let schema = read_schema_version_at(db_path)
            .with_context(|| format!("read schema_version from {}", db_path.display()))?;
        let size = std::fs::metadata(db_path)?.len();

        // 4) Async upload via a small tokio current-thread runtime — keep the
        //    rest of cartog sync.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;

        rt.block_on(async {
            let mut bucket = build_bucket(&bucket_name, remote_cfg, false)?;

            bucket.add_header(&format!("x-amz-meta-{META_SHA256}"), &sha);
            bucket.add_header(
                &format!("x-amz-meta-{META_SCHEMA_VERSION}"),
                &schema.to_string(),
            );
            bucket.add_header(
                &format!("x-amz-meta-{META_CARTOG_VERSION}"),
                env!("CARGO_PKG_VERSION"),
            );

            let mut file = tokio::fs::File::open(db_path)
                .await
                .with_context(|| format!("open {}", db_path.display()))?;
            let resp = bucket
                .put_object_stream_with_content_type(&mut file, &key, "application/octet-stream")
                .await
                .context("S3 upload failed")?;
            if !(200..300).contains(&resp.status_code()) {
                bail!("S3 upload returned HTTP {}", resp.status_code());
            }
            Ok::<_, anyhow::Error>(())
        })?;

        if json {
            println!(
                r#"{{"bucket":"{bucket_name}","key":"{key}","size":{size},"sha256":"{sha}","schema_version":{schema}}}"#
            );
        } else {
            println!(
                "pushed {}/{key} ({} bytes, sha256={}…, schema=v{schema})",
                bucket_name,
                size,
                &sha[..8]
            );
        }
        Ok(())
    }

    pub(super) fn pull_index(
        db_path: &Path,
        remote_cfg: Option<&RemoteConfig>,
        cli_override: Option<&str>,
        force: bool,
        no_sign_request: bool,
        json: bool,
    ) -> Result<()> {
        let url = resolve_remote_url(remote_cfg, cli_override)?;
        let (bucket_name, key) = parse_s3_url(&url)?;

        // 1) Refuse to overwrite a live DB unless --force.
        let peers = peers_for_db(db_path);
        if !peers.is_empty() && !force {
            bail!(
                "cannot pull: peer cartog process is using this DB ({}). \
                 Stop it or pass --force to overwrite anyway.",
                peers.join(", ")
            );
        }

        // 2) Ensure the parent dir exists for the .partial file.
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let partial: PathBuf = {
            let mut p = db_path.as_os_str().to_owned();
            p.push(".partial");
            PathBuf::from(p)
        };
        // Clean any leftover .partial from a prior failed pull.
        if partial.exists() {
            std::fs::remove_file(&partial)
                .with_context(|| format!("remove stale {}", partial.display()))?;
        }
        // RAII guard: deletes .partial on any error path until disarmed
        // just before the atomic rename.
        let guard = PartialGuard::new(partial.clone());

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;

        let (expected_sha, schema_meta): (Option<String>, Option<u32>) = rt.block_on(async {
            let bucket = build_bucket(&bucket_name, remote_cfg, no_sign_request)?;

            // 3) Stream-download to .partial.
            let mut out = tokio::fs::File::create(&partial)
                .await
                .with_context(|| format!("create {}", partial.display()))?;
            let status = bucket
                .get_object_to_writer(&key, &mut out)
                .await
                .context("S3 download failed")?;
            if !(200..300).contains(&status) {
                bail!("S3 download returned HTTP {status}");
            }

            // 4) HEAD to read metadata (sha256 + schema_version).
            let (head, code) = bucket
                .head_object(&key)
                .await
                .context("S3 head_object failed")?;
            if !(200..300).contains(&code) {
                bail!("S3 head_object returned HTTP {code}");
            }
            let md: HashMap<String, String> = head.metadata.unwrap_or_default();
            let sha = md.get(META_SHA256).cloned();
            // Distinguish "header absent" (None) from "header present but not
            // a u32" (a hard error). Collapsing both into None made a corrupt
            // or hand-edited `x-amz-meta-schema-version: abc` masquerade as a
            // missing header, sending users to the wrong fix ("re-push") when
            // the real problem is bad metadata they need to clear.
            let schema = match md.get(META_SCHEMA_VERSION) {
                None => None,
                Some(raw) => Some(raw.parse::<u32>().map_err(|_| {
                    anyhow!(
                        "remote object has a malformed `x-amz-meta-{META_SCHEMA_VERSION}` \
                         header ({raw:?}, not an integer). Clear or correct the object \
                         metadata (e.g. via `aws s3api copy-object`) or re-push from a \
                         healthy cartog run."
                    )
                })?),
            };
            Ok::<_, anyhow::Error>((sha, schema))
        })?;

        // 5) Verify SHA-256 against object metadata.
        let actual_sha = sha256_file(&partial)?;
        match expected_sha.as_deref() {
            Some(want) if want == actual_sha => { /* OK */ }
            Some(want) => {
                // guard's Drop wipes .partial.
                bail!(
                    "SHA-256 mismatch on pulled object: expected {want}, got {actual_sha}. \
                     Local file discarded."
                );
            }
            None => {
                // No checksum metadata at all → treat as untrusted. We don't
                // assume the DB is good; refuse rather than silently install.
                bail!(
                    "remote object has no `x-amz-meta-{META_SHA256}` header — refusing to install \
                     an unverified DB. Re-push with a recent cartog version."
                );
            }
        }

        // 6) Schema-version guards. We need to distinguish three failure
        //    modes that previously collapsed into a single
        //    `read_schema_version_at(...).unwrap_or(0)` call:
        //
        //   a) `Err(_)` — genuine SQLite / I/O error reading the file. The
        //      object passed sha256 verification but rusqlite still can't
        //      open it (truncation, permission denied, corrupted SQLite
        //      header). Surface the underlying error verbatim; do NOT
        //      pretend it's a "not a cartog database" issue.
        //   b) `Ok(0)` — file opens as SQLite but the cartog `metadata`
        //      table or `schema_version` row is missing. That's either an
        //      unrelated app's DB or a corrupted/truncated upload. Refuse.
        //   c) `Ok(v)` with `v > CURRENT_SCHEMA_VERSION` — a future cartog
        //      pushed it. Refuse with an actionable message.
        //
        //    `schema_meta` (the header) is also required: it's the only
        //    signal that can't be tampered with by mismatching header vs
        //    body (the sha256 binds them). A missing header is treated as
        //    a refusal to make downgrade attacks via header-stripping
        //    impossible.
        let pulled_schema = read_schema_version_at(&partial)
            .with_context(|| format!("read schema_version from pulled {}", partial.display()))?;
        if pulled_schema == 0 {
            bail!(
                "pulled object is not a cartog database (no `schema_version` row). \
                 Refusing to install; the bucket may contain an unrelated SQLite file or \
                 a corrupted upload."
            );
        }
        let claimed_schema = schema_meta.ok_or_else(|| {
            anyhow!(
                "remote object has no `x-amz-meta-{META_SCHEMA_VERSION}` header — refusing to \
                 install. Re-push with a recent cartog version so the header is set."
            )
        })?;
        if pulled_schema != claimed_schema {
            bail!(
                "schema-version mismatch: object metadata claims v{claimed_schema} but the \
                 file's `schema_version` row says v{pulled_schema}. Refusing to install — \
                 this usually means a partial / corrupted upload, or that someone edited \
                 the S3 object metadata by hand. Re-push from a healthy cartog run."
            );
        }
        if pulled_schema > CURRENT_SCHEMA_VERSION {
            bail!(
                "pulled DB has schema v{pulled_schema} but this cartog only supports up to \
                 v{CURRENT_SCHEMA_VERSION}. Upgrade cartog before pulling — the remote was \
                 pushed by a newer cartog."
            );
        }

        // 7) Re-check for peers right before the rename window. The
        //    download above can take many seconds; a `cartog serve` /
        //    `cartog watch` that started during that window now holds an
        //    open SQLite handle to the file we're about to swap. This
        //    re-check closes most of that window but NOT all of it: a
        //    peer that wins the PID-lock election in the few syscalls
        //    between this check and the rename at the end of step 9 can
        //    still race us. The genuine fix is an exclusive file lock
        //    held for the duration of the pull, deferred to a follow-up.
        //    `--force` continues to bypass, matching the step-1 check.
        let peers_now = peers_for_db(db_path);
        if !peers_now.is_empty() && !force {
            // Honest about what we know vs what we hope: the partial file
            // is dropped (RAII guard still armed at this point), the
            // existing local DB at `db_path` is untouched, and WAL/SHM
            // siblings have not been unlinked yet.
            bail!(
                "cannot pull: a peer cartog process started while downloading \
                 ({}). Local DB and its WAL siblings are untouched; the partial \
                 download will be discarded.",
                peers_now.join(", ")
            );
        }

        // 8) Remove stale WAL/SHM siblings — leaving them would cause SQLite
        //    to replay phantom frames into the freshly-pulled DB.
        for ext in ["-wal", "-shm"] {
            let mut sibling = db_path.as_os_str().to_owned();
            sibling.push(ext);
            let sibling = PathBuf::from(sibling);
            if sibling.exists() {
                std::fs::remove_file(&sibling)
                    .with_context(|| format!("remove stale {}", sibling.display()))?;
            }
        }

        // 9) Install the verified file at `db_path`. Prefer an atomic rename
        //    (no torn DB on a mid-step crash). If `.partial` and `db_path`
        //    landed on different filesystems — e.g. the project dir is a bind
        //    mount or `db_path` is symlinked across a tmpfs boundary — rename
        //    fails with EXDEV (`CrossesDevices`). Fall back to copy + remove.
        //    The copy is not atomic, but at this point the bytes are fully
        //    verified and any live peer was already refused (steps 1 + 7), so
        //    a non-atomic write is acceptable for this rare cross-FS case.
        //
        //    The guard stays armed until install succeeds: if both the rename
        //    and the copy fail, Drop wipes `.partial` so we don't leak a
        //    half-installed file.
        if let Err(rename_err) = std::fs::rename(&partial, db_path) {
            if is_cross_device_error(&rename_err) {
                std::fs::copy(&partial, db_path).with_context(|| {
                    format!(
                        "cross-filesystem install (copy {} → {})",
                        partial.display(),
                        db_path.display()
                    )
                })?;
                let _ = std::fs::remove_file(&partial);
            } else {
                return Err(rename_err).with_context(|| {
                    format!("install {} → {}", partial.display(), db_path.display())
                });
            }
        }
        // Installed — the file now lives at `db_path`, not `.partial`.
        guard.disarm();

        let size = std::fs::metadata(db_path)?.len();
        if json {
            println!(
                r#"{{"bucket":"{bucket_name}","key":"{key}","size":{size},"sha256":"{actual_sha}","schema_version":{pulled_schema}}}"#
            );
        } else {
            println!(
                "pulled {}/{key} → {} ({} bytes, sha256={}…, schema=v{pulled_schema})",
                bucket_name,
                db_path.display(),
                size,
                &actual_sha[..8]
            );
        }
        Ok(())
    }

    /// Doctor-mode reachability check: HEAD the configured bucket and report.
    pub(super) fn check_remote_reachable(remote_cfg: &RemoteConfig) -> Result<()> {
        let url = remote_cfg
            .url
            .as_deref()
            .ok_or_else(|| anyhow!("[remote].url is not set"))?;
        let (bucket_name, key) = parse_s3_url(url)?;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let bucket = build_bucket(&bucket_name, Some(remote_cfg), false)?;
            // Use HEAD on the configured key. A 404 there still proves the
            // bucket + creds work; we treat it as "reachable, object absent".
            match bucket.head_object(&key).await {
                Ok((_, code)) if (200..300).contains(&code) => Ok(()),
                Ok((_, 404)) => Ok(()),
                Ok((_, code)) => bail!("HEAD returned HTTP {code}"),
                Err(e) => Err(anyhow!("S3 unreachable: {e}")),
            }
        })
    }
}

// -----------------------------------------------------------------------------
// Feature-gated public surface
// -----------------------------------------------------------------------------

#[cfg(feature = "remote-s3")]
pub fn push_index(
    db_path: &Path,
    config: &CartogConfig,
    cli_override: Option<&str>,
    json: bool,
) -> Result<()> {
    imp::push_index(db_path, config.remote.as_ref(), cli_override, json)
}

#[cfg(feature = "remote-s3")]
pub fn pull_index(
    db_path: &Path,
    config: &CartogConfig,
    cli_override: Option<&str>,
    force: bool,
    no_sign_request: bool,
    json: bool,
) -> Result<()> {
    imp::pull_index(
        db_path,
        config.remote.as_ref(),
        cli_override,
        force,
        no_sign_request,
        json,
    )
}

#[cfg(feature = "remote-s3")]
pub fn check_remote_reachable(remote: &RemoteConfig) -> Result<()> {
    imp::check_remote_reachable(remote)
}

#[cfg(not(feature = "remote-s3"))]
pub fn push_index(
    _db_path: &Path,
    _config: &CartogConfig,
    _cli_override: Option<&str>,
    _json: bool,
) -> Result<()> {
    bail!(
        "cartog was built without the `remote-s3` feature. \
         Reinstall with `cargo install cartog` (default) or `--features remote-s3`."
    )
}

#[cfg(not(feature = "remote-s3"))]
pub fn pull_index(
    _db_path: &Path,
    _config: &CartogConfig,
    _cli_override: Option<&str>,
    _force: bool,
    _no_sign_request: bool,
    _json: bool,
) -> Result<()> {
    bail!(
        "cartog was built without the `remote-s3` feature. \
         Reinstall with `cargo install cartog` (default) or `--features remote-s3`."
    )
}

// `check_remote_reachable` is intentionally absent in the minimal build:
// `commands::check_remote` short-circuits to an Error result instead of
// calling into S3 plumbing that isn't compiled in.

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(all(test, feature = "remote-s3"))]
mod tests {
    use super::*;

    #[test]
    fn parse_s3_url_accepts_simple() {
        let (b, k) = parse_s3_url("s3://my-bucket/cartog/main").unwrap();
        assert_eq!(b, "my-bucket");
        assert_eq!(k, "cartog/main");
    }

    #[test]
    fn parse_s3_url_accepts_nested_key() {
        let (b, k) = parse_s3_url("s3://b/a/b/c.sqlite").unwrap();
        assert_eq!(b, "b");
        assert_eq!(k, "a/b/c.sqlite");
    }

    #[test]
    fn parse_s3_url_rejects_missing_scheme() {
        assert!(parse_s3_url("my-bucket/key").is_err());
        assert!(parse_s3_url("https://bucket/key").is_err());
    }

    #[test]
    fn parse_s3_url_rejects_no_key() {
        assert!(parse_s3_url("s3://my-bucket").is_err());
        assert!(parse_s3_url("s3://my-bucket/").is_err());
    }

    #[test]
    fn parse_s3_url_rejects_empty_bucket() {
        assert!(parse_s3_url("s3:///key").is_err());
    }

    #[test]
    fn parse_s3_url_rejects_embedded_credentials() {
        // Foreclose URL-shaped credentials masquerading as part of the path.
        assert!(parse_s3_url("s3://AKIA:secret@bucket/key").is_err());
    }

    #[test]
    fn sha256_file_matches_known_fixture() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("data.bin");
        // sha256("hello\n") = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        std::fs::write(&p, "hello\n").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn resolve_remote_url_prefers_cli_override() {
        let cfg = RemoteConfig {
            url: Some("s3://from-config/k".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_remote_url(Some(&cfg), Some("s3://from-cli/k")).unwrap(),
            "s3://from-cli/k"
        );
    }

    #[test]
    fn resolve_remote_url_falls_back_to_config() {
        let cfg = RemoteConfig {
            url: Some("s3://from-config/k".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_remote_url(Some(&cfg), None).unwrap(),
            "s3://from-config/k"
        );
    }

    #[test]
    fn resolve_remote_url_errors_without_any_source() {
        assert!(resolve_remote_url(None, None).is_err());
    }

    // ---- is_aws_endpoint --------------------------------------------------

    #[test]
    fn is_aws_endpoint_recognises_standard_hosts() {
        for ep in [
            "https://s3.us-east-1.amazonaws.com",
            "https://s3-fips.us-gov-west-1.amazonaws.com",
            "https://s3.amazonaws.com",
            "https://s3-accelerate.amazonaws.com",
            "https://s3.dualstack.us-west-2.amazonaws.com",
            // China partition.
            "https://s3.cn-north-1.amazonaws.com.cn",
            // Trailing path + port are stripped.
            "https://s3.us-east-1.amazonaws.com:443/v1",
            // Case-insensitive.
            "HTTPS://S3.US-EAST-1.AMAZONAWS.COM",
            // Trailing-dot FQDN (valid DNS, parsers retain it).
            "https://s3.us-east-1.amazonaws.com.",
            "https://s3.amazonaws.com.:443",
        ] {
            assert!(imp::is_aws_endpoint(ep), "should match AWS endpoint: {ep}");
        }
    }

    #[test]
    fn is_aws_endpoint_rejects_non_aws_hosts() {
        for ep in [
            "https://minio.local",
            "https://play.min.io",
            "http://localhost:9000",
            "https://r2.cloudflarestorage.com",
            "https://<account>.r2.cloudflarestorage.com",
            "https://amazonaws.com.evil.example.com",
            // Bare `amazonaws.com` without the leading dot is suspicious;
            // the suffix check requires `.amazonaws.com` to avoid the
            // typosquat above.
            "https://amazonaws.com",
        ] {
            assert!(
                !imp::is_aws_endpoint(ep),
                "should NOT match as AWS endpoint: {ep}"
            );
        }
    }
}
