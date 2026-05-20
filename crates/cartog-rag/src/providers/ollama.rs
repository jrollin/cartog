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
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

impl OllamaEmbeddingProvider {
    pub fn new(
        base_url: Option<&str>,
        model: Option<&str>,
        dimension: Option<usize>,
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
                        input: vec!["dimension probe"],
                    })
                    .send()
                    .context("Failed to connect to Ollama server")?
                    .error_for_status()
                    .context("Ollama returned an error")?
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
        })
    }

    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let resp = self
            .client
            .post(format!("{}/api/embed", self.base_url))
            .json(&EmbedRequest {
                model: &self.model,
                input: texts.to_vec(),
            })
            .send()
            .context("Ollama embed request failed")?
            .error_for_status()
            .context("Ollama returned an error")?
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
}

impl EmbeddingProvider for OllamaEmbeddingProvider {
    fn name(&self) -> &str {
        "ollama"
    }

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
            input: vec!["hello world", "test"],
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
}
