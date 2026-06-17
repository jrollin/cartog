use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::provider::EmbeddingProvider;

/// Ollama embedding provider using the `/api/embed` endpoint.
pub struct OllamaEmbeddingProvider {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
    dim: usize,
    /// Max concurrent in-flight requests (clamped `1..=16`). `1` = serial.
    concurrency: usize,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

fn connect_hint(base_url: &str) -> String {
    format!(
        "cannot reach Ollama at {base_url}. Is `ollama serve` running? \
         Check [embedding.ollama].base_url in .cartog.toml"
    )
}

/// Remediation text for an HTTP error status. A 404 from `/api/embed` means the
/// requested model has not been pulled; everything else stays generic.
fn status_hint(model: &str, status: Option<reqwest::StatusCode>) -> String {
    if status == Some(reqwest::StatusCode::NOT_FOUND) {
        format!("Ollama has no model '{model}'. Run `ollama pull {model}`")
    } else {
        "Ollama returned an error".to_string()
    }
}

/// Turn a connection-level reqwest failure into actionable guidance: a refused
/// connection almost always means the Ollama server isn't running or the
/// configured `base_url` is wrong.
fn connect_err(base_url: &str, e: reqwest::Error) -> anyhow::Error {
    anyhow::anyhow!(e).context(connect_hint(base_url))
}

/// Turn an HTTP error status into actionable guidance (see [`status_hint`]).
fn status_err(model: &str, e: reqwest::Error) -> anyhow::Error {
    let hint = status_hint(model, e.status());
    anyhow::anyhow!(e).context(hint)
}

impl OllamaEmbeddingProvider {
    pub fn new(
        base_url: Option<&str>,
        model: Option<&str>,
        dimension: Option<usize>,
        max_concurrent: usize,
    ) -> Result<Self> {
        let base_url = base_url
            .unwrap_or(super::DEFAULT_OLLAMA_BASE_URL)
            .trim_end_matches('/');
        let model = model.unwrap_or(super::DEFAULT_OLLAMA_MODEL);

        info!(base_url, model, "Connecting to Ollama embedding server");

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("Failed to create HTTP client")?;

        // Probe the server to detect dimension if not explicitly set
        let dim = match dimension {
            Some(d) => d,
            None => {
                info!("Detecting embedding dimension from Ollama...");
                let probe_resp = client
                    .post(format!("{base_url}/api/embed"))
                    .json(&EmbedRequest {
                        model,
                        input: &["dimension probe"],
                    })
                    .send()
                    .map_err(|e| connect_err(base_url, e))?
                    .error_for_status()
                    .map_err(|e| status_err(model, e))?
                    .json::<EmbedResponse>()
                    .context("Failed to parse Ollama response")?;

                let d = probe_resp.embeddings.first().map(|v| v.len()).unwrap_or(0);
                if d == 0 {
                    anyhow::bail!(
                        "Ollama returned empty embedding — check model '{model}' is pulled"
                    );
                }
                info!(dimension = d, "Detected Ollama embedding dimension");
                d
            }
        };

        Ok(Self {
            client,
            base_url: base_url.to_string(),
            model: model.to_string(),
            dim,
            concurrency: super::concurrent::clamp_concurrency(max_concurrent),
        })
    }

    /// One `/api/embed` POST for `texts`, validated for count and per-vector dim.
    fn embed_one_request(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let resp = self
            .client
            .post(format!("{}/api/embed", self.base_url))
            .json(&EmbedRequest {
                model: &self.model,
                input: texts,
            })
            .send()
            .map_err(|e| connect_err(&self.base_url, e))?
            .error_for_status()
            .map_err(|e| status_err(&self.model, e))?
            .json::<EmbedResponse>()
            .context("Failed to parse Ollama embed response")?;

        if resp.embeddings.len() != texts.len() {
            anyhow::bail!(
                "Ollama returned {} embeddings for {} inputs",
                resp.embeddings.len(),
                texts.len()
            );
        }

        for (i, emb) in resp.embeddings.iter().enumerate() {
            if emb.len() != self.dim {
                anyhow::bail!(
                    "Ollama embedding[{i}] has dimension {} but expected {dim}",
                    emb.len(),
                    dim = self.dim
                );
            }
        }

        Ok(resp.embeddings)
    }

    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Serial = one whole-array POST; concurrent splits + fans out. Both use
        // the retry-wrapped closure, so resilience is concurrency-independent.
        let sub_batches: Vec<&[&str]> = if self.concurrency <= 1 {
            vec![texts]
        } else {
            texts.chunks(OLLAMA_SUB_BATCH).collect()
        };
        super::concurrent::run_concurrent(sub_batches, self.concurrency, |ordinal, chunk| {
            super::concurrent::with_retry(ordinal, || self.embed_one_request(chunk))
        })
    }
}

/// Concurrent-path sub-batch size for Ollama. Smaller than OpenAI's: a local
/// server has more variable per-request latency, so more, smaller units help.
const OLLAMA_SUB_BATCH: usize = 64;

impl EmbeddingProvider for OllamaEmbeddingProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    /// Returns the exact string the user configured, not the canonical
    /// name Ollama resolves it to. A typo (`nomic-embed-tex` vs
    /// `nomic-embed-text`) stays as written, so the next reconcile sees
    /// fingerprint drift. Intentional: we'd rather re-embed than silently
    /// keep stale vectors when the model identity is unclear.
    fn model_id(&self) -> &str {
        &self.model
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    fn embed_document(&mut self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed_texts(&[text])?;
        results
            .into_iter()
            .next()
            .context("No embedding returned from Ollama")
    }

    fn embed_documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_texts(texts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embed_request_serialization() {
        let req = EmbedRequest {
            model: "nomic-embed-text",
            input: &["hello world", "test"],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("nomic-embed-text"));
        assert!(json.contains("hello world"));
    }

    #[test]
    fn test_embed_response_deserialization() {
        let json = r#"{"embeddings":[[0.1, 0.2, 0.3],[0.4, 0.5, 0.6]]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.embeddings.len(), 2);
        assert_eq!(resp.embeddings[0], vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn connect_hint_points_at_server_and_config() {
        let h = connect_hint("http://localhost:11434");
        assert!(h.contains("http://localhost:11434"));
        assert!(h.contains("ollama serve"));
        assert!(h.contains("base_url"));
    }

    #[test]
    fn status_hint_404_says_pull_the_model() {
        let h = status_hint("nomic-embed-text", Some(reqwest::StatusCode::NOT_FOUND));
        assert!(h.contains("ollama pull nomic-embed-text"), "got: {h}");
    }

    #[test]
    fn status_hint_other_status_stays_generic() {
        let h = status_hint("m", Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR));
        assert_eq!(h, "Ollama returned an error");
    }

    /// Ship gate for Ollama fan-out: Ollama returns positional (un-indexed)
    /// embeddings, so splitting one `/api/embed` POST into N is a stronger
    /// assumption than OpenAI's array-batch. This must prove byte-identical
    /// per-text vectors against a live model before `create_embedding_provider`
    /// stops forcing concurrency = 1. Run with `CARTOG_OLLAMA_LIVE=1`.
    #[test]
    #[ignore = "requires a live Ollama server; set CARTOG_OLLAMA_LIVE=1"]
    fn sub_batch_split_matches_single_request() {
        if std::env::var("CARTOG_OLLAMA_LIVE").is_err() {
            return;
        }
        let owned: Vec<String> = (0..OLLAMA_SUB_BATCH * 3)
            .map(|i| format!("text {i}"))
            .collect();
        let texts: Vec<&str> = owned.iter().map(String::as_str).collect();

        let serial = OllamaEmbeddingProvider::new(None, None, None, 1).unwrap();
        let concurrent = OllamaEmbeddingProvider::new(None, None, None, 4).unwrap();
        let single = serial.embed_one_request(&texts).unwrap();
        let chunked = concurrent.embed_texts(&texts).unwrap();
        assert_eq!(
            single, chunked,
            "sub-batch splitting must be byte-identical to one /api/embed POST"
        );
    }
}
