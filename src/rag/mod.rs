pub mod embeddings;
pub mod indexer;
pub mod reranker;
pub mod search;
pub mod setup;

/// Embedding dimension for the bge-small-en-v1.5 model.
pub const EMBEDDING_DIM: usize = 384;

/// Shared model cache directory for ONNX models (embedding + reranker).
///
/// Precedence:
/// 1. `FASTEMBED_CACHE_DIR` env var (fastembed's own convention)
/// 2. `XDG_CACHE_HOME/cartog/models` (XDG standard)
/// 3. `~/.cache/cartog/models` (fallback)
///
/// This avoids downloading 1.2GB of models per project (fastembed's default is
/// `.fastembed_cache` in CWD).
pub fn model_cache_dir() -> std::path::PathBuf {
    // 1. Respect fastembed's own env var
    if let Ok(dir) = std::env::var("FASTEMBED_CACHE_DIR") {
        return std::path::PathBuf::from(dir);
    }

    // 2. XDG_CACHE_HOME / cartog / models
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return std::path::PathBuf::from(xdg).join("cartog").join("models");
    }

    // 3. ~/.cache/cartog/models
    if let Some(home) = home_dir() {
        return home.join(".cache").join("cartog").join("models");
    }

    // Last resort: fastembed's default (CWD/.fastembed_cache)
    std::path::PathBuf::from(".fastembed_cache")
}

/// Get the user's home directory (no external dependency needed).
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE")) // Windows fallback
        .ok()
        .map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_cache_dir_is_not_local() {
        // Unless FASTEMBED_CACHE_DIR is explicitly set to a local path,
        // model_cache_dir should NOT return ".fastembed_cache" (the per-project default).
        let dir = model_cache_dir();
        let dir_str = dir.to_string_lossy();
        // On any system with HOME set, this should be an absolute path
        if std::env::var("FASTEMBED_CACHE_DIR").is_err() {
            assert!(
                dir_str.contains("cartog"),
                "cache dir should contain 'cartog', got: {dir_str}"
            );
            assert!(
                !dir_str.starts_with('.'),
                "cache dir should be absolute, not relative: {dir_str}"
            );
        }
    }

    #[test]
    fn test_model_cache_dir_ends_with_models() {
        if std::env::var("FASTEMBED_CACHE_DIR").is_err() {
            let dir = model_cache_dir();
            assert!(
                dir.ends_with("models"),
                "cache dir should end with 'models', got: {}",
                dir.display()
            );
        }
    }
}
