use anyhow::{Context, Result};
use tracing::info;

use crate::db::Database;

use super::embeddings::{embedding_to_bytes, EmbeddingEngine};

/// Result of a RAG indexing operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct RagIndexResult {
    pub symbols_embedded: u32,
    pub symbols_skipped: u32,
    pub total_content_symbols: u32,
}

/// Maximum number of texts sent to the embedding engine in one call.
/// fastembed sub-batches internally, but chunking here controls progress reporting.
const CHUNK_SIZE: usize = 512;

/// Maximum pending DB writes before flushing to SQLite.
const DB_BATCH_LIMIT: usize = 256;

/// Process a batch of texts through the embedding engine and write results to DB.
///
/// Returns the number of successfully processed items in this batch.
fn flush_embedding_batch(
    engine: &mut EmbeddingEngine,
    db: &Database,
    texts: &[String],
    symbol_ids: &[String],
    db_batch: &mut Vec<(i64, Vec<u8>)>,
    result: &mut RagIndexResult,
) -> Result<usize> {
    let str_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    match engine.embed_batch(&str_refs) {
        Ok(embeddings) => {
            for (embedding, sid) in embeddings.iter().zip(symbol_ids.iter()) {
                let embedding_id = db.get_or_create_embedding_id(sid)?;
                let bytes = embedding_to_bytes(embedding);
                db_batch.push((embedding_id, bytes));
                result.symbols_embedded += 1;

                if db_batch.len() >= DB_BATCH_LIMIT {
                    db.insert_embeddings(db_batch)?;
                    db_batch.clear();
                }
            }
            Ok(embeddings.len())
        }
        Err(e) => {
            // Batch failed — fall back to one-at-a-time to isolate the bad symbol
            tracing::warn!(error = %e, "Batch embedding failed, falling back to sequential");
            let mut count = 0;
            for (text, sid) in texts.iter().zip(symbol_ids.iter()) {
                match engine.embed(text) {
                    Ok(embedding) => {
                        let embedding_id = db.get_or_create_embedding_id(sid)?;
                        let bytes = embedding_to_bytes(&embedding);
                        db_batch.push((embedding_id, bytes));
                        result.symbols_embedded += 1;
                        count += 1;

                        if db_batch.len() >= DB_BATCH_LIMIT {
                            db.insert_embeddings(db_batch)?;
                            db_batch.clear();
                        }
                    }
                    Err(e2) => {
                        tracing::warn!(symbol = %sid, error = %e2, "embedding failed, skipping");
                        result.symbols_skipped += 1;
                    }
                }
            }
            Ok(count)
        }
    }
}

/// Build the compact embedding text for a symbol.
///
/// Uses `header + first line of source` only (~30-60 tokens) instead of full content.
/// BERT attention is O(n²) in sequence length, so this is the single biggest
/// performance lever. Full content stays in `symbol_content` for FTS5 and reranking.
pub fn compact_embedding_text(header: &str, content: &str) -> String {
    let first_line = content.lines().next().unwrap_or("");
    format!("{}\n{}", header, first_line)
}

/// Embed all symbols that have content but no embedding yet.
///
/// Requires the embedding model to be available (downloaded via `cartog rag setup`
/// or auto-downloaded on first use by fastembed).
/// When `force` is true, clears all existing embeddings and re-embeds everything.
pub fn index_embeddings(db: &Database, force: bool) -> Result<RagIndexResult> {
    info!("Loading embedding model...");
    let mut engine = EmbeddingEngine::new()
        .context("Failed to load embedding model. Run 'cartog rag setup' to download it.")?;

    let total_content_symbols = db.symbol_content_count()?;

    if force {
        info!("Force mode: clearing all existing embeddings");
        db.clear_all_embeddings()?;
    }

    let symbol_ids = if force {
        db.all_content_symbol_ids()?
    } else {
        db.symbols_needing_embeddings()?
    };

    let mut result = RagIndexResult {
        total_content_symbols,
        ..Default::default()
    };

    if symbol_ids.is_empty() {
        info!("No symbols need embedding");
        return Ok(result);
    }

    info!("Embedding {} symbols...", symbol_ids.len());

    let mut db_batch: Vec<(i64, Vec<u8>)> = Vec::with_capacity(DB_BATCH_LIMIT);
    let mut texts: Vec<String> = Vec::with_capacity(CHUNK_SIZE);
    let mut text_symbol_ids: Vec<String> = Vec::with_capacity(CHUNK_SIZE);

    let total = symbol_ids.len();
    let mut processed = 0usize;

    // Process in chunks, batch-fetching content for each chunk
    for chunk in symbol_ids.chunks(CHUNK_SIZE) {
        let chunk_vec: Vec<String> = chunk.to_vec();
        let content_map = db.get_symbol_contents_batch(&chunk_vec)?;

        for symbol_id in chunk {
            let (content, header) = match content_map.get(symbol_id) {
                Some(c) => c,
                None => {
                    result.symbols_skipped += 1;
                    continue;
                }
            };

            texts.push(compact_embedding_text(header, content));
            text_symbol_ids.push(symbol_id.clone());

            if texts.len() >= CHUNK_SIZE {
                let count = flush_embedding_batch(
                    &mut engine,
                    db,
                    &texts,
                    &text_symbol_ids,
                    &mut db_batch,
                    &mut result,
                )?;
                processed += count;
                texts.clear();
                text_symbol_ids.clear();

                if processed % 1000 < CHUNK_SIZE {
                    info!("  {processed}/{total} symbols embedded");
                }
            }
        }
    }

    // Flush remaining texts
    if !texts.is_empty() {
        let count = flush_embedding_batch(
            &mut engine,
            db,
            &texts,
            &text_symbol_ids,
            &mut db_batch,
            &mut result,
        )?;
        processed += count;
    }

    // Flush remaining DB writes
    if !db_batch.is_empty() {
        db.insert_embeddings(&db_batch)?;
    }

    info!(
        "Done: {} embedded, {} skipped ({processed}/{total} processed)",
        result.symbols_embedded, result.symbols_skipped
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_embedding_text_header_plus_first_line() {
        let header = "// File: auth.py | function validate_token";
        let content = "def validate_token(token: str) -> bool:\n    if token.is_expired():\n        raise TokenError('expired')\n    return True";
        let result = compact_embedding_text(header, content);
        assert_eq!(
            result,
            "// File: auth.py | function validate_token\ndef validate_token(token: str) -> bool:"
        );
    }

    #[test]
    fn test_compact_embedding_text_single_line_content() {
        let header = "// File: config.py | variable MAX_RETRIES";
        let content = "MAX_RETRIES = 3";
        let result = compact_embedding_text(header, content);
        assert_eq!(
            result,
            "// File: config.py | variable MAX_RETRIES\nMAX_RETRIES = 3"
        );
    }

    #[test]
    fn test_compact_embedding_text_empty_content() {
        let header = "// File: a.py | function foo";
        let content = "";
        let result = compact_embedding_text(header, content);
        assert_eq!(result, "// File: a.py | function foo\n");
    }

    #[test]
    fn test_compact_embedding_text_multiline_uses_only_first() {
        let header = "header";
        let content = "line1\nline2\nline3";
        let result = compact_embedding_text(header, content);
        assert_eq!(result, "header\nline1");
    }
}
