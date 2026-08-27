//! Configuration data types deserialized from `.cartog.toml`.

use serde::Deserialize;
use std::collections::HashMap;

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
    /// LSP settings: per-language command overrides plus `max_concurrent_servers`.
    pub lsp: Option<LspConfig>,
    pub index: Option<IndexConfig>,
}

/// `[lsp]` section: per-language command overrides (flattened as `[lsp.<lang>]`)
/// plus a sibling `max_concurrent_servers` cap. No `deny_unknown_fields` here —
/// it is incompatible with `#[serde(flatten)]`; the inner `LspLangConfig` keeps it.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct LspConfig {
    /// Per-language `[lsp.<lang>] command = [...]` overrides.
    #[serde(default, flatten)]
    pub langs: HashMap<String, LspLangConfig>,
    /// Max LSP server processes run concurrently during the indexer's edge pass.
    /// Absent / `0` = auto (`min(languages_in_pass, 4)`). `CARTOG_LSP_MAX_SERVERS`
    /// overrides. Each server is RAM-heavy (rust-analyzer ~1-2GB).
    pub max_concurrent_servers: Option<usize>,
}

/// Override for one language's LSP server command (`[lsp.<lang>]`).
///
/// Runs an arbitrary command as the language server instead of the built-in
/// PATH lookup — e.g. a Dockerized server so cartog needs no native toolchain.
/// `${ROOT}` in any element expands to the indexed project root (host-absolute),
/// so a container can mount the repo at the same path cartog uses for `file://`
/// URIs: `-v ${ROOT}:${ROOT} -w ${ROOT}`.
///
/// ```toml
/// [lsp.dart]
/// command = ["docker", "run", "--rm", "-i",
///            "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-dart:stable"]
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LspLangConfig {
    /// argv to launch the server; `argv[0]` is the program, the rest are args.
    pub command: Vec<String>,
}

/// Secret-redaction settings.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Indexing settings.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexConfig {
    /// Repo-root-relative globs whose matching files and directories are skipped
    /// during indexing (e.g. `vendor/**`, `**/*.generated.*`). Complements the
    /// built-in dep/gen-dir prune list; matched directories are not descended.
    pub exclude: Option<Vec<String>>,
    /// Honor `.gitignore`/`.gitexclude` (including nested files) when walking.
    /// Default `true`. Set `false` to index gitignored files (e.g. committed
    /// generated code); the built-in prune list and `exclude` still apply.
    pub respect_gitignore: Option<bool>,
    /// Worker threads for the parallel parse phase. Absent / `0` = auto
    /// (`available_parallelism`); clamped to `1..=64`. Overridden by the
    /// `CARTOG_JOBS` env var and the `cartog index --jobs N` flag (flag > env >
    /// this). The value applies on every index, including under a long-lived
    /// `serve`/`watch`.
    pub jobs: Option<usize>,
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
pub(crate) fn validate_remote_no_credentials(table: &toml::value::Table) -> Result<(), String> {
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Filesystem path to the cartog SQLite database. Supports `~` expansion.
    pub path: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Cap concurrent in-flight HTTP embedding requests for ollama/openai.
    /// `None` = 4. `CARTOG_EMBED_CONCURRENCY` overrides; clamped `1..=16`.
    /// Ignored for `provider = "local"`.
    pub max_concurrent_requests: Option<usize>,
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct RerankerConfig {
    /// Turn re-ranking off without naming a provider. `false` resolves the
    /// provider to `"none"` regardless of [`Self::provider`].
    ///
    /// Shipped in the `cartog init` template long before it was a real field,
    /// so a config carrying `enabled = false` was silently ignored and left
    /// the cross-encoder loaded. Honored here rather than rejected: erroring
    /// on a key our own template taught users to write would punish them for
    /// that bug.
    pub enabled: Option<bool>,
    /// Provider type: "local" (default) or "none".
    pub provider: Option<String>,
    /// Reranker model as a fastembed HF repo path (e.g. `BAAI/bge-reranker-base`).
    /// None = [`cartog_rag::DEFAULT_RERANKER_MODEL`]. Mirrors `[embedding] model`.
    pub model: Option<String>,
}

pub const DEFAULT_RERANKER_PROVIDER: &str = "local";
pub const RERANKER_PROVIDER_NONE: &str = "none";

impl RerankerConfig {
    /// Resolved provider name. `enabled = false` wins over an explicit
    /// `provider` so the two spellings can't disagree about being off.
    pub fn provider(&self) -> &str {
        if self.enabled == Some(false) {
            return RERANKER_PROVIDER_NONE;
        }
        self.provider
            .as_deref()
            .unwrap_or(DEFAULT_RERANKER_PROVIDER)
    }
}
