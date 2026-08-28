//! Config file loading, validation, and database-path resolution.

use super::*;
use crate::config::repair::{is_unknown_field_error, reparse_ignoring_unknown_keys};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Outcome of [`load_config`]. Distinguishes three states the caller may need
/// to react to differently:
///
/// - **`Loaded { config, path }`** — `.cartog.toml` parsed successfully.
/// - **`Missing`** — no config file was found anywhere on the walk-up to
///   git root; caller proceeds with defaults silently.
/// - **`Rejected { path }`** — a config file was found but rejected (parse
///   error, security pre-check, or `deny_unknown_fields` violation).
///   `read_config` already printed the underlying reason to stderr. Callers
///   that read `[remote]` (push/pull/doctor) must NOT silently fall back to
///   defaults here, or the user's security-error message would be drowned
///   by a misleading downstream "no remote configured" error.
// `Loaded` carries the full `CartogConfig` (~344 B) while the other variants
// are tiny. We accept the size disparity rather than box the payload: every
// real call site moves the config back onto the stack immediately, so a
// `Box` would just add one heap alloc + memcpy per invocation for no
// benefit. The lint is correct in general; not correct here.
#[allow(clippy::large_enum_variant)]
pub enum ConfigLoad {
    Loaded { config: CartogConfig, path: PathBuf },
    Missing,
    Rejected { path: PathBuf },
}

impl ConfigLoad {
    /// Convenience: the parsed config when present, or a fresh default
    /// otherwise. Use for commands that don't care about distinguishing
    /// missing-vs-rejected (most read-only commands).
    pub fn config_or_default(self) -> CartogConfig {
        match self {
            ConfigLoad::Loaded { config, .. } => config,
            _ => CartogConfig::default(),
        }
    }

    /// The path the config was loaded from (or attempted to load from when
    /// `Rejected`). Used by `cartog doctor` and `cartog config` to display
    /// the file under inspection.
    pub fn path(&self) -> Option<&Path> {
        match self {
            ConfigLoad::Loaded { path, .. } | ConfigLoad::Rejected { path } => Some(path),
            ConfigLoad::Missing => None,
        }
    }

    /// True when a `.cartog.toml` was found but failed validation. Callers
    /// that depend on `[remote]` (push, pull, doctor, config) use this to
    /// distinguish "no config" from "broken config" and surface a clear
    /// rejection rather than silently falling back to defaults.
    pub fn is_rejected(&self) -> bool {
        matches!(self, ConfigLoad::Rejected { .. })
    }

    /// Whether the user has opted this project in to a cartog index.
    ///
    /// Writing a `.cartog.toml` *is* the opt-in, so a file that was found but
    /// rejected still counts: the question "may cartog create an index here?" is
    /// answered by the file's existence, not by its contents being valid.
    /// Deriving consent from parse success instead conflated the two and made a
    /// single typo report `no .cartog.toml in this project` at a user who was
    /// looking straight at one.
    ///
    /// `Rejected` is wider than a syntax error: `read_config` also returns it for
    /// a credential-shaped `[remote]` key, a userinfo-bearing `endpoint`, an
    /// unknown `provider`, a malformed `[index] exclude` glob, and an unreadable
    /// file (EACCES/EIO). All of them grant consent — indexing reads none of
    /// those sections, and `main` still hard-refuses `push`/`pull` on a rejected
    /// config, so the credential checks keep their teeth.
    ///
    /// Settings still fail closed on a rejected config — [`config_or_default`]
    /// hands back defaults and `read_config` has already explained why on
    /// stderr. Only the consent signal is decoupled.
    ///
    /// [`config_or_default`]: ConfigLoad::config_or_default
    #[must_use]
    pub fn consent(&self) -> IndexConsent {
        match self {
            ConfigLoad::Loaded { .. } | ConfigLoad::Rejected { .. } => IndexConsent::Granted,
            ConfigLoad::Missing => IndexConsent::Absent,
        }
    }
}

/// Whether cartog may create a `.cartog/` index for this project.
///
/// A distinct type rather than a `bool` so the two states are named at every
/// call site: `allow_index_creation(&path, true)` gave no hint which `true`
/// meant, and the parameter was easy to pass inverted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexConsent {
    /// The user opted in — a `.cartog.toml` is present (parsed or not).
    Granted,
    /// No opt-in signal from a config file.
    Absent,
}

impl IndexConsent {
    /// Whether this is [`IndexConsent::Granted`].
    #[must_use]
    pub fn is_granted(self) -> bool {
        matches!(self, IndexConsent::Granted)
    }
}

/// Whether cartog may create a fresh index, and why not when it may not.
///
/// One definition for `main` and `doctor`: doctor used to re-derive its own
/// `db_path_unknown` with a "Mirror `main`" comment, and the copies drifted —
/// doctor's omitted the explicit-`--db` term, so it could report a
/// db-path-unknown state for a run that would have succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexCreation {
    /// A fresh `.cartog/` may be created.
    Allowed,
    /// No opt-in signal: no `.cartog.toml`, no existing DB, no `CARTOG_AUTO_INIT`.
    RefusedNoConsent,
    /// A rejected config may have named a `[database] path` we could not read,
    /// so creating a fresh index would silently materialize it at the default
    /// location the user configured away from. `--db`/`CARTOG_DB` or an existing
    /// DB settles the location and lifts this.
    RefusedUnknownDbPath,
}

impl IndexCreation {
    /// Resolve the gate. `db_override` is the explicit `--db`/`CARTOG_DB` path,
    /// which settles the location question on its own.
    #[must_use]
    pub fn resolve(
        db_path: &Path,
        consent: IndexConsent,
        config_rejected: bool,
        db_override: Option<&Path>,
    ) -> Self {
        if config_rejected && db_override.is_none() && !db_path.exists() {
            return Self::RefusedUnknownDbPath;
        }
        if allow_index_creation(db_path, consent) {
            Self::Allowed
        } else {
            Self::RefusedNoConsent
        }
    }

    /// Whether a fresh index may be created.
    #[must_use]
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Whether the refusal is specifically an unreadable `[database] path`.
    #[must_use]
    pub fn is_db_path_unknown(self) -> bool {
        matches!(self, Self::RefusedUnknownDbPath)
    }
}

/// Known non-language keys of `[lsp]`. A scalar `[lsp]` key outside this set is
/// a typo, not a language name (a language entry is a table: `[lsp.rust]`).
pub(crate) const LSP_SCALAR_KEYS: &[&str] = &["max_concurrent_servers"];

/// Drop misspelled scalar keys from `[lsp]`, warning for each. Returns whether
/// any were removed. See the caller for why `deny_unknown_fields` can't do this.
fn strip_unknown_lsp_scalars(raw: &mut toml::value::Table, path: &Path) -> bool {
    let Some(toml::Value::Table(lsp)) = raw.get_mut("lsp") else {
        return false;
    };
    let bad: Vec<String> = lsp
        .iter()
        .filter(|(k, v)| !v.is_table() && !LSP_SCALAR_KEYS.contains(&k.as_str()))
        .map(|(k, _)| k.clone())
        .collect();
    for key in &bad {
        lsp.remove(key);
        // Ungated for the same reason as the unknown-field warning in
        // `read_config`: a dropped key is a setting silently not in effect.
        eprintln!(
            "cartog: warning: unknown key '{key}' in [lsp] of {} (ignored); \
             expected one of {}, or a per-language table like [lsp.rust]",
            path.display(),
            LSP_SCALAR_KEYS.join(", ")
        );
    }
    !bad.is_empty()
}

/// Load the local project config from `.cartog.toml`. See [`ConfigLoad`]
/// for the three possible outcomes; existing commands that don't care
/// about the rejected-vs-missing distinction can wrap this with
/// [`ConfigLoad::config_or_default`].
pub fn load_config() -> ConfigLoad {
    match local_config_path() {
        Some(p) => match read_config(&p) {
            Some(config) => ConfigLoad::Loaded { config, path: p },
            None => ConfigLoad::Rejected { path: p },
        },
        None => ConfigLoad::Missing,
    }
}

/// Path to the local project config: `.cartog.toml` found by walking up from
/// cwd to the git root. Returns `None` if no such file exists.
fn local_config_path() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".cartog.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        // Stop searching once we reach the git root without finding a config.
        if dir.join(".git").exists() {
            return None;
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Known top-level sections of `.cartog.toml`. Kept in sync with the fields of
/// [`CartogConfig`]. Unknown keys are warned about (non-fatal) so a typo like
/// `[embeddings]` is visible instead of silently ignored.
pub(crate) const KNOWN_CONFIG_SECTIONS: &[&str] = &[
    "database",
    "embedding",
    "reranker",
    "rag",
    "remote",
    "security",
    "lsp",
    "index",
];

/// Collect top-level keys that are not a recognized config section.
pub(crate) fn unknown_sections(raw: &toml::value::Table) -> Vec<&str> {
    raw.keys()
        .map(String::as_str)
        .filter(|k| !KNOWN_CONFIG_SECTIONS.contains(k))
        .collect()
}

/// Config-load diagnostics run before the tracing subscriber is initialised
/// (db-path resolution happens early in `main`), so they use `eprintln!`
/// rather than `tracing`. To avoid polluting the stderr of non-interactive
/// consumers — the MCP `serve` child, `--json` queries, CI pipes — they are
/// emitted only when stderr is a terminal (an interactive human is watching).
pub(crate) fn config_diagnostics_visible() -> bool {
    std::io::stderr().is_terminal()
}

/// Emit a one-line stderr warning for each unrecognized top-level config key.
fn warn_unknown_sections(raw: &toml::value::Table, path: &Path) {
    if !config_diagnostics_visible() {
        return;
    }
    for key in unknown_sections(raw) {
        eprintln!(
            "cartog: warning: unknown config key '{key}' in {} (ignored)",
            path.display()
        );
    }
}

pub(crate) fn read_config(path: &Path) -> Option<CartogConfig> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // NotFound is the only IO error we treat as "no config", silently.
        // The normal caller (`local_config_path`) only hands us paths that
        // exist; this branch covers races where the file disappears between
        // `exists()` and `read_to_string`, and unit tests that probe missing
        // paths directly. Permission denied, EIO, EACCES, etc. should be
        // loud — silently swallowing them turned into a "no remote
        // configured" downstream error with no hint that the user's file
        // was just unreadable.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("cartog: error reading {}: {e}", path.display());
            return None;
        }
    };

    // Security pre-check: scan the raw `[remote]` table for credential-shaped
    // keys before they have a chance to be deserialised or logged anywhere.
    // Also warn (non-fatal) about unknown top-level sections so a typo like
    // `[embeddings]` doesn't silently leave the user on defaults.
    let mut text = text;
    if let Ok(mut raw) = toml::from_str::<toml::value::Table>(&text) {
        if let Some(toml::Value::Table(remote)) = raw.get("remote") {
            if let Err(msg) = validate_remote_no_credentials(remote) {
                eprintln!("cartog: error in {}: {msg}", path.display());
                return None;
            }
        }
        warn_unknown_sections(&raw, path);
        // `[lsp]` can't use `deny_unknown_fields` (it's `#[serde(flatten)]`), so
        // a misspelled sibling key is parsed as a language name and fails with
        // "invalid type: integer, expected struct LspLangConfig" — naming
        // neither the key nor the section. Name it and drop it here instead.
        if strip_unknown_lsp_scalars(&mut raw, path) {
            if let Ok(cleaned) = toml::to_string(&raw) {
                text = cleaned;
            }
        }
    }

    let parsed = match toml::from_str::<CartogConfig>(&text) {
        Ok(cfg) => cfg,
        // An unknown key is a typo, not a reason to discard the file. Reporting
        // it is the point of `deny_unknown_fields`, but `Rejected` drops every
        // other setting — too much blast radius for one misspelling. Name the
        // key, then retry without it.
        Err(e) if is_unknown_field_error(&e) => {
            // Deliberately NOT TTY-gated, unlike the info-ish sibling
            // diagnostics: a dropped key means a setting the user wrote is not
            // in effect, which is warn-level. Gating it would reproduce the
            // silent-ignore bug this change exists to fix precisely where
            // cartog does most of its work (MCP, `--json`, CI). Mirrors
            // `main.rs`'s rule that captured stderr suppresses info, not warn.
            eprintln!("cartog: warning: in {}: {e}", path.display());
            // The reassurance is earned only by a salvage that actually worked:
            // printing it up front told users "the rest of the config still
            // applies" and then rejected the file anyway.
            match reparse_ignoring_unknown_keys(&text) {
                Some(cfg) => {
                    eprintln!(
                        "cartog: warning: ignoring that key; the rest of the config still applies."
                    );
                    cfg
                }
                None => {
                    eprintln!("cartog: warning: failed to parse {}", path.display());
                    return None;
                }
            }
        }
        Err(e) => {
            // Use eprintln rather than tracing — tracing may not be initialised yet.
            eprintln!("cartog: warning: failed to parse {}: {e}", path.display());
            return None;
        }
    };

    // Post-parse security check on `[remote].endpoint`. `parse_s3_url` already
    // refuses `s3://user:pass@bucket/key`, but `endpoint` accepts an arbitrary
    // URL — a value like `http://AKIA:secret@minio.local` would silently leak
    // credentials into the underlying S3 client's URL builder, bypassing the
    // "credentials only via AWS env chain" guarantee. Refuse explicitly.
    if let Some(remote) = parsed.remote.as_ref() {
        if let Err(msg) = validate_endpoint(remote.endpoint.as_deref()) {
            eprintln!("cartog: error in {}: {msg}", path.display());
            return None;
        }
    }

    // Reject an unknown `provider` value at parse time. Without this a typo
    // (`provider = "ollma"`) only surfaces later, when the provider is actually
    // loaded — and the reranker typo never surfaces at all. Fail fast here.
    if let Err(msg) = validate_providers(&parsed) {
        eprintln!("cartog: error in {}: {msg}", path.display());
        return None;
    }

    // Reject an empty `[lsp.<lang>] command` — an empty argv has no program to
    // spawn, and the failure would otherwise surface only at LSP start.
    if let Err(msg) = validate_lsp_overrides(&parsed) {
        eprintln!("cartog: error in {}: {msg}", path.display());
        return None;
    }

    // Reject a malformed `[index] exclude` glob at parse time, not first index.
    if let Err(msg) = to_walk_filter(&parsed) {
        eprintln!("cartog: error in {}: {msg}", path.display());
        return None;
    }

    Some(parsed)
}

/// Reject an invalid `[lsp.<lang>]` block: an empty `command` (no executable to
/// launch) or, when the `lsp` feature is built, an unknown language key (a typo
/// like `[lsp.pytho]`). Catching both at parse time turns confusing runtime
/// failures into clear config errors, mirroring [`validate_providers`].
fn validate_lsp_overrides(config: &CartogConfig) -> Result<(), String> {
    if let Some(lsp) = config.lsp.as_ref() {
        for (lang, cfg) in &lsp.langs {
            if cfg.command.is_empty() {
                return Err(format!(
                    "[lsp.{lang}] command is empty; provide at least the executable, \
                     e.g. command = [\"some-lsp\", \"--stdio\"]"
                ));
            }
            // Without the `lsp` feature the override is inert, so only the
            // known-language check is feature-gated; the empty check always runs.
            #[cfg(feature = "lsp")]
            if !cartog_lsp::servers::has_server_spec(lang) {
                return Err(format!(
                    "[lsp.{lang}] is not a recognized cartog language; \
                     overrides are keyed by language (rust, python, go, dart, ...)"
                ));
            }
        }
    }
    Ok(())
}

/// Flatten the `[lsp.<lang>]` config into the language → argv map consumed by
/// `cartog-lsp` / `cartog-indexer` / `cartog-mcp`. Returns an empty map when no
/// overrides are configured (the default: PATH-resolved servers).
#[must_use]
pub fn to_lsp_overrides(config: &CartogConfig) -> HashMap<String, Vec<String>> {
    config
        .lsp
        .as_ref()
        .map(|lsp| {
            lsp.langs
                .iter()
                .map(|(lang, cfg)| (lang.clone(), cfg.command.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Reject an unknown embedding/reranker `provider` value. Unknown values are a
/// user typo: surface them at config load rather than at first use. Absent
/// (`None`) means "use the default" and is always accepted.
pub(crate) fn validate_providers(config: &CartogConfig) -> Result<(), String> {
    const EMBEDDING_PROVIDERS: &[&str] = &["local", "ollama", "openai"];
    const RERANKER_PROVIDERS: &[&str] = &["local", "none"];

    if let Some(p) = config
        .embedding
        .as_ref()
        .and_then(|e| e.provider.as_deref())
    {
        if !EMBEDDING_PROVIDERS.contains(&p) {
            return Err(format!(
                "unknown embedding provider '{p}'; supported: {}",
                EMBEDDING_PROVIDERS.join(", ")
            ));
        }
    }
    if let Some(p) = config.reranker.as_ref().and_then(|r| r.provider.as_deref()) {
        if !RERANKER_PROVIDERS.contains(&p) {
            return Err(format!(
                "unknown reranker provider '{p}'; supported: {}",
                RERANKER_PROVIDERS.join(", ")
            ));
        }
    }
    Ok(())
}

/// Reject a `[remote].endpoint` value that embeds credentials via the
/// `user:pass@host` URL form. None / empty endpoint is fine — both mean
/// "fall back to the default AWS host".
fn validate_endpoint(endpoint: Option<&str>) -> Result<(), String> {
    let ep = match endpoint {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };

    // Trim the scheme so `s3://user@host` is detected too.
    let after_scheme = ep.split_once("://").map(|x| x.1).unwrap_or(ep);
    // Userinfo lives before the first `/` of the path and before any `?` or `#`.
    let authority = after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .split('?')
        .next()
        .unwrap_or(after_scheme)
        .split('#')
        .next()
        .unwrap_or(after_scheme);
    if authority.contains('@') {
        return Err(format!(
            "[remote].endpoint embeds credentials in its URL ({ep:?}) — cartog \
             does not accept credentials in config. Move them to the AWS \
             environment chain (AWS_ACCESS_KEY_ID / AWS_PROFILE / IMDS) and \
             use a plain endpoint URL."
        ));
    }
    Ok(())
}

/// Environment variable that opts a config-less project into indexing with
/// in-memory defaults. Set to any non-empty value to bypass the consent gate;
/// **no `.cartog.toml` is written** — only `cartog init` writes a config file.
pub const AUTO_INIT_ENV: &str = "CARTOG_AUTO_INIT";

/// True when an index/DB may be created for this project — i.e. the user has
/// opted in by at least one of three signals:
///
/// 1. a `.cartog.toml` is present ([`IndexConsent::Granted`], from
///    [`ConfigLoad::consent`]) — including one that failed to parse, since the
///    file's existence is the opt-in and its contents are a separate concern;
/// 2. the resolved main DB file already exists (Branch 1 — once an index
///    exists the project is de-facto opted in, and steady-state updates must
///    keep working). A stray `-wal`/`-shm` without the main file does NOT
///    count: the check is keyed on `db_path` itself;
/// 3. `CARTOG_AUTO_INIT` is set (indexes with defaults, writes no config).
///
/// When none hold, the write paths (`cartog index` / `rag index` / `watch`,
/// the MCP write tools, the watcher's first index) must refuse rather than
/// materialize a `.cartog/` for a project nobody opted into.
#[must_use]
pub fn allow_index_creation(db_path: &Path, consent: IndexConsent) -> bool {
    consent.is_granted() || db_path.exists() || auto_init_enabled()
}

/// Read `CARTOG_AUTO_INIT`: any non-empty value enables the bypass.
fn auto_init_enabled() -> bool {
    std::env::var(AUTO_INIT_ENV)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Resolve the database path using the following priority:
///
/// 1. `explicit` — from `--db` flag or `CARTOG_DB` env var (already merged by clap)
/// 2. `config.database.path` — from `.cartog.toml` at git root / cwd
/// 3. Auto git-root detection: prefer `<root>/.cartog/db.sqlite`, fall back to
///    legacy `<root>/.cartog.db` if only it exists (warns once, points at
///    `cartog self migrate-db`)
/// 4. cwd fallback — `.cartog/db.sqlite` in the current directory
pub fn resolve_db_path(explicit: Option<PathBuf>, config: &CartogConfig) -> PathBuf {
    // 1. Explicit override (--db / CARTOG_DB)
    if let Some(p) = explicit {
        return expand_tilde(p);
    }

    // 2. Local project config
    if let Some(path_str) = config.database.as_ref().and_then(|d| d.path.as_deref()) {
        return expand_tilde(PathBuf::from(path_str));
    }

    // 3. Walk up to git root
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            if dir.join(".git").exists() {
                return resolve_root_db_path(&dir);
            }
            if !dir.pop() {
                break;
            }
        }
    }

    // 4. Fallback relative to cwd
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_root_db_path(&cwd)
}

/// Prefer `.cartog/db.sqlite`; fall back to legacy `.cartog.db` with a warning.
fn resolve_root_db_path(root: &Path) -> PathBuf {
    let new_path = root.join(cartog_db::DB_DIR).join(cartog_db::DB_FILENAME);
    let legacy = root.join(cartog_db::LEGACY_DB_FILE);
    if new_path.exists() {
        if legacy.exists() {
            warn_orphan_legacy_once(&legacy);
        }
        return new_path;
    }
    if legacy.exists() {
        warn_legacy_db_once(&legacy);
        return legacy;
    }
    new_path
}

fn warn_legacy_db_once(path: &Path) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    // eprintln, not tracing: db-path resolution runs before the tracing
    // subscriber is initialised in main, so a `tracing::warn!` here is dropped.
    // TTY-gated so it doesn't pollute MCP serve / --json / piped stderr.
    if !config_diagnostics_visible() {
        return;
    }
    eprintln!(
        "cartog: using legacy database at {}; run `cartog self migrate-db` to move it into .cartog/",
        path.display()
    );
}

fn warn_orphan_legacy_once(path: &Path) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    if !config_diagnostics_visible() {
        return;
    }
    eprintln!(
        "cartog: found legacy database at {} alongside the new layout; the legacy file is ignored",
        path.display()
    );
}

/// Expand a leading `~/` to the user's home directory.
pub fn expand_tilde(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    p
}
