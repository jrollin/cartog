use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::provider::EmbeddingProvider;

/// Generic OpenAI-compatible embedding provider using the `/v1/embeddings` endpoint.
///
/// Speaks the shape shared by OpenAI, Mistral, Voyage, Jina, OVHcloud, and local
/// servers (Ollama `/v1`, LM Studio, vLLM, LocalAI, HF TEI). Switch vendors by
/// changing `base_url`. The API key is read from an env var, never config.
/// Azure OpenAI is **not** supported here: it uses a `…/deployments/{id}/embeddings
/// ?api-version=…` path and an `api-key:` header that this `{base}/embeddings`
/// + `Bearer` client cannot express.
pub struct OpenAiEmbeddingProvider {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
    /// Fingerprint identity = `model@host`. Two backends serving the same model
    /// name produce different vector spaces, so the host must be part of the
    /// identity or swapping `base_url` would leave a stale index unreconciled.
    fingerprint_model: String,
    dim: usize,
    api_key: Option<String>,
    /// Name of the env var the key was read from, for the auth-failure hint.
    api_key_env: String,
    /// Emit the `dimensions` param only when the user pinned an explicit dimension.
    send_dimensions: bool,
    /// Max concurrent in-flight requests (clamped `1..=16`). `1` = serial.
    concurrency: usize,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedDatum>,
}

#[derive(Deserialize)]
struct EmbedDatum {
    embedding: Vec<f32>,
    index: usize,
}

/// Build the fingerprint identity `model@host` from the model and base URL.
/// Strips scheme and path so `https://api.openai.com/v1` → `api.openai.com`,
/// keeping any explicit port. Used so a `base_url` swap to a different backend
/// (same model name) is detected as a fingerprint change and re-embeds.
fn fingerprint_model(model: &str, base_url: &str) -> String {
    let host = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or(base_url);
    format!("{model}@{host}")
}

fn connect_hint(base_url: &str) -> String {
    format!(
        "cannot reach OpenAI endpoint at {base_url}. \
         Check [embedding.openai].base_url in .cartog.toml"
    )
}

/// Remediation for an HTTP error status: 401/403 means the key is missing or
/// wrong, 404 means the endpoint has no such model; everything else stays generic.
fn status_hint(model: &str, api_key_env: &str, status: Option<reqwest::StatusCode>) -> String {
    match status {
        Some(reqwest::StatusCode::UNAUTHORIZED) | Some(reqwest::StatusCode::FORBIDDEN) => {
            format!("auth failed; set the {api_key_env} environment variable")
        }
        Some(reqwest::StatusCode::NOT_FOUND) => {
            format!(
                "endpoint returned 404 — check [embedding.openai].base_url ends in /v1, \
                 and that model '{model}' exists"
            )
        }
        _ => "OpenAI endpoint returned an error".to_string(),
    }
}

fn connect_err(base_url: &str, e: reqwest::Error) -> anyhow::Error {
    anyhow::anyhow!(e).context(connect_hint(base_url))
}

fn status_err(model: &str, api_key_env: &str, e: reqwest::Error) -> anyhow::Error {
    let hint = status_hint(model, api_key_env, e.status());
    anyhow::anyhow!(e).context(hint)
}

impl OpenAiEmbeddingProvider {
    /// Build a provider for an OpenAI-compatible `/v1/embeddings` endpoint.
    ///
    /// - `base_url`: endpoint base ending in `/v1` (default [`DEFAULT_OPENAI_BASE_URL`]).
    /// - `model`: embedding model name (default [`DEFAULT_OPENAI_MODEL`]).
    /// - `dimension`: explicit output dimension; `None` auto-detects via a probe request.
    /// - `api_key_env`: env var holding the API key (default [`DEFAULT_OPENAI_API_KEY_ENV`]);
    ///   an unset or empty value sends no auth header (keyless local endpoints).
    ///
    /// Errors if the HTTP client cannot be built or the dimension probe fails.
    ///
    /// [`DEFAULT_OPENAI_BASE_URL`]: super::DEFAULT_OPENAI_BASE_URL
    /// [`DEFAULT_OPENAI_MODEL`]: super::DEFAULT_OPENAI_MODEL
    /// [`DEFAULT_OPENAI_API_KEY_ENV`]: super::DEFAULT_OPENAI_API_KEY_ENV
    pub fn new(
        base_url: Option<&str>,
        model: Option<&str>,
        dimension: Option<usize>,
        api_key_env: Option<&str>,
        max_concurrent: usize,
    ) -> Result<Self> {
        let base_url = base_url
            .unwrap_or(super::DEFAULT_OPENAI_BASE_URL)
            .trim_end_matches('/');
        let model = model.unwrap_or(super::DEFAULT_OPENAI_MODEL);
        let api_key_env = api_key_env.unwrap_or(super::DEFAULT_OPENAI_API_KEY_ENV);
        // Missing/empty key is fine: keyless local /v1 endpoints send no auth
        // header. An exported-but-empty var must not become `Bearer ` (empty token).
        let api_key = std::env::var(api_key_env).ok().filter(|k| !k.is_empty());

        info!(
            base_url,
            model, "Connecting to OpenAI-compatible embedding endpoint"
        );

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("Failed to create HTTP client")?;

        let send_dimensions = dimension.is_some();
        let dim = match dimension {
            Some(d) => d,
            None => {
                info!("Detecting embedding dimension from OpenAI endpoint...");
                let probe = Self::request(
                    &client,
                    base_url,
                    api_key.as_deref(),
                    api_key_env,
                    &EmbedRequest {
                        model,
                        input: &["dimension probe"],
                        dimensions: None,
                    },
                )?;
                let d = probe.data.first().map(|e| e.embedding.len()).unwrap_or(0);
                if d == 0 {
                    anyhow::bail!("OpenAI endpoint returned empty embedding for model '{model}'");
                }
                info!(dimension = d, "Detected OpenAI embedding dimension");
                d
            }
        };

        Ok(Self {
            client,
            base_url: base_url.to_string(),
            model: model.to_string(),
            fingerprint_model: fingerprint_model(model, base_url),
            dim,
            api_key,
            api_key_env: api_key_env.to_string(),
            send_dimensions,
            concurrency: super::concurrent::clamp_concurrency(max_concurrent),
        })
    }

    /// POST one embeddings request, attaching auth only when a key is present.
    fn request(
        client: &reqwest::blocking::Client,
        base_url: &str,
        api_key: Option<&str>,
        api_key_env: &str,
        body: &EmbedRequest<'_>,
    ) -> Result<EmbedResponse> {
        let mut req = client.post(format!("{base_url}/embeddings")).json(body);
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        req.send()
            .map_err(|e| connect_err(base_url, e))?
            .error_for_status()
            .map_err(|e| status_err(body.model, api_key_env, e))?
            .json::<EmbedResponse>()
            .context("Failed to parse OpenAI embed response")
    }

    /// One `/v1/embeddings` POST for `chunk`, validated + reordered to input order.
    fn embed_one_request(&self, chunk: &[&str]) -> Result<Vec<Vec<f32>>> {
        let resp = Self::request(
            &self.client,
            &self.base_url,
            self.api_key.as_deref(),
            &self.api_key_env,
            &EmbedRequest {
                model: &self.model,
                input: chunk,
                dimensions: self.send_dimensions.then_some(self.dim),
            },
        )?;
        reorder_by_index(resp.data, chunk.len(), self.dim)
    }

    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Serial keeps the 2048-input wire shape; concurrent uses smaller chunks
        // to fan out. Both use the retry-wrapped closure (concurrency-independent
        // resilience; per-ordinal jitter de-correlates concurrent retries).
        let chunk_size = if self.concurrency <= 1 {
            MAX_INPUTS_PER_REQUEST
        } else {
            OPENAI_SUB_BATCH
        };
        let sub_batches: Vec<&[&str]> = texts.chunks(chunk_size).collect();
        super::concurrent::run_concurrent(sub_batches, self.concurrency, |ordinal, chunk| {
            super::concurrent::with_retry(ordinal, || self.embed_one_request(chunk))
        })
    }
}

/// Max inputs per `/v1/embeddings` request. OpenAI caps at 2048; this also keeps
/// each request well under typical per-request token limits for code-sized text.
const MAX_INPUTS_PER_REQUEST: usize = 2048;

/// Concurrent-path sub-batch size: smaller than the 2048 cap so a typical
/// indexer batch (512) splits into several in-flight requests.
const OPENAI_SUB_BATCH: usize = 128;

/// Reorder response data to request order, validating count, the index
/// permutation, and per-vector dimension in one pass. The API does not
/// guarantee response order; a non-conformant server (duplicate or out-of-range
/// indices) would otherwise bind embeddings to the wrong inputs silently, since
/// count and dimension alone both pass.
fn reorder_by_index(
    mut data: Vec<EmbedDatum>,
    expected: usize,
    dim: usize,
) -> Result<Vec<Vec<f32>>> {
    if data.len() != expected {
        anyhow::bail!(
            "OpenAI endpoint returned {} embeddings for {expected} inputs",
            data.len()
        );
    }
    data.sort_by_key(|d| d.index);
    for (i, datum) in data.iter().enumerate() {
        if datum.index != i {
            anyhow::bail!(
                "OpenAI endpoint returned non-contiguous indices (expected {i}, got {})",
                datum.index
            );
        }
        if datum.embedding.len() != dim {
            anyhow::bail!(
                "OpenAI embedding[{i}] has dimension {} but expected {dim}",
                datum.embedding.len()
            );
        }
    }
    Ok(data.into_iter().map(|d| d.embedding).collect())
}

impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn model_id(&self) -> &str {
        &self.fingerprint_model
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    fn embed_document(&mut self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed_texts(&[text])?;
        results
            .into_iter()
            .next()
            .context("No embedding returned from OpenAI endpoint")
    }

    fn embed_documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_texts(texts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_request_omits_dimensions_when_none() {
        let req = EmbedRequest {
            model: "text-embedding-3-small",
            input: &["hello", "world"],
            dimensions: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("text-embedding-3-small"));
        assert!(json.contains("hello"));
        assert!(!json.contains("dimensions"));
    }

    #[test]
    fn embed_request_includes_dimensions_when_set() {
        let req = EmbedRequest {
            model: "text-embedding-3-small",
            input: &["hello"],
            dimensions: Some(256),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"dimensions\":256"));
    }

    #[test]
    fn reorder_by_index_aligns_out_of_order_response() {
        let json = r#"{"data":[{"embedding":[0.4,0.5,0.6],"index":1},{"embedding":[0.1,0.2,0.3],"index":0}]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        let out = reorder_by_index(resp.data, 2, 3).unwrap();
        assert_eq!(out[0], vec![0.1, 0.2, 0.3]);
        assert_eq!(out[1], vec![0.4, 0.5, 0.6]);
    }

    #[test]
    fn reorder_by_index_rejects_duplicate_indices() {
        // All-zero indices must not silently pass — they'd mispair vectors.
        let json = r#"{"data":[{"embedding":[0.1],"index":0},{"embedding":[0.2],"index":0}]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        let err = reorder_by_index(resp.data, 2, 1).unwrap_err();
        assert!(err.to_string().contains("non-contiguous"), "got: {err}");
    }

    #[test]
    fn reorder_by_index_rejects_one_based_indices() {
        let json = r#"{"data":[{"embedding":[0.1],"index":1},{"embedding":[0.2],"index":2}]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        let err = reorder_by_index(resp.data, 2, 1).unwrap_err();
        assert!(err.to_string().contains("non-contiguous"), "got: {err}");
    }

    #[test]
    fn reorder_by_index_rejects_count_mismatch() {
        let json = r#"{"data":[{"embedding":[0.1],"index":0}]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        let err = reorder_by_index(resp.data, 2, 1).unwrap_err();
        assert!(err.to_string().contains("for 2 inputs"), "got: {err}");
    }

    #[test]
    fn reorder_by_index_rejects_wrong_dimension() {
        let json = r#"{"data":[{"embedding":[0.1,0.2],"index":0}]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        let err = reorder_by_index(resp.data, 1, 3).unwrap_err();
        assert!(err.to_string().contains("dimension 2"), "got: {err}");
    }

    #[test]
    fn fingerprint_model_includes_host_so_base_url_swap_reembeds() {
        // Same model name on two backends must yield distinct fingerprints.
        let a = fingerprint_model("text-embedding-3-small", "https://api.openai.com/v1");
        let b = fingerprint_model("text-embedding-3-small", "https://api.mistral.ai/v1");
        assert_eq!(a, "text-embedding-3-small@api.openai.com");
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_model_keeps_explicit_port() {
        let f = fingerprint_model("nomic-embed-text", "http://localhost:11434/v1");
        assert_eq!(f, "nomic-embed-text@localhost:11434");
    }

    #[test]
    fn connect_hint_points_at_endpoint_and_config() {
        let h = connect_hint("https://api.openai.com/v1");
        assert!(h.contains("https://api.openai.com/v1"));
        assert!(h.contains("base_url"));
    }

    #[test]
    fn status_hint_401_says_set_env_var() {
        let h = status_hint(
            "text-embedding-3-small",
            "OPENAI_API_KEY",
            Some(reqwest::StatusCode::UNAUTHORIZED),
        );
        assert!(h.contains("OPENAI_API_KEY"), "got: {h}");
        assert!(h.contains("auth failed"), "got: {h}");
    }

    #[test]
    fn status_hint_403_also_says_set_env_var() {
        let h = status_hint("m", "MY_KEY", Some(reqwest::StatusCode::FORBIDDEN));
        assert!(h.contains("MY_KEY"), "got: {h}");
    }

    #[test]
    fn status_hint_404_mentions_base_url_and_model() {
        let h = status_hint(
            "text-embedding-3-large",
            "OPENAI_API_KEY",
            Some(reqwest::StatusCode::NOT_FOUND),
        );
        assert!(h.contains("text-embedding-3-large"), "got: {h}");
        assert!(h.contains("base_url"), "got: {h}");
    }

    #[test]
    fn status_hint_other_status_stays_generic() {
        let h = status_hint("m", "K", Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR));
        assert_eq!(h, "OpenAI endpoint returned an error");
    }

    /// Ship gate for OpenAI fan-out: splitting one big request into N concurrent
    /// sub-batches must produce byte-identical per-text vectors. Needs a live
    /// endpoint — run with `CARTOG_OPENAI_LIVE=1` (+ base_url/model/key env).
    /// Until this passes, `create_embedding_provider` forces concurrency = 1.
    #[test]
    #[ignore = "requires a live OpenAI-compatible endpoint; set CARTOG_OPENAI_LIVE=1"]
    fn sub_batch_split_matches_serial_request() {
        if std::env::var("CARTOG_OPENAI_LIVE").is_err() {
            return;
        }
        // More than OPENAI_SUB_BATCH so the concurrent path actually splits.
        let owned: Vec<String> = (0..OPENAI_SUB_BATCH * 3)
            .map(|i| format!("text {i}"))
            .collect();
        let texts: Vec<&str> = owned.iter().map(String::as_str).collect();

        let serial = OpenAiEmbeddingProvider::new(None, None, None, None, 1).unwrap();
        let concurrent = OpenAiEmbeddingProvider::new(None, None, None, None, 4).unwrap();
        let a = serial.embed_texts(&texts).unwrap();
        let b = concurrent.embed_texts(&texts).unwrap();
        assert_eq!(
            a, b,
            "sub-batch splitting must be byte-identical to one request"
        );
    }
}
