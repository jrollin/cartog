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
    batch: &[(String, String)],
    db_batch: &mut Vec<(i64, Vec<u8>)>,
    result: &mut RagIndexResult,
) -> Result<usize> {
    let str_refs: Vec<&str> = batch.iter().map(|(t, _)| t.as_str()).collect();
    match provider.embed_documents(&str_refs) {
        Ok(embeddings) => {
            for (embedding, (_, sid)) in embeddings.iter().zip(batch.iter()) {
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
            for (text, sid) in batch.iter() {
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

/// Embedding format version. Increment when `compact_embedding_text` logic changes
/// or to force a one-time full re-embed (e.g. healing embeddings drifted by an
/// older indexer that left modified symbols' vectors stale).
///
/// Stored in metadata as `embedding_format_version`. When the stored version differs
/// from this constant, `index_embeddings` automatically forces a full re-embed.
///
/// v4: heal drift from the pre-fix incremental path (modified symbols not re-embedded).
pub const EMBEDDING_FORMAT_VERSION: u32 = 4;

/// The embedding format version recorded in the DB (`1` if never stamped).
fn stored_format_version(db: &Database) -> Result<u32> {
    Ok(db
        .get_metadata("embedding_format_version")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(1))
}

/// True when the stored embedding format is behind the current version and there
/// is content to re-embed — i.e. `index_embeddings` would force a full re-embed.
/// Lets the watcher trigger the heal even when no symbol is *missing* an
/// embedding (drifted-but-present vectors aren't caught by
/// `symbols_needing_embeddings`).
pub fn embedding_format_upgrade_pending(db: &Database) -> Result<bool> {
    Ok(stored_format_version(db)? < EMBEDDING_FORMAT_VERSION && db.symbol_content_count()? > 0)
}

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

impl ProgressUpdate {
    /// Lower-case, transport-neutral phase label — single source of truth for
    /// RAG phase wording shared by CLI spinners and the MCP forwarder.
    pub fn label(&self) -> String {
        match self {
            ProgressUpdate::Preparing => "preparing".to_string(),
            ProgressUpdate::Embedding { processed, total } => {
                format!("embedding {processed}/{total}")
            }
            ProgressUpdate::Storing => "storing embeddings".to_string(),
        }
    }
}

/// Optional progress callback type accepted by [`index_embeddings`].
///
/// Called synchronously from inside the indexer (never on an async runtime).
/// Implementations must be cheap and non-blocking.
pub type ProgressCallback<'a> = &'a (dyn Fn(ProgressUpdate) + Send + Sync);

/// Optional cooperative-cancellation probe accepted by [`index_embeddings`].
///
/// Returns `true` when the caller wants the indexer to stop. Polled at phase
/// boundaries and once per embedding batch, so worst-case latency is one
/// CHUNK_SIZE worth of inference. When the probe trips, the function returns
/// `Err` whose root cause string is `"cancelled"`.
pub type CancelProbe<'a> = &'a (dyn Fn() -> bool + Send + Sync);

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
    cancel: Option<CancelProbe<'_>>,
) -> Result<RagIndexResult> {
    let emit = |u: ProgressUpdate| {
        if let Some(cb) = progress {
            cb(u);
        }
    };
    let check_cancel = || -> Result<()> {
        if cancel.is_some_and(|c| c()) {
            anyhow::bail!("cancelled");
        }
        Ok(())
    };
    let total_content_symbols = db.symbol_content_count()?;

    // Auto-detect embedding format change and force re-embed
    let stored_version = stored_format_version(db)?;
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

    check_cancel()?;
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

    // Progress total uses the post-filter pair count so `processed` can reach
    // 100%. The original `total = symbol_ids.len()` stays in tracing logs as
    // the user-facing "how many symbols did you ask me to embed?" figure.
    let progress_total = pairs.len() as u32;
    let mut db_batch: Vec<(i64, Vec<u8>)> = Vec::with_capacity(DB_BATCH_LIMIT);
    let mut processed = 0usize;

    for batch in pairs.chunks(CHUNK_SIZE) {
        check_cancel()?;
        let count = flush_embedding_batch(provider, db, batch, &mut db_batch, &mut result)?;
        processed += count;

        emit(ProgressUpdate::Embedding {
            processed: processed as u32,
            total: progress_total,
        });

        if processed % 1000 < CHUNK_SIZE {
            info!("  {processed}/{total} symbols embedded");
        }
    }

    // Always emit Storing once Preparing has fired (i.e. symbol_ids was
    // non-empty at line 233). Closes the progress sequence even when every
    // symbol was filtered out (empty pairs, no Embedding events) so clients
    // never get stuck on a lone "preparing" event with no terminal marker.
    emit(ProgressUpdate::Storing);
    if !db_batch.is_empty() {
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
        assert_eq!(EMBEDDING_FORMAT_VERSION, 4);
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

        let result = index_embeddings(&db, &mut provider, false, None, None).unwrap();
        assert_eq!(result.symbols_embedded, 5);
        assert_eq!(result.symbols_skipped, 0);
        assert_eq!(result.total_content_symbols, 5);
        assert!(provider.embed_count > 0);
    }

    #[test]
    fn format_upgrade_pending_only_with_old_version_and_content() {
        // No content → nothing to re-embed → not pending even at an old version.
        let empty = Database::open_memory().unwrap();
        empty.set_metadata("embedding_format_version", "1").unwrap();
        assert!(!embedding_format_upgrade_pending(&empty).unwrap());

        // Old stored version + content → pending.
        let db = setup_db_with_symbols(2);
        db.set_metadata("embedding_format_version", "1").unwrap();
        assert!(embedding_format_upgrade_pending(&db).unwrap());

        // Current version → not pending.
        db.set_metadata(
            "embedding_format_version",
            &EMBEDDING_FORMAT_VERSION.to_string(),
        )
        .unwrap();
        assert!(!embedding_format_upgrade_pending(&db).unwrap());
    }

    #[test]
    fn test_index_embeddings_idempotent() {
        let db = setup_db_with_symbols(3);
        let mut provider = MockEmbeddingProvider::new(384);

        let r1 = index_embeddings(&db, &mut provider, false, None, None).unwrap();
        assert_eq!(r1.symbols_embedded, 3);

        let r2 = index_embeddings(&db, &mut provider, false, None, None).unwrap();
        assert_eq!(r2.symbols_embedded, 0, "second run should embed nothing");
    }

    #[test]
    fn test_index_embeddings_force_reembeds() {
        let db = setup_db_with_symbols(3);
        let mut provider = MockEmbeddingProvider::new(384);

        let r1 = index_embeddings(&db, &mut provider, false, None, None).unwrap();
        assert_eq!(r1.symbols_embedded, 3);

        let r2 = index_embeddings(&db, &mut provider, true, None, None).unwrap();
        assert_eq!(r2.symbols_embedded, 3, "force should re-embed everything");
    }

    #[test]
    fn test_index_embeddings_stores_format_version() {
        let db = setup_db_with_symbols(1);
        let mut provider = MockEmbeddingProvider::new(384);

        index_embeddings(&db, &mut provider, false, None, None).unwrap();

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

        let result = index_embeddings(&db, &mut provider, false, None, None).unwrap();
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
        let result = index_embeddings(&db, &mut provider, false, None, None).unwrap();
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
        index_embeddings(&db, &mut provider, false, Some(&cb), None).unwrap();

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
        index_embeddings(&db, &mut provider, false, Some(&cb), None).unwrap();

        assert!(events.into_inner().unwrap().is_empty());
    }

    /// Regression: when symbol_ids is non-empty but every symbol is filtered
    /// out (content too small for MIN_EMBED_TEXT_BYTES, or missing from the
    /// content map), pairs ends up empty and no Embedding event fires. The
    /// final Storing must still be emitted so the client sees a terminal
    /// phase marker after Preparing and isn't left waiting forever.
    #[test]
    fn progress_callback_emits_storing_when_all_symbols_filtered() {
        use std::sync::Mutex;

        let db = Database::open_memory().unwrap();
        // Seed two symbols whose content + header are below MIN_EMBED_TEXT_BYTES
        // (40). Both will be filtered, leaving pairs empty.
        for i in 0..2 {
            let name = format!("tiny_{i}");
            let sym = Symbol::new(&name, SymbolKind::Function, "a.py", 1, 2, 0, 10, None);
            db.insert_symbol(&sym).unwrap();
            db.upsert_symbol_content(&sym.id, &name, "x=1", "h")
                .unwrap();
        }
        let mut provider = MockEmbeddingProvider::new(384);

        let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
        let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
        let result = index_embeddings(&db, &mut provider, false, Some(&cb), None).unwrap();

        assert_eq!(result.symbols_embedded, 0);
        assert_eq!(result.symbols_skipped, 2);
        let events = events.into_inner().unwrap();
        assert_eq!(events.first(), Some(&ProgressUpdate::Preparing));
        assert_eq!(events.last(), Some(&ProgressUpdate::Storing));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ProgressUpdate::Embedding { .. })),
            "no Embedding events expected when pairs is empty"
        );
    }

    /// B1 regression: when symbol count is a multiple of DB_BATCH_LIMIT (256),
    /// the inner `flush_embedding_batch` drains `db_batch` mid-loop and the
    /// outer `if !db_batch.is_empty()` check would skip Storing. The fix
    /// emits Storing whenever `pairs` is non-empty.
    #[test]
    fn progress_callback_emits_storing_when_db_batch_drained_midloop() {
        use std::sync::Mutex;

        let db = setup_db_with_symbols(DB_BATCH_LIMIT);
        let mut provider = MockEmbeddingProvider::new(384);

        let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
        let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
        index_embeddings(&db, &mut provider, false, Some(&cb), None).unwrap();

        let events = events.into_inner().unwrap();
        assert_eq!(events.last(), Some(&ProgressUpdate::Storing));
    }

    /// B2 regression: when some symbols are skipped (content too small), the
    /// `total` reported in Embedding events must reflect the post-filter
    /// pair count so a client can render a progress bar that reaches 100%.
    #[test]
    fn progress_callback_embedding_total_uses_post_filter_count() {
        use std::sync::Mutex;

        let db = Database::open_memory().unwrap();
        // 1 valid + 1 too-small symbol
        let s1 = Symbol::new("big", SymbolKind::Function, "a.py", 1, 2, 0, 10, None);
        db.insert_symbol(&s1).unwrap();
        let big_content =
            "def big_function():\n    return 'a value long enough for the embedding threshold'\n";
        db.upsert_symbol_content(&s1.id, "big", big_content, "// File: a.py | function big")
            .unwrap();
        let s2 = Symbol::new("tiny", SymbolKind::Function, "a.py", 5, 6, 0, 10, None);
        db.insert_symbol(&s2).unwrap();
        db.upsert_symbol_content(&s2.id, "tiny", "x=1", "h")
            .unwrap();

        let mut provider = MockEmbeddingProvider::new(384);
        let events: Mutex<Vec<ProgressUpdate>> = Mutex::new(Vec::new());
        let cb = |u: ProgressUpdate| events.lock().unwrap().push(u);
        index_embeddings(&db, &mut provider, false, Some(&cb), None).unwrap();

        let events = events.into_inner().unwrap();
        let emb = events
            .iter()
            .find_map(|e| match e {
                ProgressUpdate::Embedding { processed, total } => Some((*processed, *total)),
                _ => None,
            })
            .expect("expected an Embedding event");
        assert_eq!(
            emb,
            (1, 1),
            "total must reflect post-filter count, not symbol_ids.len()"
        );
    }

    #[test]
    fn progress_callback_none_matches_some_for_result() {
        let db1 = setup_db_with_symbols(3);
        let mut p1 = MockEmbeddingProvider::new(384);
        let r_none = index_embeddings(&db1, &mut p1, false, None, None).unwrap();

        let db2 = setup_db_with_symbols(3);
        let mut p2 = MockEmbeddingProvider::new(384);
        let cb = |_: ProgressUpdate| {};
        let r_some = index_embeddings(&db2, &mut p2, false, Some(&cb), None).unwrap();

        assert_eq!(r_none.symbols_embedded, r_some.symbols_embedded);
        assert_eq!(r_none.symbols_skipped, r_some.symbols_skipped);
        assert_eq!(r_none.total_content_symbols, r_some.total_content_symbols);
    }

    #[test]
    fn cancel_probe_returning_true_aborts_with_cancelled_error() {
        let db = setup_db_with_symbols(3);
        let mut provider = MockEmbeddingProvider::new(384);

        let probe = || true;
        let err = index_embeddings(&db, &mut provider, false, None, Some(&probe))
            .expect_err("embedding must abort when probe trips at first checkpoint");
        assert!(
            err.to_string().contains("cancelled"),
            "error must mention cancellation, got: {err}"
        );
    }

    #[test]
    fn cancel_probe_returning_false_runs_to_completion() {
        let db = setup_db_with_symbols(3);
        let mut provider = MockEmbeddingProvider::new(384);

        let probe = || false;
        let result = index_embeddings(&db, &mut provider, false, None, Some(&probe))
            .expect("non-cancelling probe must not affect normal embedding");
        assert_eq!(result.symbols_embedded, 3);
    }
}
