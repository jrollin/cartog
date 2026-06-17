pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";
pub const DEFAULT_OLLAMA_MODEL: &str = "nomic-embed-text";

pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_OPENAI_MODEL: &str = "text-embedding-3-small";
pub const DEFAULT_OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// Default cap on concurrent in-flight HTTP embedding requests (ollama/openai).
pub const DEFAULT_EMBED_CONCURRENCY: usize = 4;

#[cfg(feature = "provider-local")]
pub mod local;
#[cfg(feature = "provider-ollama")]
pub mod ollama;
#[cfg(feature = "provider-openai")]
pub mod openai;

// Shared HTTP fan-out helper; only the network providers need it.
#[cfg(any(feature = "provider-ollama", feature = "provider-openai"))]
pub(crate) mod concurrent;
