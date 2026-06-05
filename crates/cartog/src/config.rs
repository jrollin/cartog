use serde::Deserialize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Top-level cartog configuration, loaded from `.cartog.toml`.
///
/// Priority (highest to lowest):
/// 1. `--db` CLI flag / `CARTOG_DB` env var       (handled in main)
/// 2. `.cartog.toml` at git root or cwd           (`database.path`)
/// 3. Auto git-root detection: prefer `.cartog/db.sqlite`,
///    fall back to legacy `.cartog.db` if only it exists
/// 4. cwd fallback to `.cartog/db.sqlite`
#[derive(Debug, Default, Deserialize)]
pub struct CartogConfig {
    pub database: Option<DatabaseConfig>,
    pub embedding: Option<EmbeddingConfig>,
    pub reranker: Option<RerankerConfig>,
    pub rag: Option<RagConfig>,
    pub remote: Option<RemoteConfig>,
    pub security: Option<SecurityConfig>,
}

/// Secret-redaction settings.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SecurityConfig {
    /// Redact known secret patterns from stored symbol text. Default: true.
    /// The sensitive-file deny-list is always enforced regardless of this flag.
    pub redact_secrets: Option<bool>,
}

impl SecurityConfig {
    /// Whether secret redaction is enabled (default: true).
    #[must_use]
    pub fn redact_secrets(&self) -> bool {
        self.redact_secrets.unwrap_or(true)
    }
}

/// Optional S3-compatible remote for `cartog push` / `cartog pull`.
///
/// Credentials are resolved exclusively from the AWS environment chain (env
/// vars, profile, IMDS). Storing any credential-shaped key here (`access_key`,
/// `secret_key`, `credentials`, `token`, `aws_*`) is rejected at parse time —
/// see [`RemoteConfig::validate_no_credentials`].
///
/// ```toml
/// [remote]
/// url        = "s3://my-team-bucket/cartog/main"
/// region     = "us-east-1"
/// endpoint   = "https://minio.example.com"   # MinIO / R2 / floci
/// path_style = true                          # required for MinIO / floci
/// ```
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
// `region`, `endpoint`, and `path_style` are unused in minimal builds (no
// `remote-s3` feature). They still must be parsed and rejected on unknown
// keys, so the struct itself stays defined — but the fields would warn as
// unused. Silence that warning for the minimal build only.
#[cfg_attr(not(feature = "remote-s3"), allow(dead_code))]
pub struct RemoteConfig {
    /// Default `s3://bucket/key` target. `--remote` on push/pull overrides.
    pub url: Option<String>,
    /// AWS region (e.g. `us-east-1`). Optional when `endpoint` is set.
    pub region: Option<String>,
    /// Custom endpoint URL for S3-compatible stores (MinIO, R2, floci).
    pub endpoint: Option<String>,
    /// Force path-style addressing (required for most non-AWS endpoints).
    pub path_style: Option<bool>,
}

const CREDENTIAL_KEY_PREFIXES: &[&str] = &["aws_", "access_", "secret_"];
const CREDENTIAL_KEYS: &[&str] = &[
    "access_key",
    "secret_key",
    "credentials",
    "token",
    "session_token",
    "password",
];

/// Recursively inspect the raw `[remote]` table for credential-shaped keys.
///
/// `RemoteConfig` already has `deny_unknown_fields`, but that's coincidental
/// coverage that would silently regress the moment we accept a sub-table
/// (e.g. a future `[remote.headers]`). This walk is the real security
/// boundary: it traverses nested tables and arrays so `[remote.aws]
/// access_key = "..."` is rejected with the same actionable error as a
/// flat `[remote] access_key = "..."`.
///
/// Returned error contains a dotted path (e.g. `[remote].aws.access_key`)
/// so the user knows which line to delete.
fn validate_remote_no_credentials(table: &toml::value::Table) -> Result<(), String> {
    fn walk(prefix: &str, val: &toml::Value) -> Result<(), String> {
        match val {
            toml::Value::Table(t) => {
                for (k, v) in t {
                    let lower = k.to_lowercase();
                    if CREDENTIAL_KEYS.iter().any(|ck| lower == *ck)
                        || CREDENTIAL_KEY_PREFIXES.iter().any(|p| lower.starts_with(p))
                    {
                        return Err(format!(
                            "{prefix}.{k} looks like a credential — cartog does not read \
                             credentials from .cartog.toml. Use the AWS environment chain \
                             instead (AWS_ACCESS_KEY_ID / AWS_PROFILE / IMDS)."
                        ));
                    }
                    walk(&format!("{prefix}.{k}"), v)?;
                }
            }
            toml::Value::Array(arr) => {
                for (i, v) in arr.iter().enumerate() {
                    walk(&format!("{prefix}[{i}]"), v)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    walk("[remote]", &toml::Value::Table(table.clone()))
}

/// Tuning knobs for the hybrid search pipeline.
///
/// ```toml
/// [rag]
/// retrieval_multiplier = 3
/// retrieval_floor      = 20
/// rerank_max           = 50
/// rerank_min           = 8
/// ```
#[derive(Debug, Default, Clone, Deserialize)]
pub struct RagConfig {
    /// Over-retrieval multiplier for FTS5 + vector candidate pools.
    pub retrieval_multiplier: Option<u32>,
    /// Lower bound on candidate retrieval, independent of `limit`.
    pub retrieval_floor: Option<u32>,
    /// Cap on candidates passed to the cross-encoder.
    pub rerank_max: Option<u32>,
    /// Skip the cross-encoder entirely below this many candidates.
    pub rerank_min: Option<u32>,
}

impl RagConfig {
    /// Build a `SearchTuning` with caller-provided overrides applied on top
    /// of defaults, clamping invalid combinations so search never degrades
    /// to zero candidates or a silently-disabled reranker.
    pub fn to_search_tuning(&self) -> cartog_rag::search::SearchTuning {
        let d = cartog_rag::search::SearchTuning::default();
        // Multipliers and floors of 0 would collapse retrieval to nothing —
        // clamp to 1.
        let retrieval_multiplier = self
            .retrieval_multiplier
            .unwrap_or(d.retrieval_multiplier)
            .max(1);
        let retrieval_floor = self.retrieval_floor.unwrap_or(d.retrieval_floor).max(1);
        let rerank_max = self.rerank_max.unwrap_or(d.rerank_max);
        let rerank_min = self.rerank_min.unwrap_or(d.rerank_min);
        // If a user wrote `rerank_min > rerank_max`, the reranker would
        // silently never fire. Cap rerank_min at rerank_max.
        let rerank_min = rerank_min.min(rerank_max);
        cartog_rag::search::SearchTuning {
            retrieval_multiplier,
            retrieval_floor,
            rerank_max,
            rerank_min,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct DatabaseConfig {
    /// Filesystem path to the cartog SQLite database. Supports `~` expansion.
    pub path: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct EmbeddingConfig {
    /// Provider type: "local" (default), "ollama", or "openai".
    pub provider: Option<String>,
    /// Model name. For "local": fastembed built-in name or HuggingFace repo ID.
    /// For "ollama"/"openai": model name on the server.
    pub model: Option<String>,
    /// Embedding dimension. Auto-detected for built-in models, required for custom HF models.
    pub dimension: Option<usize>,
    /// Local provider settings (ONNX via fastembed).
    pub local: Option<LocalEmbeddingConfig>,
    /// Ollama provider settings.
    pub ollama: Option<OllamaConfig>,
    /// OpenAI-compatible provider settings.
    pub openai: Option<OpenAiConfig>,
    /// Auto-embed under `serve --watch` / `watch`. `None` = auto-detect (embed
    /// only if the repo already has embeddings); `Some(false)` = never;
    /// `Some(true)` = always. Precedence: `CARTOG_WATCH_RAG` env > this key >
    /// `--rag` flag (so this key overrides `--rag`, not the other way around).
    pub auto_embed: Option<bool>,
}

pub const DEFAULT_EMBEDDING_PROVIDER: &str = "local";

impl EmbeddingConfig {
    pub fn provider(&self) -> &str {
        self.provider
            .as_deref()
            .unwrap_or(DEFAULT_EMBEDDING_PROVIDER)
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct LocalEmbeddingConfig {
    /// Prefix prepended to text during search (e.g. "search_query: ").
    pub query_prefix: Option<String>,
    /// Prefix prepended to text during indexing (e.g. "search_document: ").
    pub document_prefix: Option<String>,
    /// Optional cap on ONNX intra-op threads for indexing/reranking. None =
    /// all cores (fastembed default). `CARTOG_ONNX_THREADS` overrides; read at
    /// provider load (restart `serve` to change it).
    pub intra_threads: Option<usize>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct OllamaConfig {
    /// Ollama server URL (default: "http://localhost:11434").
    pub base_url: Option<String>,
    /// Model name (default: "nomic-embed-text").
    pub model: Option<String>,
}

pub const DEFAULT_OLLAMA_BASE_URL: &str = cartog_rag::providers::DEFAULT_OLLAMA_BASE_URL;
pub const DEFAULT_OLLAMA_MODEL: &str = cartog_rag::providers::DEFAULT_OLLAMA_MODEL;

impl OllamaConfig {
    pub fn base_url(&self) -> &str {
        self.base_url.as_deref().unwrap_or(DEFAULT_OLLAMA_BASE_URL)
    }

    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or(DEFAULT_OLLAMA_MODEL)
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct OpenAiConfig {
    /// OpenAI-compatible base URL (default: "https://api.openai.com/v1").
    pub base_url: Option<String>,
    /// Model name (default: "text-embedding-3-small").
    pub model: Option<String>,
    /// Env var NAME holding the API key (default: "OPENAI_API_KEY"). The key
    /// value is never stored in config; unset means a keyless local endpoint.
    pub api_key_env: Option<String>,
}

pub const DEFAULT_OPENAI_BASE_URL: &str = cartog_rag::providers::DEFAULT_OPENAI_BASE_URL;
pub const DEFAULT_OPENAI_MODEL: &str = cartog_rag::providers::DEFAULT_OPENAI_MODEL;
pub const DEFAULT_OPENAI_API_KEY_ENV: &str = cartog_rag::providers::DEFAULT_OPENAI_API_KEY_ENV;

impl OpenAiConfig {
    /// Endpoint base URL, or [`DEFAULT_OPENAI_BASE_URL`] when unset.
    pub fn base_url(&self) -> &str {
        self.base_url.as_deref().unwrap_or(DEFAULT_OPENAI_BASE_URL)
    }

    /// Embedding model name, or [`DEFAULT_OPENAI_MODEL`] when unset.
    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or(DEFAULT_OPENAI_MODEL)
    }

    /// Env var name holding the API key, or [`DEFAULT_OPENAI_API_KEY_ENV`] when unset.
    pub fn api_key_env(&self) -> &str {
        self.api_key_env
            .as_deref()
            .unwrap_or(DEFAULT_OPENAI_API_KEY_ENV)
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct RerankerConfig {
    /// Provider type: "local" (default) or "none".
    pub provider: Option<String>,
    /// Reranker model as a fastembed HF repo path (e.g. `BAAI/bge-reranker-base`).
    /// None = [`cartog_rag::DEFAULT_RERANKER_MODEL`]. Mirrors `[embedding] model`.
    pub model: Option<String>,
}

pub const DEFAULT_RERANKER_PROVIDER: &str = "local";

impl RerankerConfig {
    pub fn provider(&self) -> &str {
        self.provider
            .as_deref()
            .unwrap_or(DEFAULT_RERANKER_PROVIDER)
    }
}

/// Resolve whether the watcher should auto-embed, as an explicit override.
///
/// Precedence: `CARTOG_WATCH_RAG` env (`0`/`false`/`1`/`true`) > `[embedding]
/// auto_embed` > `--rag` flag. Returns `None` when no explicit signal is set,
/// leaving the watcher to decide from the live `embedding_count` (auto-detect).
pub fn resolve_auto_embed(cli_rag: bool, config: &CartogConfig) -> Option<bool> {
    resolve_auto_embed_with(
        std::env::var("CARTOG_WATCH_RAG").ok().as_deref(),
        config.embedding.as_ref().and_then(|e| e.auto_embed),
        cli_rag,
    )
}

/// Pure resolver for [`resolve_auto_embed`], split out so tests don't mutate the
/// process environment. An unparseable env value falls through to the next tier.
fn resolve_auto_embed_with(env: Option<&str>, cfg: Option<bool>, cli_rag: bool) -> Option<bool> {
    if let Some(v) = env.and_then(parse_bool_flag) {
        return Some(v);
    }
    cfg.or_else(|| cli_rag.then_some(true))
}

/// Parse a permissive boolean env value; `None` if unrecognized.
fn parse_bool_flag(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Build the indexer redaction policy from the `[security]` section.
pub fn to_redaction_config(config: &CartogConfig) -> cartog_indexer::RedactionConfig {
    let enabled = config
        .security
        .as_ref()
        .map_or(true, SecurityConfig::redact_secrets);
    cartog_indexer::RedactionConfig { enabled }
}

/// Convert the embedding config section into an `EmbeddingProviderConfig` for cartog-rag.
pub fn to_provider_config(config: &CartogConfig) -> cartog_rag::EmbeddingProviderConfig {
    // The reranker section is independent of the embedding section — honor it even
    // when `[embedding]` is absent (otherwise a lone `[reranker]` block is dropped).
    let reranker_provider = config
        .reranker
        .as_ref()
        .map(|r| r.provider().to_string())
        .unwrap_or_else(|| DEFAULT_RERANKER_PROVIDER.to_string());
    let reranker_model = config.reranker.as_ref().and_then(|r| r.model.clone());

    match &config.embedding {
        Some(embed) => {
            let (query_prefix, document_prefix, intra_threads) = match &embed.local {
                Some(local) => (
                    local.query_prefix.clone(),
                    local.document_prefix.clone(),
                    local.intra_threads,
                ),
                None => (None, None, None),
            };
            // Resolve base_url/model/api_key_env from the sub-table matching the
            // active provider — not whichever sub-table happens to be present, or a
            // lingering [embedding.ollama] block would override an openai config.
            let (sub_base_url, sub_model, api_key_env) = match embed.provider() {
                "ollama" => (
                    embed.ollama.as_ref().map(|o| o.base_url().to_string()),
                    embed.ollama.as_ref().map(|o| o.model().to_string()),
                    None,
                ),
                "openai" => (
                    embed.openai.as_ref().map(|o| o.base_url().to_string()),
                    embed.openai.as_ref().map(|o| o.model().to_string()),
                    embed.openai.as_ref().map(|o| o.api_key_env().to_string()),
                ),
                _ => (None, None, None),
            };
            cartog_rag::EmbeddingProviderConfig {
                provider: embed.provider().to_string(),
                model: embed.model.clone().or(sub_model),
                dimension: embed.dimension,
                query_prefix,
                document_prefix,
                base_url: sub_base_url,
                api_key_env,
                reranker_provider,
                reranker_model,
                intra_threads,
            }
        }
        None => cartog_rag::EmbeddingProviderConfig {
            reranker_provider,
            reranker_model,
            ..Default::default()
        },
    }
}

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
const KNOWN_CONFIG_SECTIONS: &[&str] = &[
    "database",
    "embedding",
    "reranker",
    "rag",
    "remote",
    "security",
];

/// Collect top-level keys that are not a recognized config section.
fn unknown_sections(raw: &toml::value::Table) -> Vec<&str> {
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
fn config_diagnostics_visible() -> bool {
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

fn read_config(path: &Path) -> Option<CartogConfig> {
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
    if let Ok(raw) = toml::from_str::<toml::value::Table>(&text) {
        if let Some(toml::Value::Table(remote)) = raw.get("remote") {
            if let Err(msg) = validate_remote_no_credentials(remote) {
                eprintln!("cartog: error in {}: {msg}", path.display());
                return None;
            }
        }
        warn_unknown_sections(&raw, path);
    }

    let parsed = match toml::from_str::<CartogConfig>(&text) {
        Ok(cfg) => cfg,
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

    Some(parsed)
}

/// Reject an unknown embedding/reranker `provider` value. Unknown values are a
/// user typo: surface them at config load rather than at first use. Absent
/// (`None`) means "use the default" and is always accepted.
fn validate_providers(config: &CartogConfig) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    #[test]
    fn test_expand_tilde_with_home() {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".into());
        let expanded = expand_tilde(PathBuf::from("~/foo/bar"));
        assert_eq!(expanded, PathBuf::from(home).join("foo/bar"));
    }

    #[test]
    fn unknown_sections_flags_typos_but_not_known_keys() {
        let raw: toml::value::Table =
            toml::from_str("[embeddings]\nprovider = \"ollama\"\n[database]\npath = \"x\"\n")
                .unwrap();
        let unknown = unknown_sections(&raw);
        assert_eq!(unknown, vec!["embeddings"]);
    }

    #[test]
    fn unknown_sections_empty_for_all_known() {
        let raw: toml::value::Table =
            toml::from_str("[database]\npath = \"x\"\n[embedding]\nprovider = \"local\"\n")
                .unwrap();
        assert!(unknown_sections(&raw).is_empty());
    }

    #[test]
    fn validate_providers_accepts_known_values() {
        let config: CartogConfig =
            toml::from_str("[embedding]\nprovider = \"ollama\"\n[reranker]\nprovider = \"none\"\n")
                .unwrap();
        assert!(validate_providers(&config).is_ok());
    }

    #[test]
    fn validate_providers_accepts_absent_provider() {
        let config = CartogConfig::default();
        assert!(validate_providers(&config).is_ok());
    }

    #[test]
    fn validate_providers_rejects_unknown_embedding_provider() {
        let config: CartogConfig = toml::from_str("[embedding]\nprovider = \"ollma\"\n").unwrap();
        let err = validate_providers(&config).unwrap_err();
        assert!(
            err.contains("ollma"),
            "error should name the bad value: {err}"
        );
    }

    #[test]
    fn validate_providers_rejects_unknown_reranker_provider() {
        let config: CartogConfig = toml::from_str("[reranker]\nprovider = \"bogus\"\n").unwrap();
        let err = validate_providers(&config).unwrap_err();
        assert!(
            err.contains("bogus"),
            "error should name the bad value: {err}"
        );
    }

    #[test]
    fn read_config_rejects_unknown_provider() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("config.toml");
        fs::write(&cfg_path, "[embedding]\nprovider = \"ollma\"\n").unwrap();
        assert!(read_config(&cfg_path).is_none());
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        let p = PathBuf::from("/absolute/path");
        assert_eq!(expand_tilde(p.clone()), p);
    }

    #[test]
    fn test_read_config_valid_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("config.toml");
        fs::write(&cfg_path, "[database]\npath = \"/tmp/test.db\"\n").unwrap();
        let cfg = read_config(&cfg_path).expect("should parse");
        assert_eq!(
            cfg.database.as_ref().unwrap().path.as_deref(),
            Some("/tmp/test.db")
        );
    }

    #[test]
    fn redact_secrets_defaults_true_when_absent() {
        let cfg = CartogConfig::default();
        assert!(to_redaction_config(&cfg).enabled);
    }

    #[test]
    fn redact_secrets_can_be_disabled_via_config() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("config.toml");
        fs::write(&cfg_path, "[security]\nredact_secrets = false\n").unwrap();
        let cfg = read_config(&cfg_path).expect("should parse");
        assert!(!to_redaction_config(&cfg).enabled);
    }

    #[test]
    fn test_read_config_missing_file_returns_none() {
        let result = read_config(Path::new("/nonexistent/path/config.toml"));
        assert!(result.is_none());
    }

    #[test]
    fn test_read_config_invalid_toml_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("config.toml");
        fs::write(&cfg_path, "this is {{ not valid toml").unwrap();
        assert!(read_config(&cfg_path).is_none());
    }

    #[test]
    fn test_read_config_empty_toml_returns_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("config.toml");
        fs::write(&cfg_path, "").unwrap();
        let cfg = read_config(&cfg_path).expect("empty toml is valid");
        assert!(cfg.database.is_none());
    }

    #[test]
    fn test_remote_config_valid_minimal() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        fs::write(
            &cfg_path,
            r#"[remote]
url = "s3://team-bucket/cartog/main"
region = "us-east-1"
"#,
        )
        .unwrap();
        let cfg = read_config(&cfg_path).expect("should parse");
        let remote = cfg.remote.expect("remote section parsed");
        assert_eq!(remote.url.as_deref(), Some("s3://team-bucket/cartog/main"));
        assert_eq!(remote.region.as_deref(), Some("us-east-1"));
        assert_eq!(remote.path_style, None);
    }

    #[test]
    fn test_remote_config_full_minio_shape() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        fs::write(
            &cfg_path,
            r#"[remote]
url = "s3://b/k"
region = "us-east-1"
endpoint = "https://minio.local"
path_style = true
"#,
        )
        .unwrap();
        let cfg = read_config(&cfg_path).expect("should parse");
        let r = cfg.remote.unwrap();
        assert_eq!(r.endpoint.as_deref(), Some("https://minio.local"));
        assert_eq!(r.path_style, Some(true));
    }

    /// `deny_unknown_fields` plus the credential pre-check together guarantee
    /// no rogue key sneaks through. This test exercises each named credential
    /// key and the `aws_*` prefix.
    #[test]
    fn test_remote_config_rejects_credential_keys() {
        for bad in [
            "access_key = \"AKIA...\"",
            "secret_key = \"...\"",
            "credentials = \"...\"",
            "token = \"...\"",
            "session_token = \"...\"",
            "password = \"...\"",
            "aws_access_key_id = \"...\"",
            "AWS_SECRET = \"...\"",
        ] {
            let dir = tempfile::TempDir::new().unwrap();
            let cfg_path = dir.path().join(".cartog.toml");
            fs::write(&cfg_path, format!("[remote]\nurl = \"s3://b/k\"\n{bad}\n")).unwrap();
            assert!(
                read_config(&cfg_path).is_none(),
                "should reject credential key: {bad}"
            );
        }
    }

    /// Credentials hidden one level deeper in `[remote.aws]` / `[remote.creds]`
    /// must not slip past the security pre-check. `deny_unknown_fields` would
    /// already reject the nested table itself, but the user-visible error
    /// would say "unknown field `aws`" rather than the security-specific
    /// message — which is the whole point of having this pre-check.
    #[test]
    fn test_remote_config_rejects_nested_credential_keys() {
        for bad_section in [
            "[remote.aws]\naccess_key = \"AKIA...\"\n",
            "[remote.creds]\nsecret_key = \"...\"\n",
            "[remote.minio]\naws_session_token = \"...\"\n",
        ] {
            let dir = tempfile::TempDir::new().unwrap();
            let cfg_path = dir.path().join(".cartog.toml");
            fs::write(
                &cfg_path,
                format!("[remote]\nurl = \"s3://b/k\"\n{bad_section}"),
            )
            .unwrap();
            assert!(
                read_config(&cfg_path).is_none(),
                "should reject nested credential: {bad_section}"
            );
        }
    }

    /// The prefix list (`aws_`, `access_`, `secret_`) is the second arm of the
    /// detector and only had implicit coverage before this test (the named
    /// keys mostly happen to match prefixes too). Exercise it directly so a
    /// future refactor that loses the prefix arm fails loudly.
    #[test]
    fn test_remote_config_rejects_credential_prefixes() {
        for bad in [
            // Names not in CREDENTIAL_KEYS but caught by prefix.
            "access_token_v2 = \"...\"",
            "secret_value = \"...\"",
            "aws_role_arn = \"...\"",
        ] {
            let dir = tempfile::TempDir::new().unwrap();
            let cfg_path = dir.path().join(".cartog.toml");
            fs::write(&cfg_path, format!("[remote]\nurl = \"s3://b/k\"\n{bad}\n")).unwrap();
            assert!(
                read_config(&cfg_path).is_none(),
                "should reject prefix-matched credential key: {bad}"
            );
        }
    }

    /// `deny_unknown_fields` should also reject anything else that's not a
    /// known field — this is a forward-compatibility guard so typos like
    /// `pathstyle` fail loudly instead of silently doing nothing.
    #[test]
    fn test_remote_config_rejects_unknown_field() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        fs::write(
            &cfg_path,
            "[remote]\nurl = \"s3://b/k\"\npathstyle = true\n",
        )
        .unwrap();
        assert!(read_config(&cfg_path).is_none());
    }

    /// `[remote].endpoint` must not embed credentials via the `user:pass@host`
    /// URL form. `parse_s3_url` rejects this for `url`; the symmetric check
    /// on `endpoint` closes the matching gap.
    #[test]
    fn test_remote_config_rejects_endpoint_with_userinfo() {
        for bad in [
            "http://AKIA:secret@minio.local",
            "https://user@s3.example.com",
            // No scheme — still parseable as userinfo + host.
            "AKIA:secret@host:9000",
            "https://user:pass@host/path",
        ] {
            let dir = tempfile::TempDir::new().unwrap();
            let cfg_path = dir.path().join(".cartog.toml");
            fs::write(
                &cfg_path,
                format!("[remote]\nurl = \"s3://b/k\"\nendpoint = \"{bad}\"\n"),
            )
            .unwrap();
            assert!(
                read_config(&cfg_path).is_none(),
                "should reject endpoint with userinfo: {bad}"
            );
        }
    }

    /// Common legitimate endpoint shapes must still parse. Belt-and-braces
    /// guard against an overly-aggressive `validate_endpoint` regex.
    #[test]
    fn test_remote_config_accepts_clean_endpoints() {
        for ok in [
            "https://s3.us-east-1.amazonaws.com",
            "https://minio.example.com:9000",
            "https://r2.cloudflarestorage.com/path",
            "http://localhost:4566",
        ] {
            let dir = tempfile::TempDir::new().unwrap();
            let cfg_path = dir.path().join(".cartog.toml");
            fs::write(
                &cfg_path,
                format!("[remote]\nurl = \"s3://b/k\"\nendpoint = \"{ok}\"\n"),
            )
            .unwrap();
            assert!(
                read_config(&cfg_path).is_some(),
                "should accept clean endpoint: {ok}"
            );
        }
    }

    #[test]
    fn test_resolve_explicit_wins_over_config() {
        let cfg = CartogConfig {
            database: Some(DatabaseConfig {
                path: Some("/config/path.db".to_string()),
            }),
            ..Default::default()
        };
        let result = resolve_db_path(Some(PathBuf::from("/explicit/path.db")), &cfg);
        assert_eq!(result, PathBuf::from("/explicit/path.db"));
    }

    #[test]
    fn test_resolve_config_path_used_when_no_explicit() {
        let cfg = CartogConfig {
            database: Some(DatabaseConfig {
                path: Some("/config/proj.db".to_string()),
            }),
            ..Default::default()
        };
        let result = resolve_db_path(None, &cfg);
        assert_eq!(result, PathBuf::from("/config/proj.db"));
    }

    #[test]
    #[serial]
    fn test_resolve_fallback_when_no_config_and_no_git() {
        let dir = tempfile::TempDir::new().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let result = resolve_db_path(None, &CartogConfig::default());
        std::env::set_current_dir(original).unwrap();

        assert_eq!(
            result,
            canonical
                .join(cartog_db::DB_DIR)
                .join(cartog_db::DB_FILENAME)
        );
    }

    #[test]
    #[serial]
    fn test_resolve_git_root_detection() {
        let dir = tempfile::TempDir::new().unwrap();
        let canonical_root = dir.path().canonicalize().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&subdir).unwrap();

        let result = resolve_db_path(None, &CartogConfig::default());
        std::env::set_current_dir(original).unwrap();

        assert_eq!(
            result,
            canonical_root
                .join(cartog_db::DB_DIR)
                .join(cartog_db::DB_FILENAME)
        );
    }

    #[test]
    #[serial]
    fn test_resolve_prefers_new_layout_over_legacy() {
        let dir = tempfile::TempDir::new().unwrap();
        let canonical_root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        // Both files exist — new layout wins.
        std::fs::create_dir(dir.path().join(cartog_db::DB_DIR)).unwrap();
        std::fs::write(
            dir.path()
                .join(cartog_db::DB_DIR)
                .join(cartog_db::DB_FILENAME),
            b"",
        )
        .unwrap();
        std::fs::write(dir.path().join(cartog_db::LEGACY_DB_FILE), b"").unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = resolve_db_path(None, &CartogConfig::default());
        std::env::set_current_dir(original).unwrap();

        assert_eq!(
            result,
            canonical_root
                .join(cartog_db::DB_DIR)
                .join(cartog_db::DB_FILENAME)
        );
    }

    #[test]
    #[serial]
    fn test_resolve_falls_back_to_legacy_db_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let canonical_root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        // Only legacy file exists — picks it up (and warns once).
        std::fs::write(dir.path().join(cartog_db::LEGACY_DB_FILE), b"").unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = resolve_db_path(None, &CartogConfig::default());
        std::env::set_current_dir(original).unwrap();

        assert_eq!(result, canonical_root.join(cartog_db::LEGACY_DB_FILE));
    }

    // ── Embedding config tests ──

    #[test]
    fn test_embedding_config_defaults() {
        let cfg = EmbeddingConfig::default();
        assert_eq!(cfg.provider(), "local");
        assert!(cfg.dimension.is_none());
        assert!(cfg.model.is_none());
        assert!(cfg.local.is_none());
        assert!(cfg.ollama.is_none());
    }

    #[test]
    fn test_embedding_config_from_toml() {
        let toml_str = r#"
[embedding]
provider = "ollama"
model = "nomic-embed-text"
dimension = 768
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let embed = cfg.embedding.unwrap();
        assert_eq!(embed.provider(), "ollama");
        assert_eq!(embed.model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(embed.dimension, Some(768));
    }

    #[test]
    fn test_embedding_config_local_with_prefixes() {
        let toml_str = r#"
[embedding]
provider = "local"
model = "BAAI/bge-small-en-v1.5"

[embedding.local]
query_prefix = "search_query: "
document_prefix = "search_document: "
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let embed = cfg.embedding.unwrap();
        assert_eq!(embed.provider(), "local");
        let local = embed.local.unwrap();
        assert_eq!(local.query_prefix.as_deref(), Some("search_query: "));
        assert_eq!(local.document_prefix.as_deref(), Some("search_document: "));
    }

    #[test]
    fn test_ollama_config_defaults() {
        let cfg = OllamaConfig::default();
        assert_eq!(cfg.base_url(), "http://localhost:11434");
        assert_eq!(cfg.model(), "nomic-embed-text");
    }

    #[test]
    fn test_ollama_config_from_toml() {
        let toml_str = r#"
[embedding.ollama]
base_url = "http://gpu-server:11434"
model = "mxbai-embed-large"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let ollama = cfg.embedding.unwrap().ollama.unwrap();
        assert_eq!(ollama.base_url(), "http://gpu-server:11434");
        assert_eq!(ollama.model(), "mxbai-embed-large");
    }

    #[test]
    fn test_openai_config_defaults() {
        let cfg = OpenAiConfig::default();
        assert_eq!(cfg.base_url(), "https://api.openai.com/v1");
        assert_eq!(cfg.model(), "text-embedding-3-small");
        assert_eq!(cfg.api_key_env(), "OPENAI_API_KEY");
    }

    #[test]
    fn test_openai_config_from_toml() {
        let toml_str = r#"
[embedding.openai]
base_url = "http://localhost:11434/v1"
model = "nomic-embed-text"
api_key_env = "MY_OPENAI_KEY"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let openai = cfg.embedding.unwrap().openai.unwrap();
        assert_eq!(openai.base_url(), "http://localhost:11434/v1");
        assert_eq!(openai.model(), "nomic-embed-text");
        assert_eq!(openai.api_key_env(), "MY_OPENAI_KEY");
    }

    #[test]
    fn validate_providers_accepts_openai() {
        let config: CartogConfig = toml::from_str("[embedding]\nprovider = \"openai\"\n").unwrap();
        assert!(validate_providers(&config).is_ok());
    }

    #[test]
    fn test_to_provider_config_threads_openai_settings() {
        let toml_str = r#"
[embedding]
provider = "openai"

[embedding.openai]
base_url = "https://api.mistral.ai/v1"
model = "mistral-embed"
api_key_env = "MISTRAL_API_KEY"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.provider, "openai");
        assert_eq!(pc.model.as_deref(), Some("mistral-embed"));
        assert_eq!(pc.base_url.as_deref(), Some("https://api.mistral.ai/v1"));
        assert_eq!(pc.api_key_env.as_deref(), Some("MISTRAL_API_KEY"));
    }

    #[test]
    fn test_to_provider_config_openai_ignores_lingering_ollama_block() {
        // A leftover [embedding.ollama] block must not override an openai config:
        // resolution keys off the active provider, not sub-table presence.
        let toml_str = r#"
[embedding]
provider = "openai"

[embedding.openai]
base_url = "https://api.openai.com/v1"
model = "text-embedding-3-small"

[embedding.ollama]
base_url = "http://localhost:11434"
model = "nomic-embed-text"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.provider, "openai");
        assert_eq!(pc.base_url.as_deref(), Some("https://api.openai.com/v1"));
        assert_eq!(pc.model.as_deref(), Some("text-embedding-3-small"));
    }

    #[test]
    fn test_to_provider_config_ollama_ignores_lingering_openai_block() {
        let toml_str = r#"
[embedding]
provider = "ollama"

[embedding.ollama]
base_url = "http://gpu:11434"

[embedding.openai]
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.provider, "ollama");
        assert_eq!(pc.base_url.as_deref(), Some("http://gpu:11434"));
        // api_key_env belongs to openai only — never leaks into an ollama config.
        assert!(pc.api_key_env.is_none());
    }

    #[test]
    fn test_reranker_config_defaults() {
        let cfg = RerankerConfig::default();
        assert_eq!(cfg.provider(), "local");
    }

    #[test]
    fn test_reranker_config_none() {
        let toml_str = r#"
[reranker]
provider = "none"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.reranker.unwrap().provider(), "none");
    }

    #[test]
    fn test_reranker_model_defaults_to_none_in_config() {
        let cfg = RerankerConfig::default();
        assert!(cfg.model.is_none());
    }

    #[test]
    fn test_to_provider_config_reranker_model_from_toml() {
        let toml_str = r#"
[reranker]
model = "BAAI/bge-reranker-base"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.reranker_model.as_deref(), Some("BAAI/bge-reranker-base"));
    }

    #[test]
    fn test_to_provider_config_reranker_model_default_is_none() {
        // Unset model maps to None; cartog-rag resolves None to DEFAULT_RERANKER_MODEL.
        let cfg = CartogConfig::default();
        let pc = to_provider_config(&cfg);
        assert!(pc.reranker_model.is_none());
    }

    #[test]
    fn test_reranker_provider_none_keeps_model_inert() {
        // A model alongside provider="none" parses fine; the model is simply never loaded.
        let toml_str = r#"
[reranker]
provider = "none"
model = "BAAI/bge-reranker-base"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.reranker_provider, "none");
        assert_eq!(pc.reranker_model.as_deref(), Some("BAAI/bge-reranker-base"));
    }

    #[test]
    fn test_full_config_backward_compat() {
        let toml_str = r#"
[database]
path = "/tmp/test.db"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.embedding.is_none());
        assert!(cfg.reranker.is_none());
        assert_eq!(cfg.database.unwrap().path.as_deref(), Some("/tmp/test.db"));
    }

    #[test]
    fn test_config_unknown_fields_ignored() {
        let toml_str = r#"
[embedding]
provider = "local"
unknown_field = "should be ignored"
"#;
        // serde default: unknown fields are silently ignored
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.embedding.unwrap().provider(), "local");
    }

    // ── to_provider_config tests ──

    #[test]
    fn test_to_provider_config_defaults() {
        let cfg = CartogConfig::default();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.provider, "local");
        assert!(pc.model.is_none());
        assert_eq!(pc.resolved_dimension(), 384);
        assert!(pc.query_prefix.is_none());
        assert!(pc.document_prefix.is_none());
    }

    #[test]
    fn test_to_provider_config_from_toml() {
        let toml_str = r#"
[embedding]
provider = "ollama"
model = "nomic-embed-text"
dimension = 768

[embedding.local]
query_prefix = "search_query: "
document_prefix = "search_document: "
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.provider, "ollama");
        assert_eq!(pc.model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(pc.resolved_dimension(), 768);
        assert_eq!(pc.query_prefix.as_deref(), Some("search_query: "));
        assert_eq!(pc.document_prefix.as_deref(), Some("search_document: "));
    }

    #[test]
    fn test_provider_config_dimension_override() {
        let pc = cartog_rag::EmbeddingProviderConfig {
            dimension: Some(1536),
            ..Default::default()
        };
        assert_eq!(pc.resolved_dimension(), 1536);
    }

    #[test]
    fn test_provider_config_dimension_default_fallback() {
        let pc = cartog_rag::EmbeddingProviderConfig::default();
        assert_eq!(pc.resolved_dimension(), 384);
        assert!(pc.dimension.is_none());
    }

    #[test]
    fn test_to_provider_config_ollama_model_fallback() {
        let toml_str = r#"
[embedding]
provider = "ollama"

[embedding.ollama]
model = "mxbai-embed-large"
base_url = "http://gpu:11434"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.provider, "ollama");
        assert_eq!(pc.model.as_deref(), Some("mxbai-embed-large"));
        assert_eq!(pc.base_url.as_deref(), Some("http://gpu:11434"));
    }

    #[test]
    fn test_to_provider_config_top_level_model_wins() {
        let toml_str = r#"
[embedding]
provider = "ollama"
model = "top-level-model"

[embedding.ollama]
model = "ollama-model"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.model.as_deref(), Some("top-level-model"),);
    }

    #[test]
    fn test_to_provider_config_base_url_threaded() {
        let toml_str = r#"
[embedding]
provider = "ollama"

[embedding.ollama]
base_url = "http://custom:11434"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert_eq!(pc.base_url.as_deref(), Some("http://custom:11434"));
    }

    #[test]
    fn test_to_provider_config_no_base_url_when_local() {
        let toml_str = r#"
[embedding]
provider = "local"
"#;
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        let pc = to_provider_config(&cfg);
        assert!(pc.base_url.is_none());
    }

    // ── RagConfig ──────────────────────────────────────────────────────

    #[test]
    fn to_search_tuning_clamps_zero_retrieval() {
        let cfg = RagConfig {
            retrieval_multiplier: Some(0),
            retrieval_floor: Some(0),
            rerank_max: None,
            rerank_min: None,
        };
        let t = cfg.to_search_tuning();
        assert_eq!(t.retrieval_multiplier, 1);
        assert_eq!(t.retrieval_floor, 1);
    }

    #[test]
    fn to_search_tuning_caps_rerank_min_at_max() {
        let cfg = RagConfig {
            retrieval_multiplier: None,
            retrieval_floor: None,
            rerank_max: Some(10),
            rerank_min: Some(50),
        };
        let t = cfg.to_search_tuning();
        assert_eq!(t.rerank_max, 10);
        assert_eq!(t.rerank_min, 10);
    }

    #[test]
    fn to_search_tuning_passes_valid_values() {
        let cfg = RagConfig {
            retrieval_multiplier: Some(5),
            retrieval_floor: Some(40),
            rerank_max: Some(100),
            rerank_min: Some(10),
        };
        let t = cfg.to_search_tuning();
        assert_eq!(t.retrieval_multiplier, 5);
        assert_eq!(t.retrieval_floor, 40);
        assert_eq!(t.rerank_max, 100);
        assert_eq!(t.rerank_min, 10);
    }

    #[test]
    fn auto_embed_env_beats_config_and_flag() {
        assert_eq!(
            resolve_auto_embed_with(Some("0"), Some(true), true),
            Some(false)
        );
        assert_eq!(
            resolve_auto_embed_with(Some("on"), Some(false), false),
            Some(true)
        );
    }

    #[test]
    fn auto_embed_config_beats_flag() {
        assert_eq!(
            resolve_auto_embed_with(None, Some(false), true),
            Some(false)
        );
        assert_eq!(resolve_auto_embed_with(None, Some(true), false), Some(true));
    }

    #[test]
    fn auto_embed_flag_only_when_no_other_signal() {
        assert_eq!(resolve_auto_embed_with(None, None, true), Some(true));
    }

    #[test]
    fn auto_embed_none_when_no_signal_defers_to_watcher() {
        assert_eq!(resolve_auto_embed_with(None, None, false), None);
        // Unparseable env falls through to the next tier.
        assert_eq!(resolve_auto_embed_with(Some("maybe"), None, false), None);
    }
}
