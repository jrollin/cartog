//! Shared `#[cfg(test)]` fixtures for the command-module tests: a small Python
//! repo indexed into a temp DB, plus a query-count probe. Used by the
//! `graph`, `search`, and `manage` command tests to drive `cmd_*` end-to-end.

use cartog_db::Database;
use cartog_indexer as indexer;

pub(crate) const CMD_FIXTURE_SRC: &str = "\
class Animal:
    def speak(self):
        return helper()


class Dog(Animal):
    def speak(self):
        return helper()


def helper():
    return 42


def main():
    d = Dog()
    return d.speak()
";

/// Index [`CMD_FIXTURE_SRC`] as `lib.py` and return the DB path. The TempDir is
/// returned so the caller keeps it alive for the test's duration. The index
/// root is a named subdir: the walker prunes dot-prefixed dirs, and a bare
/// TempDir name starts with ".tmp".
pub(crate) fn indexed_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("lib.py"), CMD_FIXTURE_SRC).unwrap();
    let db_path = tmp.path().join("cartog.db");
    let db = Database::open(&db_path, 384).unwrap();
    indexer::index_directory(
        &db,
        &root,
        true,
        false,
        None,
        None,
        indexer::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &indexer::WalkFilter::unrestricted(),
    )
    .expect("fixture indexes");
    drop(db);
    (tmp, db_path)
}

/// Logged query count — a delta proves a command hit the query layer (commands
/// print to stdout, so rendered content can't be asserted directly).
pub(crate) fn queries_logged(db_path: &std::path::Path) -> u64 {
    Database::open(db_path, 384)
        .unwrap()
        .savings_breakdown()
        .unwrap()
        .total_queries
}
