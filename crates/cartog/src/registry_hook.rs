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
use cartog_registry::{Declared, DeclaredUpdate, DescriptionSource, ProjectFacts};

/// Record a project after an indexing pass committed.
///
/// Costs one `db.stats()` — five scans over `files`/`symbols`/`edges`. That is
/// **not** free, so callers gate it on the pass having changed something: a
/// no-op pass has nothing new to record, and paying five scans to write
/// unchanged numbers is the one case worth avoiding. A pass that did change the
/// graph just wrote those tables, so their pages are already warm.
///
/// Takes a [`DeclaredUpdate`], not a `Declared`: the caller decides whether it
/// *knows* the project's declared identity. A rejected config knows nothing and
/// must pass `Keep`, or its empty default `[project]` overwrites the stored one.
pub fn record_indexed(db: &Database, db_path: &Path, root: &Path, declared: DeclaredUpdate) {
    let facts = ProjectFacts {
        last_indexed: Some(now_unix()),
        declared,
        ..facts_with_counts(db, db_path, root)
    };
    cartog_registry::record_project(&facts);
}

/// Record a project's embedding state after `cartog rag index`.
///
/// Carries no `last_indexed`: embedding is not a graph index pass, and claiming
/// otherwise would make `projects list` report a stale graph as freshly
/// indexed. Reads only the embedding count, never `stats()`.
pub fn record_embedded(db: &Database, db_path: &Path, root: &Path, declared: DeclaredUpdate) {
    // Records the graph counts too, not just `embedding_count`.
    //
    // `cartog rag index` runs a full `index_directory` pass before embedding,
    // so on a project whose only ever cartog invocation is `rag index` this is
    // the write that creates the row. Supplying `embedding_count` alone left
    // `symbol_count`/`languages` NULL — and because the fingerprint then
    // matched, every later identity-only write took the skip path and never
    // filled them in. The project rendered as `? symbols` with no languages
    // forever, which in phase 1 (routing keys on name + languages + size) made
    // it unroutable.
    //
    // Still carries no `last_indexed`: see the doc comment above.
    record_measured_without_indexing(db, db_path, root, declared);
}

/// Record a project the user asked to register, without indexing it.
///
/// The backfill writer behind `cartog projects add` / `scan`. It reads the
/// counts off an index that already exists, so the row is immediately useful,
/// but carries **no `last_indexed`**: nothing here indexed anything, and
/// stamping "now" would report a month-old graph as freshly indexed. The
/// listing renders the resulting `NULL` as `never`, which is the honest
/// answer — this row's writer never indexed, exactly as for a `serve` startup
/// row.
///
/// Carrying no `last_indexed` also means the fingerprint fast path applies: a
/// re-`add` of an unchanged project refreshes `last_seen` and the declared
/// identity without paying `stats()` twice.
pub fn record_backfilled(db: &Database, db_path: &Path, root: &Path, declared: DeclaredUpdate) {
    record_measured_without_indexing(db, db_path, root, declared);
}

/// Record full counts for a project that was **not** indexed by this caller.
///
/// The shared body of [`record_embedded`] and [`record_backfilled`]: both
/// measure an index someone else built, so both must record every count and
/// leave `last_indexed` alone. Sharing one body is deliberate, exactly as
/// [`record_opened`] delegates to [`record_declared`] — the omission of
/// `last_indexed` is the invariant, and two copies could drift on it.
fn record_measured_without_indexing(
    db: &Database,
    db_path: &Path,
    root: &Path,
    declared: DeclaredUpdate,
) {
    let facts = ProjectFacts {
        declared,
        ..facts_with_counts(db, db_path, root)
    };
    cartog_registry::record_project(&facts);
}

/// Record that a project was opened, without measuring it.
///
/// Used by `cartog serve` startup: it opened the database but never counted
/// it, so registration costs no extra query. The registry's upsert leaves
/// every unknown column at its stored value, so this cannot erase counts an
/// earlier `index` recorded.
///
/// Delegates to [`record_declared`] with [`DeclaredUpdate::Keep`]: this caller
/// has no config in scope, so it must not clear a declared name or description
/// a config-aware writer stored. The two writers differ only in that update,
/// so keeping one body means they cannot drift.
pub fn record_opened(db_path: &Path, root: &Path) {
    record_declared(db_path, root, DeclaredUpdate::Keep);
}

/// Refresh a project's declared identity without measuring it.
///
/// The no-op counterpart of [`record_indexed`]: a `cartog index` pass that
/// changed no file still has config in scope, and a `[project] description` or
/// README edit changes no byte of the database — so the fingerprint fast path
/// would skip it. Editing either and re-running `index` must update what other
/// sessions see, which is why this writer exists rather than falling through to
/// the config-less [`record_opened`].
///
/// Costs no `stats()`: the counts are unchanged by definition, and the
/// registry's skip path rewrites only the three declared columns.
///
/// With [`DeclaredUpdate::Keep`] this is exactly [`record_opened`], which
/// delegates here.
pub fn record_declared(db_path: &Path, root: &Path, declared: DeclaredUpdate) {
    let mut facts = ProjectFacts {
        declared,
        ..ProjectFacts::identity_only(db_path, root)
    };
    read_fingerprint_into(&mut facts, db_path);
    cartog_registry::record_project(&facts);
}

/// Resolve a project's declared identity from its config values and its README.
///
/// Takes the two `[project]` values as primitives rather than the config
/// struct: this module lives in the library crate, and `config` is bin-only —
/// the same reason `allow_create` and `walk_filter` reach the lower crates as
/// primitives. Callers pass `ProjectConfig::name()` / `description()`, which
/// have already trimmed and validated them.
///
/// Name comes from `[project] name` only. `None` means "not declared" rather
/// than "unknown": the root basename is the reader's fallback, stored in the
/// separate `name` column.
///
/// Description is highest-wins: the declared one (source `Config`), else the
/// README's first prose paragraph (source `Readme`). Resolving both here rather
/// than in each command keeps the precedence in one place, and is why a README
/// edit shows up on the next index with no config change.
///
/// The result is repository-authored text destined to be read by an agent: it
/// is data on every downstream surface, never instructions.
#[must_use]
pub fn resolve_declared(name: Option<&str>, description: Option<&str>, root: &Path) -> Declared {
    let description = match description {
        Some(text) => Some(cartog_registry::Description {
            text: text.to_string(),
            source: DescriptionSource::Config,
        }),
        None => cartog_registry::readme_description(root),
    };
    Declared {
        name: name.map(str::to_string),
        description,
    }
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
    let probed = cartog_db::read_database_facts_at(db_path);
    facts.schema_version = probed.schema_version;
    facts.embed_provider = probed.embed_provider;
    facts.embed_model = probed.embed_model;
    facts.embed_dim = probed.embed_dim;
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

    /// RAII redirect of `CARTOG_REGISTRY`, restoring the previous value on drop.
    ///
    /// Mandatory for any test reaching `record_project`: it resolves the
    /// registry from the live environment, so an unguarded test writes into the
    /// developer's real user-global registry. Every user is `#[serial]` —
    /// `serial_test` is this crate's only test-serialization mechanism, so
    /// there is no second set of tests this guard can interleave with.
    struct RegistryEnvGuard(Option<std::ffi::OsString>);

    impl RegistryEnvGuard {
        fn set(value: &std::ffi::OsStr) -> Self {
            let prev = std::env::var_os(cartog_registry::REGISTRY_ENV);
            std::env::set_var(cartog_registry::REGISTRY_ENV, value);
            Self(prev)
        }
    }

    impl Drop for RegistryEnvGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var(cartog_registry::REGISTRY_ENV, v),
                None => std::env::remove_var(cartog_registry::REGISTRY_ENV),
            }
        }
    }

    #[test]
    fn the_config_description_wins_over_the_readme() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("README.md"), "Inferred from the readme.\n").unwrap();
        let declared =
            resolve_declared(Some("svc-billing"), Some("Declared in config."), dir.path());

        assert_eq!(declared.name.as_deref(), Some("svc-billing"));
        let d = declared.description.unwrap();
        assert_eq!(d.text, "Declared in config.");
        assert_eq!(d.source, DescriptionSource::Config);
    }

    #[test]
    fn the_readme_is_used_when_the_config_declares_no_description() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "# Title\n\nInferred from the readme.\n",
        )
        .unwrap();
        let declared = resolve_declared(Some("svc-billing"), None, dir.path());

        assert_eq!(declared.name.as_deref(), Some("svc-billing"));
        let d = declared.description.unwrap();
        assert_eq!(d.text, "Inferred from the readme.");
        assert_eq!(d.source, DescriptionSource::Readme);
    }

    #[test]
    fn neither_source_yields_an_empty_declaration() {
        // The empty declaration is `Set`-able: a writer that resolved nothing
        // must still clear a stale stored description, which is why this is
        // `Declared::default()` rather than an absent update.
        let dir = tempfile::TempDir::new().unwrap();

        assert_eq!(
            resolve_declared(None, None, dir.path()),
            Declared::default()
        );
    }

    #[test]
    fn a_config_without_a_name_leaves_the_declared_name_unset() {
        // `None` here means "not declared" — the reader falls back to the root
        // basename it stores separately, so this must not invent a name.
        let dir = tempfile::TempDir::new().unwrap();
        let declared = resolve_declared(None, Some("Only a description."), dir.path());

        assert_eq!(declared.name, None);
        assert!(declared.description.is_some());
    }

    #[test]
    #[serial_test::serial]
    fn an_indexed_project_stores_its_declared_name_and_description() {
        // The round trip through the real write path: `record_indexed` must
        // reach the stored columns, not just build the facts.
        let dir = tempfile::TempDir::new().unwrap();
        let registry = dir.path().join("reg.sqlite");
        let _env = RegistryEnvGuard::set(registry.as_os_str());
        let (db, db_path) = seeded_db(dir.path());
        let declared = resolve_declared(
            Some("svc-billing"),
            Some("Invoices and payments."),
            dir.path(),
        );

        record_indexed(&db, &db_path, dir.path(), DeclaredUpdate::Set(declared));

        let listing =
            cartog_registry::list_projects_at(&registry, None, cartog_db::CURRENT_SCHEMA_VERSION);
        let row = listing.projects.first().expect("the project was recorded");
        assert_eq!(row.declared_name.as_deref(), Some("svc-billing"));
        assert_eq!(row.display_name(), "svc-billing");
        let d = row.description.as_ref().expect("a stored description");
        assert_eq!(d.text, "Invoices and payments.");
        assert_eq!(d.source, DescriptionSource::Config);
    }

    #[test]
    #[serial_test::serial]
    fn an_embedding_pass_refreshes_the_declared_description_too() {
        // `rag index` is config-aware, so it resolves and writes the declared
        // identity like `index` does; only `last_indexed` differs.
        let dir = tempfile::TempDir::new().unwrap();
        let registry = dir.path().join("reg.sqlite");
        let _env = RegistryEnvGuard::set(registry.as_os_str());
        let (db, db_path) = seeded_db(dir.path());
        let declared = resolve_declared(None, Some("Embedded and described."), dir.path());

        record_embedded(&db, &db_path, dir.path(), DeclaredUpdate::Set(declared));

        let listing =
            cartog_registry::list_projects_at(&registry, None, cartog_db::CURRENT_SCHEMA_VERSION);
        let row = listing.projects.first().expect("the project was recorded");
        assert_eq!(
            row.description.as_ref().map(|d| d.text.as_str()),
            Some("Embedded and described.")
        );
        assert_eq!(row.last_indexed, None, "embedding is not an index pass");
    }

    #[test]
    #[serial_test::serial]
    fn opening_a_project_leaves_a_stored_description_intact() {
        // `serve` startup has no config, so it must not clear what `index`
        // stored — the Keep/Set distinction, end to end.
        let dir = tempfile::TempDir::new().unwrap();
        let registry = dir.path().join("reg.sqlite");
        let _env = RegistryEnvGuard::set(registry.as_os_str());
        let (db, db_path) = seeded_db(dir.path());
        let declared = resolve_declared(
            Some("svc-billing"),
            Some("Invoices and payments."),
            dir.path(),
        );
        record_indexed(&db, &db_path, dir.path(), DeclaredUpdate::Set(declared));

        record_opened(&db_path, dir.path());

        let listing =
            cartog_registry::list_projects_at(&registry, None, cartog_db::CURRENT_SCHEMA_VERSION);
        let row = listing.projects.first().expect("the project was recorded");
        assert_eq!(row.declared_name.as_deref(), Some("svc-billing"));
        assert_eq!(
            row.description.as_ref().map(|d| d.text.as_str()),
            Some("Invoices and payments.")
        );
    }

    #[test]
    #[serial_test::serial]
    fn removing_both_sources_clears_a_stored_description() {
        // A `Set` with no description must overwrite, not skip: the registry
        // would otherwise advertise a description the repo no longer makes.
        let dir = tempfile::TempDir::new().unwrap();
        let registry = dir.path().join("reg.sqlite");
        let _env = RegistryEnvGuard::set(registry.as_os_str());
        let (db, db_path) = seeded_db(dir.path());
        let declared = resolve_declared(
            Some("svc-billing"),
            Some("Invoices and payments."),
            dir.path(),
        );
        record_indexed(&db, &db_path, dir.path(), DeclaredUpdate::Set(declared));

        record_indexed(
            &db,
            &db_path,
            dir.path(),
            DeclaredUpdate::Set(resolve_declared(None, None, dir.path())),
        );

        let listing =
            cartog_registry::list_projects_at(&registry, None, cartog_db::CURRENT_SCHEMA_VERSION);
        let row = listing.projects.first().expect("the project was recorded");
        assert_eq!(row.declared_name, None);
        assert_eq!(row.description, None);
    }

    #[test]
    #[serial_test::serial]
    fn a_config_edit_refreshes_the_description_on_a_no_op_pass() {
        // Regression: a README or `[project]` edit changes no byte of the
        // database, so the fingerprint fast path skips the row. Routing that
        // pass through the config-less `record_opened` left every other
        // session reading the stale description forever (caught on a live
        // `cartog index`, not by a unit test).
        let dir = tempfile::TempDir::new().unwrap();
        let registry = dir.path().join("reg.sqlite");
        let _env = RegistryEnvGuard::set(registry.as_os_str());
        let (db, db_path) = seeded_db(dir.path());
        std::fs::write(dir.path().join("README.md"), "From the readme.\n").unwrap();
        record_indexed(
            &db,
            &db_path,
            dir.path(),
            DeclaredUpdate::Set(resolve_declared(None, None, dir.path())),
        );

        // No counts, no `last_indexed`: exactly what a no-op pass supplies.
        record_declared(
            &db_path,
            dir.path(),
            DeclaredUpdate::Set(resolve_declared(
                Some("svc-widgets"),
                Some("From the config."),
                dir.path(),
            )),
        );

        let listing =
            cartog_registry::list_projects_at(&registry, None, cartog_db::CURRENT_SCHEMA_VERSION);
        let row = listing.projects.first().expect("the project was recorded");
        assert_eq!(row.declared_name.as_deref(), Some("svc-widgets"));
        let d = row.description.as_ref().expect("a stored description");
        assert_eq!(d.text, "From the config.");
        assert_eq!(d.source, DescriptionSource::Config);
    }

    #[test]
    #[serial_test::serial]
    fn a_declared_only_write_does_not_erase_the_counts_an_index_recorded() {
        // It measures nothing, so every count column must stay put.
        let dir = tempfile::TempDir::new().unwrap();
        let registry = dir.path().join("reg.sqlite");
        let _env = RegistryEnvGuard::set(registry.as_os_str());
        let (db, db_path) = seeded_db(dir.path());
        record_indexed(
            &db,
            &db_path,
            dir.path(),
            DeclaredUpdate::Set(Declared::default()),
        );

        record_declared(
            &db_path,
            dir.path(),
            DeclaredUpdate::Set(resolve_declared(None, Some("Described later."), dir.path())),
        );

        let listing =
            cartog_registry::list_projects_at(&registry, None, cartog_db::CURRENT_SCHEMA_VERSION);
        let row = listing.projects.first().expect("the project was recorded");
        assert_eq!(row.symbol_count, Some(1), "counts must survive");
        assert!(row.last_indexed.is_some(), "last_indexed must survive");
    }

    #[test]
    #[serial_test::serial]
    fn a_keep_write_leaves_a_stored_declared_identity_untouched() {
        // A rejected `.cartog.toml` collapses to a default config, so a writer
        // that still sent `Set` wiped the declared name and replaced the
        // description with the README fallback: one typo anywhere in the file
        // erased what the project says it is.
        let dir = tempfile::TempDir::new().unwrap();
        let registry = dir.path().join("reg.sqlite");
        let _env = RegistryEnvGuard::set(registry.as_os_str());
        let (db, db_path) = seeded_db(dir.path());
        // A README exists, so a `Set` would visibly overwrite with its text.
        std::fs::write(dir.path().join("README.md"), "The readme fallback.\n").unwrap();
        record_indexed(
            &db,
            &db_path,
            dir.path(),
            DeclaredUpdate::Set(resolve_declared(
                Some("svc-billing"),
                Some("Invoices and payments."),
                dir.path(),
            )),
        );

        record_indexed(&db, &db_path, dir.path(), DeclaredUpdate::Keep);

        let listing =
            cartog_registry::list_projects_at(&registry, None, cartog_db::CURRENT_SCHEMA_VERSION);
        let row = listing.projects.first().expect("the project was recorded");
        assert_eq!(row.declared_name.as_deref(), Some("svc-billing"));
        let d = row.description.as_ref().expect("the stored description");
        assert_eq!(d.text, "Invoices and payments.");
        assert_eq!(d.source, DescriptionSource::Config);
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

        record_indexed(
            &db,
            &db_path,
            dir.path(),
            DeclaredUpdate::Set(Declared::default()),
        );
        record_embedded(
            &db,
            &db_path,
            dir.path(),
            DeclaredUpdate::Set(Declared::default()),
        );
        record_declared(&db_path, dir.path(), DeclaredUpdate::Keep);
        record_opened(&db_path, dir.path());

        match prev {
            Some(v) => std::env::set_var(cartog_registry::REGISTRY_ENV, v),
            None => std::env::remove_var(cartog_registry::REGISTRY_ENV),
        }
    }
}
