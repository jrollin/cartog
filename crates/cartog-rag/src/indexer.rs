use anyhow::Result;
use tracing::info;

use cartog_db::Database;

use super::provider::{embedding_to_bytes, EmbeddingProvider};

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
fn flush_embedding_batch<P: EmbeddingProvider + ?Sized>(
    provider: &mut P,
    db: &Database,
    texts: &[String],
    symbol_ids: &[String],
    db_batch: &mut Vec<(i64, Vec<u8>)>,
    result: &mut RagIndexResult,
) -> Result<usize> {
    let str_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    match provider.embed_documents(&str_refs) {
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
            tracing::warn!(error = %e, "Batch embedding failed, falling back to sequential");
            let mut count = 0;
            for (text, sid) in texts.iter().zip(symbol_ids.iter()) {
                match provider.embed_document(text) {
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

/// Embedding format version. Increment when changing `compact_embedding_text` logic.
///
/// Stored in metadata as `embedding_format_version`. When the stored version differs
/// from this constant, `index_embeddings` automatically forces a full re-embed.
pub const EMBEDDING_FORMAT_VERSION: u32 = 3;

/// Maximum bytes for the embedding text sent to the model.
///
/// BGE-small-en-v1.5 has a 512-token limit. Code tokenizes at ~3-4 chars/token,
/// so 500 bytes ≈ 125-170 tokens. Header + signature + first meaningful lines
/// capture the semantic core; full content remains in `symbol_content` for FTS5
/// and cross-encoder re-ranking.
const MAX_EMBED_TEXT_BYTES: usize = 500;

/// Minimum bytes of embedding text (after compaction) to be worth embedding.
/// Symbols that produce less than this are too trivial for vector similarity
/// (e.g. empty modules, bare re-exports). They remain searchable via FTS5.
const MIN_EMBED_TEXT_BYTES: usize = 40;

/// Build embedding text for a symbol: header + signature + significant body lines.
///
/// Skips blank lines, comment-only lines, and brace-only lines to maximize
/// semantic signal per token. Keeps decorators/annotations (they carry meaning
/// like `@login_required`, `#[derive(Serialize)]`).
///
/// Full content stays in `symbol_content` for FTS5 keyword search and
/// cross-encoder re-ranking — this function only controls what gets embedded
/// for vector similarity.
pub fn compact_embedding_text(header: &str, content: &str) -> String {
    let mut out = String::with_capacity(MAX_EMBED_TEXT_BYTES);
    out.push_str(header);

    for line in content.lines() {
        if out.len() >= MAX_EMBED_TEXT_BYTES {
            break;
        }
        if is_insignificant_line(line) {
            continue;
        }
        out.push('\n');
        let remaining = MAX_EMBED_TEXT_BYTES.saturating_sub(out.len());
        if line.len() > remaining {
            // Find a valid UTF-8 char boundary (max 4 bytes back)
            let cut = (remaining.saturating_sub(3)..=remaining)
                .rev()
                .find(|&i| line.is_char_boundary(i))
                .unwrap_or(0);
            out.push_str(&line[..cut]);
            break;
        }
        out.push_str(line);
    }

    out
}

/// Returns true for lines that add little semantic value for embedding:
/// blank lines, comment-only lines, and closing-brace-only lines.
fn is_insignificant_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Closing braces/brackets only: }, }, end, })
    if matches!(trimmed, "}" | "})" | "};" | "end" | ")" | "]" | "])") {
        return true;
    }
    // Comment-only lines across common languages.
    // Carefully excludes:
    //   - Rust attributes: #[...] and #![...]
    //   - Python *args/**kwargs and C/Rust pointer derefs: *foo, **bar
    if trimmed.starts_with("//")
        || (trimmed.starts_with('#') && !trimmed.starts_with("#[") && !trimmed.starts_with("#!["))
        || trimmed.starts_with("--")
        || (trimmed.starts_with("* ") || trimmed == "*")
        || trimmed.starts_with("/*")
        || trimmed.starts_with("'''")
        || trimmed.starts_with("\"\"\"")
    {
        return true;
    }
    false
}

/// Coarse-grained progress events emitted by [`index_embeddings`].
///
/// Plain data — no transport or runtime types — so callers (CLI, watcher,
/// MCP, tests) can adapt these to whatever channel they like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressUpdate {
    /// Preparing: text collection + sort, before any embedding calls.
    Preparing,
    /// One batch of symbols just finished embedding. Fires once per
    /// CHUNK_SIZE (≈512) symbols.
    Embedding { processed: u32, total: u32 },
    /// Final DB flush of any remaining batched writes.
    Storing,
}

/// Optional progress callback type accepted by [`index_embeddings`].
///
/// Called synchronously from inside the indexer (never on an async runtime).
/// Implementations must be cheap and non-blocking.
pub type ProgressCallback<'a> = &'a (dyn Fn(ProgressUpdate) + Send + Sync);

/// Embed all symbols that have content but no embedding yet.
///
/// Requires the embedding model to be available (downloaded via `cartog rag setup`
/// or auto-downloaded on first use by fastembed).
/// When `force` is true, clears all existing embeddings and re-embeds everything.
/// When `progress` is `Some`, the callback fires at each coarse phase boundary
/// (see [`ProgressUpdate`]). Pass `None` for the no-op default.
pub fn index_embeddings<P: EmbeddingProvider + ?Sized>(
    db: &Database,
    provider: &mut P,
    force: bool,
    progress: Option<ProgressCallback<'_>>,
) -> Result<RagIndexResult> {
    let emit = |u: ProgressUpdate| {
        if let Some(cb) = progress {
            cb(u);
        }
    };
    let total_content_symbols = db.symbol_content_count()?;

    // Auto-detect embedding format change and force re-embed
    let stored_version: u32 = db
        .get_metadata("embedding_format_version")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let format_changed = stored_version < EMBEDDING_FORMAT_VERSION;
    let force = force || format_changed;

    if format_changed {
        info!(
            "Embedding format upgraded (v{stored_version} → v{EMBEDDING_FORMAT_VERSION}), re-embedding all symbols"
        );
    }

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

    emit(ProgressUpdate::Preparing);
    info!("Embedding {} symbols...", symbol_ids.len());

    let total = symbol_ids.len();

    // Build all (text, symbol_id) pairs upfront, then sort by text length.
    // Sorting minimises padding waste in the ONNX model: texts of similar
    // token count land in the same batch, avoiding short texts being padded
    // to the longest text's length. This can cut inference time 30-50%.
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(total);
    for chunk in symbol_ids.chunks(CHUNK_SIZE) {
        let chunk_vec: Vec<String> = chunk.to_vec();
        let content_map = db.get_symbol_contents_batch(&chunk_vec)?;
        for symbol_id in chunk {
            match content_map.get(symbol_id) {
                Some((content, header)) => {
                    let text = compact_embedding_text(header, content);
                    if text.len() < MIN_EMBED_TEXT_BYTES {
                        result.symbols_skipped += 1;
                        continue;
                    }
                    pairs.push((text, symbol_id.clone()));
                }
                None => {
                    result.symbols_skipped += 1;
                }
            }
        }
    }
    pairs.sort_by_key(|(text, _)| text.len());

    let mut db_batch: Vec<(i64, Vec<u8>)> = Vec::with_capacity(DB_BATCH_LIMIT);
    let mut processed = 0usize;

    for batch in pairs.chunks(CHUNK_SIZE) {
        let texts: Vec<String> = batch.iter().map(|(t, _)| t.clone()).collect();
        let sids: Vec<String> = batch.iter().map(|(_, s)| s.clone()).collect();

        let count = flush_embedding_batch(provider, db, &texts, &sids, &mut db_batch, &mut result)?;
        processed += count;

        emit(ProgressUpdate::Embedding {
            processed: processed as u32,
            total: total as u32,
        });

        if processed % 1000 < CHUNK_SIZE {
            info!("  {processed}/{total} symbols embedded");
        }
    }

    // Flush remaining DB writes
    if !db_batch.is_empty() {
        emit(ProgressUpdate::Storing);
        db.insert_embeddings(&db_batch)?;
    }

    // Store the current embedding format version
    db.set_metadata(
        "embedding_format_version",
        &EMBEDDING_FORMAT_VERSION.to_string(),
    )?;

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
    fn test_compact_embedding_text_includes_significant_lines() {
        let header = "// File: auth.py | function validate_token";
        let content = "def validate_token(token: str) -> bool:\n    if token.is_expired():\n        raise TokenError('expired')\n    return True";
        let result = compact_embedding_text(header, content);
        assert!(result.contains("validate_token(token: str)"));
        assert!(result.contains("token.is_expired()"));
        assert!(result.contains("raise TokenError"));
        assert!(result.contains("return True"));
    }

    #[test]
    fn test_compact_embedding_text_skips_blanks_and_comments() {
        let header = "header";
        let content = "def foo():\n    # setup\n\n    x = 1\n    // another comment\n    y = 2\n\n    return x + y";
        let result = compact_embedding_text(header, content);
        assert!(result.contains("def foo():"));
        assert!(result.contains("x = 1"));
        assert!(result.contains("y = 2"));
        assert!(result.contains("return x + y"));
        assert!(!result.contains("# setup"));
        assert!(!result.contains("// another comment"));
    }

    #[test]
    fn test_compact_embedding_text_skips_closing_braces() {
        let header = "header";
        let content = "fn main() {\n    let x = 1;\n    println!(x);\n}";
        let result = compact_embedding_text(header, content);
        assert!(result.contains("fn main()"));
        assert!(result.contains("let x = 1;"));
        assert!(result.contains("println!(x);"));
        assert!(!result.ends_with("\n}"));
    }

    #[test]
    fn test_compact_embedding_text_keeps_decorators() {
        let header = "header";
        let content = "@login_required\n@cached(ttl=300)\ndef protected_view(request):\n    return render(request)";
        let result = compact_embedding_text(header, content);
        assert!(result.contains("@login_required"));
        assert!(result.contains("@cached(ttl=300)"));
        assert!(result.contains("def protected_view"));
    }

    #[test]
    fn test_compact_embedding_text_single_line() {
        let header = "// File: config.py | variable MAX_RETRIES";
        let content = "MAX_RETRIES = 3";
        let result = compact_embedding_text(header, content);
        assert!(result.contains("MAX_RETRIES = 3"));
    }

    #[test]
    fn test_compact_embedding_text_empty_content() {
        let header = "// File: a.py | function foo";
        let content = "";
        let result = compact_embedding_text(header, content);
        assert_eq!(result, "// File: a.py | function foo");
    }

    #[test]
    fn test_compact_embedding_text_respects_byte_limit() {
        let header = "header";
        // Build content with many significant lines that exceed MAX_EMBED_TEXT_BYTES
        let lines: Vec<String> = (0..100)
            .map(|i| format!("    let var_{i} = compute({i});"))
            .collect();
        let content = lines.join("\n");
        let result = compact_embedding_text(header, &content);
        assert!(result.len() <= MAX_EMBED_TEXT_BYTES + 50); // small tolerance for last line
    }

    #[test]
    fn test_is_insignificant_line() {
        // Should be insignificant (skipped)
        assert!(is_insignificant_line(""));
        assert!(is_insignificant_line("   "));
        assert!(is_insignificant_line("// comment"));
        assert!(is_insignificant_line("# comment"));
        assert!(is_insignificant_line("  # comment"));
        assert!(is_insignificant_line("  }"));
        assert!(is_insignificant_line("})"));
        assert!(is_insignificant_line("end"));
        assert!(is_insignificant_line("  * javadoc line"));
        assert!(is_insignificant_line("  \"\"\"docstring\"\"\""));
        assert!(is_insignificant_line("  * "));
        assert!(is_insignificant_line("*"));

        // Should be significant (kept)
        assert!(!is_insignificant_line("let x = 1;"));
        assert!(!is_insignificant_line("@login_required"));
        assert!(!is_insignificant_line("def foo():"));
        assert!(!is_insignificant_line("  return x + y"));
        assert!(!is_insignificant_line("  hash_map.insert(key, value);"));
    }

    #[test]
    fn test_is_insignificant_line_rust_attributes() {
        assert!(!is_insignificant_line("#[derive(Debug, Clone)]"));
        assert!(!is_insignificant_line("#![allow(unused)]"));
        assert!(!is_insignificant_line("  #[test]"));
        assert!(!is_insignificant_line("#[cfg(test)]"));
    }

    #[test]
    fn test_is_insignificant_line_python_star_args() {
        assert!(!is_insignificant_line("def foo(*args, **kwargs):"));
        assert!(!is_insignificant_line("  *args"));
        assert!(!is_insignificant_line("  **kwargs"));
    }

    #[test]
    fn test_is_insignificant_line_c_pointer_deref() {
        assert!(!is_insignificant_line("*ptr = 42;"));
        assert!(!is_insignificant_line("  *self.data"));
    }

    #[test]
    fn test_compact_embedding_text_utf8_boundary() {
        // Build a header that leaves very little room, then content with multi-byte chars
        let header = "h".repeat(MAX_EMBED_TEXT_BYTES - 20);
        let content = "café résumé naïve"; // multi-byte chars (é = 2 bytes)
        let result = compact_embedding_text(&header, content);
        // Should not panic and should be valid UTF-8
        assert!(result.len() <= MAX_EMBED_TEXT_BYTES + 10);
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn test_compact_embedding_text_all_insignificant() {
        let header = "header";
        let content = "# comment\n\n// another\n  }\n\nend";
        let result = compact_embedding_text(header, content);
        assert_eq!(result, "header");
    }

    #[test]
    fn test_embedding_format_version_is_current() {
        assert_eq!(EMBEDDING_FORMAT_VERSION, 3);
    }

    // ── index_embeddings tests with mock provider ──

    use crate::provider::test_utils::MockEmbeddingProvider;
    use cartog_core::{Symbol, SymbolKind};

    fn setup_db_with_symbols(n: usize) -> Database {
        let db = Database::open_memory().unwrap();
        for i in 0..n {
            let name = format!("func_{i}");
            let sym = Symbol::new(
                &name,
                SymbolKind::Function,
                "test.py",
                (i * 10 + 1) as u32,
                (i * 10 + 10) as u32,
                0,
                100,
                None,
            );
            db.insert_symbol(&sym).unwrap();
            let content = format!("def {name}(x):\n    return x * {i}\n");
            let header = format!("// File: test.py | function {name}");
            db.upsert_symbol_content(&sym.id, &name, &content, &header)
                .unwrap();
        }
        db
    }

    #[test]
    fn test_index_embeddings_basic() {
        let db = setup_db_with_symbols(5);
        let mut provider = MockEmbeddingProvider::new(384);

        let result = index_embeddings(&db, &mut provider, false, None).unwrap();
        assert_eq!(result.symbols_embedded, 5);
        assert_eq!(result.symbols_skipped, 0);
        assert_eq!(result.total_content_symbols, 5);
        assert!(provider.embed_count > 0);
    }

    #[test]
    fn test_index_embeddings_idempotent() {
        let db = setup_db_with_symbols(3);
        let mut provider = MockEmbeddingProvider::new(384);

        let r1 = index_embeddings(&db, &mut provider, false, None).unwrap();
        assert_eq!(r1.symbols_embedded, 3);

        let r2 = index_embeddings(&db, &mut provider, false, None).unwrap();
        assert_eq!(r2.symbols_embedded, 0, "second run should embed nothing");
    }

    #[test]
    fn test_index_embeddings_force_reembeds() {
        let db = setup_db_with_symbols(3);
        let mut provider = MockEmbeddingProvider::new(384);

        let r1 = index_embeddings(&db, &mut provider, false, None).unwrap();
        assert_eq!(r1.symbols_embedded, 3);

        let r2 = index_embeddings(&db, &mut provider, true, None).unwrap();
        assert_eq!(r2.symbols_embedded, 3, "force should re-embed everything");
    }

    #[test]
    fn test_index_embeddings_stores_format_version() {
        let db = setup_db_with_symbols(1);
        let mut provider = MockEmbeddingProvider::new(384);

        index_embeddings(&db, &mut provider, false, None).unwrap();

        let version: String = db
            .get_metadata("embedding_format_version")
            .unwrap()
            .unwrap();
        assert_eq!(version, EMBEDDING_FORMAT_VERSION.to_string());
    }

    #[test]
    fn test_index_embeddings_empty_db() {
        let db = Database::open_memory().unwrap();
        let mut provider = MockEmbeddingProvider::new(384);

        let result = index_embeddings(&db, &mut provider, false, None).unwrap();
        assert_eq!(result.symbols_embedded, 0);
        assert_eq!(result.total_content_symbols, 0);
        assert_eq!(provider.embed_count, 0);
    }

    #[test]
    fn test_index_embeddings_skips_trivial_content() {
        let db = Database::open_memory().unwrap();
        let sym = Symbol::new("tiny", SymbolKind::Function, "a.py", 1, 2, 0, 10, None);
        db.insert_symbol(&sym).unwrap();
        // Content + header below MIN_EMBED_TEXT_BYTES (40 bytes)
        db.upsert_symbol_content(&sym.id, "tiny", "x=1", "h")
            .unwrap();

        let mut provider = MockEmbeddingProvider::new(384);
        let result = index_embeddings(&db, &mut provider, false, None).unwrap();
        assert_eq!(result.symbols_skipped, 1);
        assert_eq!(result.symbols_embedded, 0);
    }

    // ── progress callback ──

    #[test]
    fn progress_callback_fires_preparing_embedding_storing() {
        use std::sync::Mutex;

        let db = setup_db_with_symbols(3);
        let mut provider = MockEmbeddingProvider::new(384);

        let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
        let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
        index_embeddings(&db, &mut provider, false, Some(&cb)).unwrap();

        let events = events.into_inner().unwrap();
        assert!(!events.is_empty(), "expected at least one event");
        assert_eq!(events[0], ProgressUpdate::Preparing);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProgressUpdate::Embedding { total, .. } if *total == 3)),
            "expected at least one Embedding event with total=3, got {events:?}"
        );
        assert_eq!(events.last(), Some(&ProgressUpdate::Storing));
    }

    #[test]
    fn progress_callback_silent_when_nothing_to_embed() {
        use std::sync::Mutex;

        let db = Database::open_memory().unwrap();
        let mut provider = MockEmbeddingProvider::new(384);

        let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
        let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
        index_embeddings(&db, &mut provider, false, Some(&cb)).unwrap();

        assert!(events.into_inner().unwrap().is_empty());
    }

    #[test]
    fn progress_callback_none_matches_some_for_result() {
        let db1 = setup_db_with_symbols(3);
        let mut p1 = MockEmbeddingProvider::new(384);
        let r_none = index_embeddings(&db1, &mut p1, false, None).unwrap();

        let db2 = setup_db_with_symbols(3);
        let mut p2 = MockEmbeddingProvider::new(384);
        let cb = |_: ProgressUpdate| {};
        let r_some = index_embeddings(&db2, &mut p2, false, Some(&cb)).unwrap();

        assert_eq!(r_none.symbols_embedded, r_some.symbols_embedded);
        assert_eq!(r_none.symbols_skipped, r_some.symbols_skipped);
        assert_eq!(r_none.total_content_symbols, r_some.total_content_symbols);
    }
}
