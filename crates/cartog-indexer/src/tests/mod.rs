//! Unit tests for cartog-indexer, split by concern.

mod dedup;
mod git;
mod incremental;
mod jobs;
mod merkle;
mod progress;
mod redaction;
mod sfc;
mod walk_filter;

pub(super) fn visible_dir(tmp: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    let dir = tmp.path().join(name);
    std::fs::create_dir(&dir).unwrap();
    dir
}
