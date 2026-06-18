//! Conversions from the parsed config into cartog-rag/indexer runtime types.

use super::*;

/// Resolve whether the watcher should auto-embed, as an explicit override.
///
/// Precedence: `CARTOG_WATCH_RAG` env (`0`/`false`/`1`/`true`) > `[embedding]
/// auto_embed` > `--rag` flag. Returns `None` when no explicit signal is set,
/// leaving the watcher to decide from the live `embedding_count` (auto-detect).
#[must_use]
pub fn resolve_auto_embed(cli_rag: bool, config: &CartogConfig) -> Option<bool> {
    resolve_auto_embed_with(
        std::env::var("CARTOG_WATCH_RAG").ok().as_deref(),
        config.embedding.as_ref().and_then(|e| e.auto_embed),
        cli_rag,
    )
}

/// Pure resolver for [`resolve_auto_embed`], split out so tests don't mutate the
/// process environment. An unparseable env value falls through to the next tier.
pub(crate) fn resolve_auto_embed_with(
    env: Option<&str>,
    cfg: Option<bool>,
    cli_rag: bool,
) -> Option<bool> {
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

/// Build the [`WalkFilter`](cartog_indexer::WalkFilter) from the `[index]`
/// section: the `exclude` globs plus `respect_gitignore` (default `true`).
/// Absent section → no excludes, gitignore honored.
///
/// # Errors
/// Returns the offending pattern's message if any glob is malformed or empty,
/// so a bad `[index] exclude` entry is rejected at config load.
pub fn to_walk_filter(config: &CartogConfig) -> Result<cartog_indexer::WalkFilter, String> {
    let index = config.index.as_ref();
    let globs = index.and_then(|i| i.exclude.as_deref()).unwrap_or(&[]);
    let exclude = cartog_indexer::ExcludeGlobs::from_globs(globs)
        .map_err(|e| format!("[index] exclude: {e:#}"))?;
    let respect_gitignore = index.and_then(|i| i.respect_gitignore).unwrap_or(true);
    let jobs = resolve_jobs(parse_env_usize("CARTOG_JOBS"), index.and_then(|i| i.jobs));
    let lsp_max_servers = resolve_lsp_max_servers(
        parse_env_usize("CARTOG_LSP_MAX_SERVERS"),
        config.lsp.as_ref().and_then(|l| l.max_concurrent_servers),
    );
    Ok(cartog_indexer::WalkFilter {
        exclude,
        respect_gitignore,
        jobs,
        lsp_max_servers,
    })
}

/// Resolve the parse-pool size: env (`CARTOG_JOBS`) > `[index] jobs` > 0 (auto).
/// The `--jobs` flag wins over both and is applied by the caller. `0` stays 0
/// (auto); it is resolved + clamped inside the indexer.
pub(crate) fn resolve_jobs(env: Option<usize>, toml: Option<usize>) -> usize {
    env.or(toml).unwrap_or(0)
}

/// Resolve the concurrent-LSP-server cap: env (`CARTOG_LSP_MAX_SERVERS`) >
/// `[lsp] max_concurrent_servers` > 0 (auto). `0` stays 0 (auto →
/// `min(languages, 4)`); clamped at the cartog-lsp use site.
pub(crate) fn resolve_lsp_max_servers(env: Option<usize>, toml: Option<usize>) -> usize {
    env.or(toml).unwrap_or(0)
}

/// Resolve the network-embed concurrency cap: env (`CARTOG_EMBED_CONCURRENCY`) >
/// `[embedding] max_concurrent_requests` > default 4, clamped `1..=16`. Applies
/// to ollama/openai only (the local arm never reads it).
pub(crate) fn resolve_embed_concurrency(env: Option<usize>, toml: Option<usize>) -> usize {
    env.or(toml)
        .unwrap_or(cartog_rag::providers::DEFAULT_EMBED_CONCURRENCY)
        .clamp(1, 16)
}

/// Read a non-negative integer env var, warning and ignoring a malformed value.
fn parse_env_usize(var: &str) -> Option<usize> {
    parse_usize_or_warn(var, std::env::var(var).ok()?.as_str())
}

/// Pure parse + warn, split out so it is testable without touching the env.
pub(crate) fn parse_usize_or_warn(var: &str, raw: &str) -> Option<usize> {
    match raw.trim().parse::<usize>() {
        Ok(n) => Some(n),
        Err(_) => {
            // eprintln, not tracing: env resolution runs before the tracing
            // subscriber is initialised in main, so a `tracing::warn!` here is
            // dropped (cf. warn_legacy_db_once). TTY-gated to spare MCP/--json.
            if config_diagnostics_visible() {
                eprintln!("cartog: {var}={raw:?} is not a non-negative integer; ignoring");
            }
            None
        }
    }
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
                max_concurrent_requests: Some(resolve_embed_concurrency(
                    parse_env_usize("CARTOG_EMBED_CONCURRENCY"),
                    embed.max_concurrent_requests,
                )),
            }
        }
        None => cartog_rag::EmbeddingProviderConfig {
            reranker_provider,
            reranker_model,
            ..Default::default()
        },
    }
}
