//! Recording indexed projects in the machine-local registry.
//!
//! This module is the bridge the registry crate deliberately does not own:
//! `cartog-registry` has no `cartog-db` dependency, so the graph-schema values
//! it stores (the schema version, the embedding fingerprint, the counts) are
//! read here — where `cartog-db` is already a dependency — and handed over as
//! plain primitives.
//!
//! Every function here is infallible by design. Registration is bookkeeping
//! that rides on a command the user actually asked for, so a registry problem
//! must never fail that command; the registry's own write path logs and
//! returns rather than propagating.

use std::path::Path;

use cartog_db::Database;
use cartog_registry::ProjectFacts;

/// Record a project after an indexing pass committed.
///
/// Costs one `db.stats()` — five scans over `files`/`symbols`/`edges`. That is
/// **not** free, so callers gate it on the pass having changed something: a
/// no-op pass has nothing new to record, and paying five scans to write
/// unchanged numbers is the one case worth avoiding. A pass that did change the
/// graph just wrote those tables, so their pages are already warm.
pub fn record_indexed(db: &Database, db_path: &Path, root: &Path) {
    let facts = ProjectFacts {
        last_indexed: Some(now_unix()),
        ..facts_with_counts(db, db_path, root)
    };
    cartog_registry::record_project(&facts);
}

/// Record a project's embedding state after `cartog rag index`.
///
/// Carries no `last_indexed`: embedding is not a graph index pass, and claiming
/// otherwise would make `projects list` report a stale graph as freshly
/// indexed. Reads only the embedding count, never `stats()`.
pub fn record_embedded(db: &Database, db_path: &Path, root: &Path) {
    let mut facts = ProjectFacts::identity_only(db_path, root);
    facts.embedding_count = db.embedding_count().ok();
    read_fingerprint_into(&mut facts, db_path);
    cartog_registry::record_project(&facts);
}

/// Record that a project was opened, without measuring it.
///
/// Used by `cartog serve` startup: it opened the database but never counted
/// it, so registration costs no extra query. The registry's upsert leaves
/// every unknown column at its stored value, so this cannot erase counts an
/// earlier `index` recorded.
pub fn record_opened(db_path: &Path, root: &Path) {
    let mut facts = ProjectFacts::identity_only(db_path, root);
    read_fingerprint_into(&mut facts, db_path);
    cartog_registry::record_project(&facts);
}

/// Facts including a full `stats()` read.
fn facts_with_counts(db: &Database, db_path: &Path, root: &Path) -> ProjectFacts {
    let mut facts = ProjectFacts::identity_only(db_path, root);
    if let Ok(stats) = db.stats() {
        facts.file_count = Some(stats.num_files);
        facts.symbol_count = Some(stats.num_symbols);
        facts.edge_count = Some(stats.num_edges);
        facts.resolved_count = Some(stats.num_resolved);
        facts.languages = Some(stats.languages);
    }
    facts.embedding_count = db.embedding_count().ok();
    read_fingerprint_into(&mut facts, db_path);
    facts
}

/// Read the schema version and embedding fingerprint off the database file.
///
/// Deliberately reads the **closed file** via `cartog-db`'s read-only probes
/// rather than the open handle: those are the same values a consumer inspecting
/// someone else's project would see, and they skip migrations and the drift
/// check. A probe failure leaves the field `None` — "not known" — never an
/// error.
fn read_fingerprint_into(facts: &mut ProjectFacts, db_path: &Path) {
    facts.schema_version = cartog_db::read_schema_version_at(db_path)
        .ok()
        .filter(|v| *v > 0);
    facts.embed_provider = cartog_db::read_metadata_at(db_path, cartog_db::EMBED_PROVIDER_KEY)
        .ok()
        .flatten();
    facts.embed_model = cartog_db::read_metadata_at(db_path, cartog_db::EMBED_MODEL_KEY)
        .ok()
        .flatten();
    facts.embed_dim = cartog_db::read_metadata_at(db_path, cartog_db::EMBED_DIMENSION_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real on-disk cartog DB with one symbol, so counts are non-trivial.
    fn seeded_db(dir: &Path) -> (Database, std::path::PathBuf) {
        let db_path = dir.join(".cartog").join("db.sqlite");
        let db = Database::open(&db_path, 384).unwrap();
        let sym = cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "a.rs",
            1,
            10,
            0,
            100,
            None,
        );
        db.insert_symbols(std::slice::from_ref(&sym)).unwrap();
        (db, db_path)
    }

    #[test]
    fn indexed_facts_carry_counts_languages_and_a_last_indexed() {
        let dir = tempfile::TempDir::new().unwrap();
        let (db, db_path) = seeded_db(dir.path());

        let facts = ProjectFacts {
            last_indexed: Some(now_unix()),
            ..facts_with_counts(&db, &db_path, dir.path())
        };

        assert_eq!(facts.symbol_count, Some(1));
        assert!(facts.file_count.is_some());
        assert!(facts.languages.is_some());
        assert!(
            facts.last_indexed.is_some(),
            "an index pass must stamp last_indexed, or the fingerprint skip could suppress it"
        );
    }

    #[test]
    fn opened_facts_carry_no_counts_so_they_cannot_clobber_an_index() {
        let dir = tempfile::TempDir::new().unwrap();
        let (_db, db_path) = seeded_db(dir.path());

        let mut facts = ProjectFacts::identity_only(&db_path, dir.path());
        read_fingerprint_into(&mut facts, &db_path);

        assert_eq!(facts.symbol_count, None);
        assert_eq!(facts.file_count, None);
        assert_eq!(facts.languages, None);
        assert_eq!(
            facts.last_indexed, None,
            "opening a project is not indexing it"
        );
    }

    #[test]
    fn the_schema_version_is_read_from_the_closed_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let (db, db_path) = seeded_db(dir.path());
        drop(db);

        let mut facts = ProjectFacts::identity_only(&db_path, dir.path());
        read_fingerprint_into(&mut facts, &db_path);

        assert_eq!(
            facts.schema_version,
            Some(cartog_db::CURRENT_SCHEMA_VERSION)
        );
    }

    #[test]
    fn a_non_cartog_file_yields_no_schema_version_rather_than_zero() {
        // read_schema_version_at returns Ok(0) for a non-cartog file; storing 0
        // would render as a real version and flag `stale-schema` misleadingly.
        let dir = tempfile::TempDir::new().unwrap();
        let foreign = dir.path().join("foreign.db");
        std::fs::write(&foreign, b"not a database").unwrap();

        let mut facts = ProjectFacts::identity_only(&foreign, dir.path());
        read_fingerprint_into(&mut facts, &foreign);

        assert_eq!(facts.schema_version, None);
    }

    #[test]
    fn an_embedding_fingerprint_round_trips_from_the_closed_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let (db, db_path) = seeded_db(dir.path());
        db.reconcile_embedding_fingerprint(&cartog_db::EmbeddingFingerprint {
            provider: "local".to_string(),
            model: "bge-small".to_string(),
            dimension: 384,
        })
        .unwrap();
        drop(db);

        let mut facts = ProjectFacts::identity_only(&db_path, dir.path());
        read_fingerprint_into(&mut facts, &db_path);

        assert_eq!(facts.embed_provider.as_deref(), Some("local"));
        assert_eq!(facts.embed_model.as_deref(), Some("bge-small"));
        assert_eq!(facts.embed_dim, Some(384));
    }

    #[test]
    fn embedded_facts_stamp_no_last_indexed() {
        // Embedding is not a graph index pass; claiming otherwise would report
        // a stale graph as freshly indexed.
        let dir = tempfile::TempDir::new().unwrap();
        let (db, db_path) = seeded_db(dir.path());

        let mut facts = ProjectFacts::identity_only(&db_path, dir.path());
        facts.embedding_count = db.embedding_count().ok();

        assert_eq!(facts.last_indexed, None);
        assert_eq!(facts.embedding_count, Some(0));
    }

    #[test]
    #[serial_test::serial]
    fn recording_a_project_never_panics_when_the_registry_is_disabled() {
        // The kill switch must be a no-op, not a failure path.
        //
        // `#[serial]`: mutates the process-global CARTOG_REGISTRY. Without it a
        // concurrent test could observe the empty value and write nowhere, or
        // this test could observe a real registry and touch the developer's
        // own file.
        let dir = tempfile::TempDir::new().unwrap();
        let (db, db_path) = seeded_db(dir.path());
        let prev = std::env::var_os(cartog_registry::REGISTRY_ENV);
        std::env::set_var(cartog_registry::REGISTRY_ENV, "");

        record_indexed(&db, &db_path, dir.path());
        record_embedded(&db, &db_path, dir.path());
        record_opened(&db_path, dir.path());

        match prev {
            Some(v) => std::env::set_var(cartog_registry::REGISTRY_ENV, v),
            None => std::env::remove_var(cartog_registry::REGISTRY_ENV),
        }
    }
}
