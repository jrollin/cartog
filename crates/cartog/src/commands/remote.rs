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
#[cfg(feature = "remote-s3")]
const META_GIT_COMMIT: &str = "git-commit";

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
/// DB) to compute the `x-amz-meta-sha256` header without loading the whole
/// file into memory — the index can be hundreds of MB on large repos. Pull no
/// longer calls this: it hashes the download stream in a single pass via
/// `HashingAsyncWriter` (#69).
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
    use cartog_db::{
        checkpoint_wal, read_metadata_at, read_schema_version_at, CURRENT_SCHEMA_VERSION,
    };
    use cartog_process_lock::{find_active_locks, AcquireError, ProcessLock};
    use s3::bucket::Bucket;
    use s3::creds::Credentials;
    use s3::region::Region;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Poll};
    use tokio::io::AsyncWrite;

    /// Actionable suffix for an S3 HTTP error status. 401/403 almost always
    /// mean credentials or bucket policy, not a cartog bug — say so.
    fn http_status_hint(status: u16) -> &'static str {
        match status {
            401 | 403 => " (check AWS credentials and bucket permissions)",
            404 => " (bucket or key not found — check the remote URL)",
            _ => "",
        }
    }

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

    /// Tee [`AsyncWrite`] that SHA-256-hashes bytes on their way to `inner`.
    ///
    /// Lets pull compute the digest in a single pass over the download stream,
    /// avoiding a second full read of `.partial` (#69). It hashes only the `n`
    /// bytes `inner` *acknowledges* per `poll_write`, never the whole input
    /// buffer — so the finalized hash is provably over the exact byte sequence
    /// written through it, even under partial writes.
    struct HashingAsyncWriter<W> {
        inner: W,
        hasher: Sha256,
    }

    impl<W> HashingAsyncWriter<W> {
        fn new(inner: W) -> Self {
            Self {
                inner,
                hasher: Sha256::new(),
            }
        }

        /// Consume the writer and return the lowercase-hex SHA-256 of every byte
        /// written through it.
        #[must_use]
        fn finalize_hex(self) -> String {
            hex_encode(&self.hasher.finalize())
        }
    }

    impl<W: AsyncWrite + Unpin> AsyncWrite for HashingAsyncWriter<W> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            match Pin::new(&mut self.inner).poll_write(cx, buf) {
                Poll::Ready(Ok(n)) => {
                    self.hasher.update(&buf[..n]);
                    Poll::Ready(Ok(n))
                }
                other => other,
            }
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
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
    /// would acquire. Used to refuse a push while a peer is using the DB.
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

    /// `, commit=<short>` suffix. Whitelists ASCII hex (a git SHA is hex), so a
    /// hand-edited/forged value can never emit control codes; empty → no suffix.
    fn commit_suffix(commit: Option<&str>) -> String {
        let hex: String = commit
            .unwrap_or("")
            .chars()
            .filter(char::is_ascii_hexdigit)
            .take(8)
            .collect();
        if hex.is_empty() {
            String::new()
        } else {
            format!(", commit={hex}")
        }
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

        // 3) Hash + schema lookup. Push reads the file twice (hash here, then
        //    stream the body below): S3 wants `x-amz-meta-sha256` in the request
        //    headers, sent before the body, but a streamed hash is only known
        //    after it. Trailing checksums (`x-amz-trailer`/`aws-chunked`) would
        //    fix it, but rust-s3 0.37 has no public API for them. The first read
        //    is the local file (fast), so we keep the two-read approach (#69).
        let sha = sha256_file(db_path)?;
        let schema = read_schema_version_at(db_path)
            .with_context(|| format!("read schema_version from {}", db_path.display()))?;
        // Commit the index was built at; absent on a non-git index (header omitted).
        let git_commit = read_metadata_at(db_path, "last_commit")
            .with_context(|| format!("read last_commit from {}", db_path.display()))?;
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
            if let Some(commit) = git_commit.as_deref() {
                bucket.add_header(&format!("x-amz-meta-{META_GIT_COMMIT}"), commit);
            }

            let mut file = tokio::fs::File::open(db_path)
                .await
                .with_context(|| format!("open {}", db_path.display()))?;
            let resp = bucket
                .put_object_stream_with_content_type(&mut file, &key, "application/octet-stream")
                .await
                .context("S3 upload failed")?;
            if !(200..300).contains(&resp.status_code()) {
                bail!(
                    "S3 upload returned HTTP {}{}",
                    resp.status_code(),
                    http_status_hint(resp.status_code())
                );
            }
            Ok::<_, anyhow::Error>(())
        })?;

        if json {
            // serde serialization escapes every field; git_commit is null when absent.
            let value = serde_json::json!({
                "bucket": bucket_name,
                "key": key,
                "size": size,
                "sha256": sha,
                "schema_version": schema,
                "git_commit": git_commit,
            });
            println!("{}", serde_json::to_string(&value)?);
        } else {
            println!(
                "pushed {}/{key} ({} bytes, sha256={}…, schema=v{schema}{})",
                bucket_name,
                size,
                &sha[..8],
                commit_suffix(git_commit.as_deref())
            );
        }
        Ok(())
    }

    /// Hold the serve+watch slots for the whole pull so no peer opens the DB mid-install (#68).
    /// `Ok(None)` = proceed unguarded (`--force` past a live peer, or unlockable state dir).
    pub(super) fn acquire_pull_locks(
        state_dir: &Path,
        db_path: &Path,
        force: bool,
    ) -> Result<Option<Vec<ProcessLock>>> {
        let slots = [
            crate::state::slot_for_db("serve", db_path),
            crate::state::slot_for_db("watch", db_path),
        ];
        let mut locks = Vec::with_capacity(slots.len());
        for slot in &slots {
            match ProcessLock::acquire(state_dir, slot) {
                Ok(lock) => locks.push(lock),
                Err(AcquireError::Held(held)) => {
                    if force {
                        eprintln!(
                            "warning: pulling with --force while a peer is live ({} (pid {})); \
                             its open DB handle may be corrupted by the swap",
                            held.slot, held.pid
                        );
                        return Ok(None);
                    }
                    bail!(
                        "cannot pull: peer cartog process is using this DB ({} (pid {})). \
                         Stop it or pass --force to overwrite anyway.",
                        held.slot,
                        held.pid
                    );
                }
                Err(AcquireError::Io(e)) => {
                    // Peers can't lock an unlockable dir either; degrade to best effort.
                    eprintln!(
                        "warning: cannot lock {} ({e}); pulling without peer exclusion",
                        state_dir.display()
                    );
                    return Ok(None);
                }
            }
        }
        Ok(Some(locks))
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

        // 1) Hold the peer slots for the whole pull (#68); a peer starting mid-download
        //    now loses its election instead of opening the file we're about to swap.
        let _peer_locks = match crate::state::default_state_dir() {
            Some(dir) => acquire_pull_locks(&dir, db_path, force)?,
            None => None,
        };

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

        let (actual_sha, expected_sha, schema_meta, commit_meta): (
            String,
            Option<String>,
            Option<u32>,
            Option<String>,
        ) = rt.block_on(async {
            let bucket = build_bucket(&bucket_name, remote_cfg, no_sign_request)?;

            // 3) Stream-download to .partial, hashing the bytes as they flow so
            //    we don't re-read the file to verify the checksum (#69).
            let out = tokio::fs::File::create(&partial)
                .await
                .with_context(|| format!("create {}", partial.display()))?;
            let mut tee = HashingAsyncWriter::new(out);
            let status = bucket
                .get_object_to_writer(&key, &mut tee)
                .await
                .context("S3 download failed")?;
            if !(200..300).contains(&status) {
                bail!(
                    "S3 download returned HTTP {status}{}",
                    http_status_hint(status)
                );
            }
            // Shut down the writer so its final buffered bytes reach the OS
            // before we hand `.partial` to the schema/rename steps below. The
            // digest itself is over the bytes written through the tee (in
            // memory), not a re-read of the file.
            tokio::io::AsyncWriteExt::shutdown(&mut tee)
                .await
                .with_context(|| format!("flush {}", partial.display()))?;
            let actual_sha = tee.finalize_hex();

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
            let commit = md.get(META_GIT_COMMIT).cloned();
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
            Ok::<_, anyhow::Error>((actual_sha, sha, schema, commit))
        })?;

        // 5) Verify SHA-256 against object metadata. `actual_sha` was computed
        //    in a single pass over the download stream above (#69) — no re-read.
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

        // 6b) Git-commit provenance, report-only. When both header and file
        //     row are present they must agree (catches partial/edited uploads);
        //     an absent header is fine. Staleness is the caller's decision.
        let file_commit = read_metadata_at(&partial, "last_commit")
            .with_context(|| format!("read last_commit from pulled {}", partial.display()))?;
        if let (Some(header), Some(file)) = (commit_meta.as_deref(), file_commit.as_deref()) {
            if header != file {
                // {:?} so an untrusted header can't inject terminal control codes.
                bail!(
                    "git-commit mismatch: object metadata claims commit {header:?} but the \
                     file's `last_commit` row says {file:?}. Refusing to install — this usually \
                     means a partial / corrupted upload, or hand-edited S3 metadata. Re-push \
                     from a healthy cartog run."
                );
            }
        }
        // Report the file's own row only — never the unvalidated S3 header.
        let report_commit = file_commit;

        // 7) Remove stale WAL/SHM siblings — leaving them would cause SQLite
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

        // 8) Install the verified file at `db_path`. Prefer an atomic rename
        //    (no torn DB on a mid-step crash). If `.partial` and `db_path`
        //    landed on different filesystems — e.g. the project dir is a bind
        //    mount or `db_path` is symlinked across a tmpfs boundary — rename
        //    fails with EXDEV (`CrossesDevices`). Fall back to copy + remove.
        //    The copy is not atomic, but at this point the bytes are fully
        //    verified and the peer locks taken in step 1 are still held, so
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

        // A pull replaces the whole index, so every cached count in the
        // registry is now wrong about this project. Re-read them: unlike the
        // index hooks this opens a database of its own, which is affordable
        // precisely because a pull just finished a network transfer. Held
        // locks make this race-free — no peer can be writing.
        //
        // Best-effort: a pulled index that cannot be re-opened is still a
        // successful pull, so a failure here leaves the row stale rather than
        // failing the command.
        // `open_existing_rw` skips migrations: registering a pulled index must
        // never migrate it as a side effect. A future-schema pull is refused
        // earlier; an older one is the user's to migrate deliberately.
        match cartog_db::Database::open_existing_rw(db_path) {
            Ok(db) => {
                let root = cartog_registry::infer_root_from_db_path(db_path);
                crate::registry_hook::record_indexed(&db, db_path, &root);
            }
            Err(e) => tracing::warn!(
                db = %db_path.display(),
                error = %e,
                "pulled index could not be re-opened to refresh the project registry"
            ),
        }

        let size = std::fs::metadata(db_path)?.len();
        if json {
            // serde serialization escapes every field; git_commit is null when absent.
            let value = serde_json::json!({
                "bucket": bucket_name,
                "key": key,
                "size": size,
                "sha256": actual_sha,
                "schema_version": pulled_schema,
                "git_commit": report_commit,
            });
            println!("{}", serde_json::to_string(&value)?);
        } else {
            println!(
                "pulled {}/{key} → {} ({} bytes, sha256={}…, schema=v{pulled_schema}{})",
                bucket_name,
                db_path.display(),
                size,
                &actual_sha[..8],
                commit_suffix(report_commit.as_deref())
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn http_status_hint_flags_auth_and_not_found() {
            assert!(http_status_hint(401).contains("credentials"));
            assert!(http_status_hint(403).contains("credentials"));
            assert!(http_status_hint(404).contains("not found"));
            assert_eq!(http_status_hint(500), "");
            assert_eq!(http_status_hint(200), "");
        }

        #[test]
        fn is_cross_device_error_matches_exdev_only() {
            let exdev = std::io::Error::from_raw_os_error(18);
            assert!(
                is_cross_device_error(&exdev),
                "EXDEV (18) is the cross-device case"
            );
            let enoent = std::io::Error::from_raw_os_error(2);
            assert!(
                !is_cross_device_error(&enoent),
                "ENOENT is not cross-device"
            );
            let no_os = std::io::Error::other("synthetic");
            assert!(
                !is_cross_device_error(&no_os),
                "an error without an OS code is not cross-device"
            );
        }

        #[test]
        fn commit_suffix_whitelists_hex_and_truncates() {
            assert_eq!(commit_suffix(None), "");
            assert_eq!(commit_suffix(Some("1234567890abcdef")), ", commit=12345678");
            // Control codes / multibyte / punctuation are filtered out: only the
            // hex digits survive (the ESC, '[', 'm' are gone; '3','1' are hex).
            assert_eq!(commit_suffix(Some("\x1b[31mabc")), ", commit=31abc");
            assert!(!commit_suffix(Some("\x1b[31mabc")).contains('\x1b'));
            assert_eq!(commit_suffix(Some("12é34")), ", commit=1234");
            // Nothing hex-like → no suffix (safe default).
            assert_eq!(commit_suffix(Some("zzz!!!")), "");
        }

        // The acceptance-criteria invariant for #69: the streamed hash must equal
        // the SHA-256 of the exact bytes that landed on disk. We pin it against
        // `sha256_file`, the function pull used to call for the second read.
        #[tokio::test]
        async fn hashing_writer_hash_equals_sha256_of_written_bytes() {
            use tokio::io::AsyncWriteExt;
            let payload = b"the quick brown fox\njumps over the lazy dog\n";

            let mut tee = HashingAsyncWriter::new(Vec::new());
            tee.write_all(payload).await.unwrap();
            tee.shutdown().await.unwrap();
            let streamed = tee.finalize_hex();

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("payload.bin");
            std::fs::write(&path, payload).unwrap();
            let on_disk = sha256_file(&path).unwrap();

            assert_eq!(streamed, on_disk);
        }

        #[tokio::test]
        async fn hashing_writer_handles_multi_chunk_writes() {
            use tokio::io::AsyncWriteExt;
            let chunks: [&[u8]; 3] = [b"first-", b"second-", b"third"];

            let mut tee = HashingAsyncWriter::new(Vec::new());
            for chunk in chunks {
                tee.write_all(chunk).await.unwrap();
            }
            tee.shutdown().await.unwrap();
            let streamed = tee.finalize_hex();

            // Several writes hash the same as one write of the concatenation
            // (the tee accumulates across calls). Short-write handling is
            // covered separately by the OneBytePerWrite test below.
            let mut whole = HashingAsyncWriter::new(Vec::new());
            whole.write_all(b"first-second-third").await.unwrap();
            whole.shutdown().await.unwrap();
            assert_eq!(streamed, whole.finalize_hex());
        }

        #[tokio::test]
        async fn hashing_writer_inner_receives_all_bytes() {
            use tokio::io::AsyncWriteExt;
            let payload = b"tee duplicates, it does not divert";

            let mut tee = HashingAsyncWriter::new(Vec::new());
            tee.write_all(payload).await.unwrap();
            tee.shutdown().await.unwrap();

            assert_eq!(tee.inner.as_slice(), payload);
        }

        /// An `AsyncWrite` that accepts at most one byte per `poll_write`,
        /// forcing the short-write path a real `tokio::fs::File` exhibits under
        /// backpressure. `write_all` re-submits the remaining tail.
        struct OneBytePerWrite(Vec<u8>);

        impl AsyncWrite for OneBytePerWrite {
            fn poll_write(
                mut self: Pin<&mut Self>,
                _cx: &mut TaskContext<'_>,
                buf: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                if buf.is_empty() {
                    return Poll::Ready(Ok(0));
                }
                self.0.push(buf[0]);
                Poll::Ready(Ok(1))
            }
            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut TaskContext<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _cx: &mut TaskContext<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        // Guards the `&buf[..n]` invariant: under short writes the tee must hash
        // only the acknowledged byte each call, so the total equals the
        // full-buffer hash. Hashing the whole `buf` (a plausible regression)
        // would over-count every retried tail and diverge here.
        #[tokio::test]
        async fn hashing_writer_hashes_only_acknowledged_bytes_under_short_writes() {
            use tokio::io::AsyncWriteExt;
            let payload = b"short-write stress: every poll_write takes one byte";

            let mut tee = HashingAsyncWriter::new(OneBytePerWrite(Vec::new()));
            tee.write_all(payload).await.unwrap();
            tee.shutdown().await.unwrap();
            assert_eq!(tee.inner.0.as_slice(), payload, "inner got every byte");
            let short = tee.finalize_hex();

            let mut whole = HashingAsyncWriter::new(Vec::new());
            whole.write_all(payload).await.unwrap();
            whole.shutdown().await.unwrap();

            assert_eq!(short, whole.finalize_hex());
        }
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

    // ── build_bucket region resolution ────────────────────────────────

    #[test]
    fn build_bucket_uses_custom_region_and_endpoint() {
        let cfg = RemoteConfig {
            region: Some("eu-west-3".into()),
            endpoint: Some("https://minio.local:9000".into()),
            ..Default::default()
        };
        // Anonymous creds so the test doesn't depend on an AWS credential chain.
        let bucket = imp::build_bucket("my-bucket", Some(&cfg), true).unwrap();
        assert_eq!(bucket.region.to_string(), "eu-west-3");
    }

    #[test]
    fn build_bucket_parses_named_region_without_endpoint() {
        let cfg = RemoteConfig {
            region: Some("us-east-1".into()),
            ..Default::default()
        };
        let bucket = imp::build_bucket("my-bucket", Some(&cfg), true).unwrap();
        assert_eq!(bucket.region.to_string(), "us-east-1");
    }

    #[test]
    fn build_bucket_defaults_region_when_only_endpoint_given() {
        let cfg = RemoteConfig {
            endpoint: Some("https://r2.example.com".into()),
            ..Default::default()
        };
        let bucket = imp::build_bucket("my-bucket", Some(&cfg), true).unwrap();
        // No region configured + a custom endpoint → fall back to us-east-1.
        assert_eq!(bucket.region.to_string(), "us-east-1");
    }

    #[test]
    #[serial_test::serial]
    fn build_bucket_falls_back_to_aws_region_env() {
        let saved_region = std::env::var("AWS_REGION").ok();
        let saved_default = std::env::var("AWS_DEFAULT_REGION").ok();
        // SAFETY: serialized via #[serial]; restored below regardless of outcome.
        unsafe {
            std::env::set_var("AWS_REGION", "ap-southeast-2");
            std::env::remove_var("AWS_DEFAULT_REGION");
        }

        let result = imp::build_bucket("my-bucket", None, true);

        unsafe {
            match saved_region {
                Some(v) => std::env::set_var("AWS_REGION", v),
                None => std::env::remove_var("AWS_REGION"),
            }
            match saved_default {
                Some(v) => std::env::set_var("AWS_DEFAULT_REGION", v),
                None => std::env::remove_var("AWS_DEFAULT_REGION"),
            }
        }

        let bucket = result.expect("env region resolves a bucket");
        assert_eq!(bucket.region.to_string(), "ap-southeast-2");
    }

    #[test]
    #[serial_test::serial]
    fn build_bucket_errors_when_no_region_resolvable() {
        let saved_region = std::env::var("AWS_REGION").ok();
        let saved_default = std::env::var("AWS_DEFAULT_REGION").ok();
        unsafe {
            std::env::remove_var("AWS_REGION");
            std::env::remove_var("AWS_DEFAULT_REGION");
        }

        let result = imp::build_bucket("my-bucket", None, true);

        unsafe {
            match saved_region {
                Some(v) => std::env::set_var("AWS_REGION", v),
                None => std::env::remove_var("AWS_REGION"),
            }
            match saved_default {
                Some(v) => std::env::set_var("AWS_DEFAULT_REGION", v),
                None => std::env::remove_var("AWS_DEFAULT_REGION"),
            }
        }

        let err = result.expect_err("no region anywhere must fail");
        assert!(
            err.to_string().contains("no AWS region resolvable"),
            "actionable error names the missing region: {err}"
        );
    }

    #[test]
    fn pull_locks_exclude_peers_for_their_lifetime() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("proj.db");
        let serve_slot = crate::state::slot_for_db("serve", &db);
        let watch_slot = crate::state::slot_for_db("watch", &db);

        let locks = imp::acquire_pull_locks(dir.path(), &db, false)
            .unwrap()
            .expect("free slots must be acquired");
        // The previously-racy window (#68): a peer election mid-pull must lose.
        assert!(matches!(
            cartog_process_lock::ProcessLock::acquire(dir.path(), &serve_slot),
            Err(cartog_process_lock::AcquireError::Held(_))
        ));
        assert!(matches!(
            cartog_process_lock::ProcessLock::acquire(dir.path(), &watch_slot),
            Err(cartog_process_lock::AcquireError::Held(_))
        ));

        drop(locks);
        assert!(
            cartog_process_lock::ProcessLock::acquire(dir.path(), &serve_slot).is_ok(),
            "slots free again after the pull releases"
        );
    }

    #[test]
    fn pull_locks_refusal_names_holder_and_releases_partial_acquisitions() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("proj.db");
        let serve_slot = crate::state::slot_for_db("serve", &db);
        let watch_slot = crate::state::slot_for_db("watch", &db);
        let _peer = cartog_process_lock::ProcessLock::acquire(dir.path(), &watch_slot).unwrap();

        let err = imp::acquire_pull_locks(dir.path(), &db, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(&watch_slot), "names the held slot: {msg}");
        assert!(msg.contains("--force"), "points at the override: {msg}");
        assert!(
            cartog_process_lock::ProcessLock::acquire(dir.path(), &serve_slot).is_ok(),
            "the transiently-acquired serve slot is released on refusal"
        );
    }

    #[test]
    fn pull_locks_force_proceeds_unguarded_when_peer_is_live() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("proj.db");
        let serve_slot = crate::state::slot_for_db("serve", &db);
        let _peer = cartog_process_lock::ProcessLock::acquire(dir.path(), &serve_slot).unwrap();

        let locks = imp::acquire_pull_locks(dir.path(), &db, true).unwrap();
        assert!(locks.is_none(), "--force pulls without holding locks");
    }

    #[test]
    fn pull_locks_force_still_locks_free_slots() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("proj.db");

        let locks = imp::acquire_pull_locks(dir.path(), &db, true).unwrap();
        assert!(
            locks.is_some(),
            "--force takes the locks when nobody holds them"
        );
    }
}
