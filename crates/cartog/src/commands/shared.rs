//! Cross-cutting helpers shared by the command modules: DB open, token-budget
//! output, and the "no result" diagnostics (`empty_index_hint`, `did_you_mean`).
//!
//! The spinner/progress plumbing lives in [`super::progress`]; this module holds
//! the data-path helpers that every command body reaches for.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use cartog_db::{Database, DbError};

/// Open the DB for a **read** command without ever creating a `.cartog/`.
///
/// If the main DB file is absent (a config-less, un-indexed repo), this returns
/// an empty in-memory `Database` so the ~13 read command bodies are unchanged:
/// every query just comes back empty and [`empty_index_hint`] fires, pointing
/// the user at `cartog init` / `cartog index`. Write commands must use
/// [`open_db_create`] instead — they are gated on consent *above* `main`'s
/// dispatch, so they never reach this fallback.
pub(crate) fn open_db(path: &Path, embedding_dim: usize) -> Result<Database> {
    match Database::open_existing(path, embedding_dim) {
        Ok(db) => Ok(db),
        // No index yet → empty in-memory DB; the command returns empty + hint
        // and, crucially, no `.cartog/` is materialized for an un-opted repo.
        Err(DbError::NotFound { .. }) => {
            Database::open_memory().map_err(|e| open_db_error(path, e.into()))
        }
        Err(e) => Err(open_db_error(path, e.into())),
    }
}

/// Open the DB for a **write** command (`index` / `rag index`), creating the
/// `.cartog/` directory + file if needed. Used only after the consent gate in
/// `main` has confirmed the project is opted in — so this materializing the
/// `.cartog/` is intended, not a surprise.
pub(crate) fn open_db_create(path: &Path, embedding_dim: usize) -> Result<Database> {
    Database::open(path, embedding_dim).map_err(|e| open_db_error(path, e.into()))
}

/// Map a database-open failure to an actionable message naming the path and the
/// fix. Corruption ("not a database") and read-only mounts produce the most
/// confusing raw SQLite errors, so they get specific remediation; anything else
/// keeps a generic wrapper with the path. The original error is the cause.
pub(crate) fn open_db_error(path: &Path, err: anyhow::Error) -> anyhow::Error {
    let raw = err.to_string().to_ascii_lowercase();
    let p = path.display();
    let hint = if raw.contains("not a database") {
        format!(
            "database at {p} is corrupt or not a cartog database — \
             delete it and run `cartog index .` to rebuild"
        )
    } else if raw.contains("readonly") || raw.contains("read-only") {
        format!(
            "database at {p} is not writable — check the file and directory \
             permissions, or set [database].path to a writable location"
        )
    } else {
        format!("failed to open cartog database at {p}")
    };
    err.context(hint)
}

/// Estimate token count from a string using chars/4 approximation.
#[cfg(test)]
pub(crate) fn estimate_tokens(s: &str) -> u32 {
    (s.len() as u32).div_ceil(4)
}

/// Truncate a string to fit within a token budget, appending a truncation notice.
pub(crate) fn truncate_to_budget(s: &str, max_tokens: u32) -> String {
    let max_bytes = (max_tokens as usize) * 4;
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Find a char boundary at or before max_bytes, leaving room for notice
    let notice = "\n... (truncated to fit token budget)";
    let target = max_bytes.saturating_sub(notice.len());
    // UTF-8 chars are at most 4 bytes, so we only need to check 4 positions back.
    let cut = (target.saturating_sub(3)..=target)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    let mut out = s[..cut].to_string();
    out.push_str(notice);
    out
}

/// Print `data` as pretty JSON if `json` is true, otherwise call `human_fmt`.
/// When `token_budget` is Some, truncate human-readable output to fit.
pub(crate) fn output<T: Serialize>(
    data: &T,
    json: bool,
    token_budget: Option<u32>,
    human_fmt: impl FnOnce(&T) -> String,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(data)?);
    } else {
        let text = human_fmt(data);
        match token_budget {
            Some(budget) => print!("{}", truncate_to_budget(&text, budget)),
            None => print!("{}", text),
        }
    }
    Ok(())
}

/// Hint suffix appended to "no result" messages when the index is empty, so a
/// fresh user can tell "you haven't indexed yet" from a genuine no-match.
/// Returns `""` when the index has symbols (the common case).
pub(crate) fn empty_index_hint(db: &Database) -> &'static str {
    match db.is_empty() {
        Ok(true) => " (index is empty — run 'cartog init' then 'cartog index .' first)",
        _ => "",
    }
}

/// Suggestion suffix for "no result" messages: when a navigation command
/// (refs/callees/impact/hierarchy) finds no exact match but the fuzzy search
/// surfaces similarly-named symbols, list them so the user can correct a typo
/// or partial name. Returns `""` when the index is empty (the empty-index hint
/// covers that) or when there are no near matches.
pub(crate) fn did_you_mean(db: &Database, name: &str) -> String {
    if name.is_empty() || matches!(db.is_empty(), Ok(true)) {
        return String::new();
    }
    let candidates = match db.search(name, None, None, 5) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    // An exact match means the symbol exists but genuinely has no edges/results;
    // suggesting it would be noise.
    if candidates.iter().any(|s| s.name == name) || candidates.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = candidates.iter().map(|s| s.name.as_str()).collect();
    format!(" — did you mean: {}?", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartog_core::{Symbol, SymbolKind};

    fn db_with_symbol(name: &str) -> Database {
        use cartog_core::FileInfo;
        let db = Database::open_memory().unwrap();
        db.upsert_file(&FileInfo {
            path: "a.rs".into(),
            last_modified: 0.0,
            hash: "h".into(),
            language: "rust".into(),
            num_symbols: 1,
        })
        .unwrap();
        let sym = Symbol::new(name, SymbolKind::Class, "a.rs", 1, 2, 0, 10, None);
        db.insert_symbols(&[sym]).unwrap();
        db
    }

    #[test]
    fn open_db_error_corrupt_names_path_and_rebuild() {
        let e = anyhow::anyhow!("file is not a database");
        let msg = open_db_error(Path::new("/p/.cartog/db.sqlite"), e).to_string();
        assert!(msg.contains("/p/.cartog/db.sqlite"), "names path: {msg}");
        assert!(msg.contains("corrupt"), "{msg}");
        assert!(msg.contains("cartog index"), "{msg}");
    }

    #[test]
    fn open_db_error_readonly_names_path_and_permissions() {
        let e = anyhow::anyhow!("attempt to write a readonly database");
        let msg = open_db_error(Path::new("/p/db.sqlite"), e).to_string();
        assert!(msg.contains("/p/db.sqlite"), "{msg}");
        assert!(msg.contains("permission"), "{msg}");
    }

    #[test]
    fn open_db_error_generic_keeps_path() {
        let e = anyhow::anyhow!("disk full");
        let msg = open_db_error(Path::new("/p/db.sqlite"), e).to_string();
        assert!(msg.contains("/p/db.sqlite"), "{msg}");
    }

    #[test]
    fn did_you_mean_suggests_near_matches() {
        let db = db_with_symbol("ReviewResult");
        let hint = did_you_mean(&db, "Revie");
        assert!(hint.contains("did you mean"), "got: {hint}");
        assert!(hint.contains("ReviewResult"), "got: {hint}");
    }

    #[test]
    fn did_you_mean_silent_on_exact_match() {
        // An exact match means the symbol exists but has no edges — no suggestion.
        let db = db_with_symbol("ReviewResult");
        assert_eq!(did_you_mean(&db, "ReviewResult"), "");
    }

    #[test]
    fn did_you_mean_silent_on_empty_index() {
        let db = Database::open_memory().unwrap();
        assert_eq!(did_you_mean(&db, "Whatever"), "");
    }

    #[test]
    fn did_you_mean_silent_when_no_candidates() {
        let db = db_with_symbol("ReviewResult");
        assert_eq!(did_you_mean(&db, "ZZZnomatch"), "");
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn test_truncate_to_budget_within_limit() {
        let text = "short text";
        let result = truncate_to_budget(text, 100);
        assert_eq!(result, text);
    }

    #[test]
    fn test_truncate_to_budget_exceeds_limit() {
        let text = "a".repeat(200);
        let result = truncate_to_budget(&text, 10);
        assert!(result.len() <= 40 + 50); // budget bytes + notice
        assert!(result.ends_with("... (truncated to fit token budget)"));
    }

    #[test]
    fn test_truncate_to_budget_exact_boundary() {
        let text = "abcd"; // 4 bytes = 1 token
        let result = truncate_to_budget(text, 1);
        assert_eq!(result, "abcd");
    }

    #[test]
    fn test_truncate_to_budget_unicode() {
        // Each emoji is 4 bytes
        let text = "Hello 🌍🌍🌍🌍🌍🌍🌍🌍🌍🌍";
        let result = truncate_to_budget(text, 5);
        assert!(result.ends_with("... (truncated to fit token budget)"));
        // Should not panic on char boundary issues
    }

    #[test]
    fn empty_index_hint_present_on_fresh_db() {
        // Non-empty case is covered by cartog-db's is_empty_reflects_symbol_presence.
        let db = Database::open_memory().unwrap();
        assert!(empty_index_hint(&db).contains("cartog index"));
    }

    #[test]
    fn empty_index_hint_mentions_init() {
        let db = Database::open_memory().unwrap();
        assert!(empty_index_hint(&db).contains("cartog init"));
    }

    #[test]
    fn open_db_falls_back_to_memory_without_creating_dir() {
        // A read command on a fresh, un-indexed repo must NOT materialize
        // `.cartog/` — it gets an empty in-memory DB and the empty-index hint.
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join(".cartog").join("db.sqlite");

        let db = open_db(&db_path, cartog_db::DEFAULT_EMBEDDING_DIM).unwrap();
        assert!(db.is_empty().unwrap(), "fallback DB must be empty");
        assert!(
            !db_path.parent().unwrap().exists(),
            "open_db must NOT create .cartog/ for a read on a fresh repo"
        );
    }

    #[test]
    fn open_db_opens_an_existing_index() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join(".cartog").join("db.sqlite");
        // Materialize a real DB, then confirm open_db opens it (not the fallback).
        Database::open(&db_path, cartog_db::DEFAULT_EMBEDDING_DIM).unwrap();
        let db = open_db(&db_path, cartog_db::DEFAULT_EMBEDDING_DIM).unwrap();
        assert!(db.is_empty().unwrap());
        assert!(db_path.exists());
    }

    proptest::proptest! {
        /// `s[..cut]` would panic if `cut` landed mid-codepoint.
        #[test]
        fn truncate_never_panics(s in ".*", budget in 0u32..64) {
            let _ = truncate_to_budget(&s, budget);
        }

        /// Within budget → returned verbatim, no notice. Budget is derived from
        /// the string so every case exercises the in-budget branch (a fixed
        /// budget range would reject most strings via prop_assume).
        #[test]
        fn truncate_within_budget_is_verbatim(s in ".{0,200}", slack in 0u32..50) {
            let budget = (s.len() as u32).div_ceil(4) + slack;
            proptest::prop_assert_eq!(truncate_to_budget(&s, budget), s);
        }

        /// When truncation fires, the kept content stays within the byte budget.
        #[test]
        fn truncate_respects_byte_budget(s in ".{0,500}", budget in 0u32..200) {
            let max_bytes = (budget as usize) * 4;
            proptest::prop_assume!(s.len() > max_bytes);
            let notice = "\n... (truncated to fit token budget)";
            let out = truncate_to_budget(&s, budget);
            proptest::prop_assert!(out.ends_with(notice), "truncated output must carry the notice");
            let content = &out[..out.len() - notice.len()];
            proptest::prop_assert!(
                content.len() <= max_bytes,
                "kept {} content bytes > {} budget",
                content.len(),
                max_bytes
            );
        }
    }
}
