//! Tests for walk filtering: gitignore, exclude globs, and the dir floor.

use super::visible_dir;
use crate::*;

fn index_dir(db: &cartog_db::Database, root: &Path, force: bool) -> Result<IndexResult> {
    index_directory(
        db,
        root,
        force,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
}

#[test]
fn index_refuses_empty_root_when_db_has_files() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let src = visible_dir(&tmp, "proj");
    std::fs::write(src.join("a.py"), "def foo():\n    return 1\n").unwrap();
    index_dir(&db, &src, false).unwrap();
    assert!(!db.all_files().unwrap().is_empty());

    // Pointing the same DB at a root with no supported files (the
    // `rag index --db X` from-a-wrong-cwd footgun) must refuse instead
    // of sweeping every indexed file away.
    let empty = visible_dir(&tmp, "elsewhere");
    let res = index_dir(&db, &empty, false);
    assert!(res.is_err(), "expected refusal, got {res:?}");
    assert!(
        !db.all_files().unwrap().is_empty(),
        "index must be left untouched on refusal"
    );
}

#[test]
fn index_force_allows_emptying_the_index() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let src = visible_dir(&tmp, "proj");
    std::fs::write(src.join("a.py"), "def foo():\n    return 1\n").unwrap();
    index_dir(&db, &src, false).unwrap();

    let empty = visible_dir(&tmp, "elsewhere");
    index_dir(&db, &empty, true).unwrap();
    assert!(db.all_files().unwrap().is_empty());
}

fn index_dir_excluding(
    db: &cartog_db::Database,
    root: &Path,
    patterns: &[&str],
) -> Result<IndexResult> {
    let exclude = ExcludeGlobs::from_globs(
        &patterns
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let filter = WalkFilter {
        exclude,
        respect_gitignore: true,
        ..Default::default()
    };
    index_directory(
        db,
        root,
        true,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &filter,
    )
}

#[test]
fn exclude_prunes_dir_and_matches_files() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::write(proj.join("a.py"), "def foo():\n    return 1\n").unwrap();
    std::fs::create_dir(proj.join("Pods")).unwrap();
    std::fs::write(proj.join("Pods/b.py"), "def bar():\n    return 2\n").unwrap();
    // A non-source file inside the excluded dir: if the dir were merely
    // file-filtered (walked + each file dropped) rather than PRUNED, the walk
    // would tally this as unsupported. Pruning means it's never visited.
    std::fs::write(proj.join("Pods/readme.xyz"), "junk\n").unwrap();
    std::fs::write(proj.join("notes.md"), "# Title\n\nbody\n").unwrap();

    let r = index_dir_excluding(&db, &proj, &["Pods/**", "**/*.md"]).unwrap();

    assert_eq!(r.files_indexed, 1, "only a.py should be indexed");
    assert_eq!(
        r.files_unsupported, 0,
        "Pods/ must be pruned (not descended), so its .xyz is never tallied"
    );
    let files = db.all_files().unwrap();
    assert!(files.iter().any(|f| f.ends_with("a.py")));
    assert!(
        !files.iter().any(|f| f.contains("Pods")),
        "pruned dir must contribute no files"
    );
    assert!(!files.iter().any(|f| f.ends_with(".md")));
}

#[test]
fn exclude_matching_everything_errors_with_exclude_hint() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::write(proj.join("a.py"), "def foo():\n    return 1\n").unwrap();
    index_dir(&db, &proj, false).unwrap();

    // A valid glob that happens to match every source file empties the walk.
    // Non-force must refuse AND point at the exclude, not just "wrong root".
    let filter = WalkFilter {
        exclude: ExcludeGlobs::from_globs(&["**/*.py".to_string()]).unwrap(),
        respect_gitignore: true,
        ..Default::default()
    };
    let res = index_directory(
        &db,
        &proj,
        false,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &filter,
    );
    let err = format!("{:#}", res.unwrap_err());
    assert!(err.contains("[index] exclude"), "hint missing: {err}");
    assert!(!db.all_files().unwrap().is_empty(), "index left untouched");
}

#[test]
fn exclude_empty_is_noop() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::write(proj.join("a.py"), "def foo():\n    return 1\n").unwrap();
    std::fs::create_dir(proj.join("Pods")).unwrap();
    std::fs::write(proj.join("Pods/b.py"), "def bar():\n    return 2\n").unwrap();
    std::fs::write(proj.join("notes.md"), "# Title\n\nbody\n").unwrap();

    let r = index_dir_excluding(&db, &proj, &[]).unwrap();

    // No globs → everything supported is indexed (both .py + the .md doc).
    assert_eq!(r.files_indexed, 3);
    let files = db.all_files().unwrap();
    assert!(files.iter().any(|f| f.contains("Pods")));
    assert!(files.iter().any(|f| f.ends_with(".md")));
}

fn index_dir_filtered(
    db: &cartog_db::Database,
    root: &Path,
    filter: &WalkFilter,
) -> Result<IndexResult> {
    index_directory(
        db,
        root,
        true,
        false,
        None,
        None,
        RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        filter,
    )
}

#[test]
fn gitignore_nested_is_honored() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::create_dir_all(proj.join("sub/ignored")).unwrap();
    // Nested .gitignore (not at the root) — e.g. a CocoaPods Pods/ dir.
    std::fs::write(proj.join("sub/.gitignore"), "ignored/\n").unwrap();
    std::fs::write(proj.join("sub/keep.py"), "def keep():\n    pass\n").unwrap();
    std::fs::write(proj.join("sub/ignored/skip.py"), "def skip():\n    pass\n").unwrap();

    let r = index_dir_filtered(&db, &proj, &WalkFilter::unrestricted()).unwrap();

    let files = db.all_files().unwrap();
    assert!(files.iter().any(|f| f.ends_with("keep.py")));
    assert!(
        !files.iter().any(|f| f.contains("ignored")),
        "nested .gitignore must hide sub/ignored/; got {files:?}"
    );
    assert_eq!(r.files_indexed, 1);
}

#[test]
fn gitignore_ancestor_above_root_is_not_applied() {
    // A .gitignore ABOVE the indexed root (parent dir, or $HOME/.gitignore)
    // must NOT prune files inside the root — else indexing a subdir of a
    // repo silently drops files matched by the repo-root or $HOME ignore.
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    // Ignore file in the PARENT of what we index.
    std::fs::write(tmp.path().join(".gitignore"), "*.log\nsecret/\n").unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::create_dir(proj.join("secret")).unwrap();
    std::fs::write(proj.join("app.py"), "def app():\n    pass\n").unwrap();
    std::fs::write(proj.join("debug.log"), "noise\n").unwrap();
    std::fs::write(proj.join("secret/s.py"), "def s():\n    pass\n").unwrap();

    index_dir_filtered(&db, &proj, &WalkFilter::unrestricted()).unwrap();

    // The ancestor's `*.log`/`secret/` rules must not reach into proj/.
    let files = db.all_files().unwrap();
    assert!(files.iter().any(|f| f.ends_with("app.py")));
    assert!(
        files.iter().any(|f| f.ends_with("secret/s.py")),
        "ancestor .gitignore must NOT prune in-root secret/; got {files:?}"
    );
}

#[test]
fn gitignore_floor_wins_over_unignore() {
    // Even if .gitignore `!`-unignores node_modules, the hardcoded floor
    // still skips it.
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::write(proj.join(".gitignore"), "!node_modules/\n").unwrap();
    std::fs::create_dir(proj.join("node_modules")).unwrap();
    std::fs::write(proj.join("node_modules/x.py"), "def x():\n    pass\n").unwrap();
    std::fs::write(proj.join("app.py"), "def app():\n    pass\n").unwrap();

    index_dir_filtered(&db, &proj, &WalkFilter::unrestricted()).unwrap();

    let files = db.all_files().unwrap();
    assert!(files.iter().any(|f| f.ends_with("app.py")));
    assert!(!files.iter().any(|f| f.contains("node_modules")));
}

#[test]
fn gitignore_honored_without_git_dir() {
    // No `.git` anywhere — `.gitignore` is still applied (require_git=false).
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::create_dir(proj.join("gen")).unwrap();
    std::fs::write(proj.join(".gitignore"), "gen/\n").unwrap();
    std::fs::write(proj.join("gen/derived.py"), "def d():\n    pass\n").unwrap();
    std::fs::write(proj.join("src.py"), "def s():\n    pass\n").unwrap();

    index_dir_filtered(&db, &proj, &WalkFilter::unrestricted()).unwrap();

    let files = db.all_files().unwrap();
    assert!(files.iter().any(|f| f.ends_with("src.py")));
    assert!(!files.iter().any(|f| f.contains("gen")));
}

#[test]
fn respect_gitignore_false_indexes_ignored_but_keeps_floor() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::create_dir(proj.join("gen")).unwrap();
    std::fs::write(proj.join(".gitignore"), "gen/\n").unwrap();
    std::fs::write(proj.join("gen/derived.py"), "def d():\n    pass\n").unwrap();
    std::fs::create_dir(proj.join("node_modules")).unwrap();
    std::fs::write(proj.join("node_modules/x.py"), "def x():\n    pass\n").unwrap();

    let filter = WalkFilter {
        exclude: ExcludeGlobs::empty(),
        respect_gitignore: false,
        ..Default::default()
    };
    index_dir_filtered(&db, &proj, &filter).unwrap();

    let files = db.all_files().unwrap();
    // Opt-out indexes the gitignored file...
    assert!(
        files.iter().any(|f| f.contains("gen")),
        "respect_gitignore=false should index gen/; got {files:?}"
    );
    // ...but the floor still skips node_modules.
    assert!(!files.iter().any(|f| f.contains("node_modules")));
}

#[test]
fn cartogignore_is_honored() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    std::fs::create_dir(proj.join("secret")).unwrap();
    // .cartogignore uses gitignore syntax and is always honored.
    std::fs::write(proj.join(".cartogignore"), "secret/\n").unwrap();
    std::fs::write(proj.join("secret/s.py"), "def s():\n    pass\n").unwrap();
    std::fs::write(proj.join("ok.py"), "def ok():\n    pass\n").unwrap();

    index_dir_filtered(&db, &proj, &WalkFilter::unrestricted()).unwrap();

    let files = db.all_files().unwrap();
    assert!(files.iter().any(|f| f.ends_with("ok.py")));
    assert!(!files.iter().any(|f| f.contains("secret")));
}

#[test]
fn gitignore_and_exclude_compose() {
    let db = cartog_db::Database::open_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = visible_dir(&tmp, "proj");
    for d in ["a", "b", "c"] {
        std::fs::create_dir(proj.join(d)).unwrap();
        std::fs::write(proj.join(d).join("f.py"), "def f():\n    pass\n").unwrap();
    }
    std::fs::write(proj.join(".gitignore"), "a/\n").unwrap(); // a/ via gitignore

    let filter = WalkFilter {
        exclude: ExcludeGlobs::from_globs(&["b/**".to_string()]).unwrap(), // b/ via exclude
        respect_gitignore: true,
        ..Default::default()
    };
    index_dir_filtered(&db, &proj, &filter).unwrap();

    // Stored paths are repo-root-relative (e.g. "c/f.py").
    let files = db.all_files().unwrap();
    assert!(!files.iter().any(|f| f.starts_with("a/")), "a/ gitignored");
    assert!(!files.iter().any(|f| f.starts_with("b/")), "b/ excluded");
    assert!(files.iter().any(|f| f.starts_with("c/")), "c/ kept");
}

#[test]
fn test_file_hash_deterministic() {
    let h1 = file_hash("def foo(): pass");
    let h2 = file_hash("def foo(): pass");
    assert_eq!(h1, h2);
}

#[test]
fn test_file_hash_different_content() {
    let h1 = file_hash("def foo(): pass");
    let h2 = file_hash("def bar(): pass");
    assert_ne!(h1, h2);
}

#[test]
fn extract_with_cached_returns_none_for_unregistered_language() {
    assert!(extract_with_cached("klingon", "irrelevant", "a.kl").is_none());
}

#[test]
fn index_summary_reports_file_symbol_and_edge_counts() {
    let r = IndexResult {
        files_indexed: 3,
        files_skipped: 1,
        symbols_added: 12,
        edges_added: 20,
        edges_resolved: 18,
        ..Default::default()
    };
    let s = render_index_summary(&r);
    assert!(s.contains("Indexed 3 files (1 skipped, 0 removed)"));
    assert!(s.contains("12 symbols"));
    assert!(s.contains("20 edges (18 resolved)"));
}

#[test]
fn index_summary_shows_detail_for_removal_only_delta() {
    // A pass that only removes symbols (no new/modified/unchanged) must
    // still report the removed count, not a bare "0 symbols".
    let r = IndexResult {
        files_indexed: 1,
        symbols_removed: 4,
        ..Default::default()
    };
    let s = render_index_summary(&r);
    assert!(
        s.contains("4 removed"),
        "removal-only delta must surface the removed count: {s}"
    );
}

#[test]
fn index_summary_breaks_out_lsp_resolution_when_present() {
    let r = IndexResult {
        files_indexed: 1,
        symbols_added: 5,
        edges_added: 10,
        edges_resolved: 6,
        edges_lsp_resolved: 3,
        edges_marked_external: 1,
        ..Default::default()
    };
    let s = render_index_summary(&r);
    assert!(s.contains("9 resolved"), "6 heuristic + 3 LSP = 9");
    assert!(s.contains("6 heuristic + 3 LSP"));
    assert!(s.contains("1 external"));
}

#[test]
fn index_summary_lists_unsupported_languages() {
    let r = IndexResult {
        files_indexed: 2,
        files_unsupported: 4,
        unsupported_by_ext: vec![("kt".into(), 3), ("cpp".into(), 1)],
        ..Default::default()
    };
    let s = render_index_summary(&r);
    assert!(s.contains("4 files in unsupported languages"));
    assert!(s.contains("3 .kt"));
}

#[test]
fn extract_with_cached_extracts_for_known_language() {
    let result = extract_with_cached("python", "def foo():\n    pass\n", "a.py")
        .expect("python is registered")
        .expect("valid source extracts");
    assert!(
        result.symbols.iter().any(|s| s.name == "foo"),
        "expected `foo` among extracted symbols"
    );
}

#[test]
fn test_is_ignored_directories() {
    let ignored_dirs = [
        ".git",
        "node_modules",
        "__pycache__",
        "target",
        "dist",
        "build",
        ".venv",
        "var",
        "builds",
    ];
    for name in ignored_dirs {
        assert!(is_ignored(name, true, 1), "{name} should be ignored");
    }
    for name in ["src", "lib", "tests", "docs"] {
        assert!(!is_ignored(name, true, 1), "{name} should NOT be ignored");
    }
    // Files are never ignored by the floor.
    assert!(!is_ignored("node_modules", false, 1));
}

#[test]
fn test_var_and_builds_not_ignored_when_nested() {
    // "var"/"builds" are floored only at depth 1 (project root); a nested
    // `src/var` (depth 2) is valid application code.
    assert!(is_ignored("var", true, 1));
    assert!(is_ignored("builds", true, 1));
    assert!(!is_ignored("var", true, 2), "src/var should NOT be ignored");
    assert!(
        !is_ignored("builds", true, 2),
        "src/builds should NOT be ignored"
    );
}

#[test]
fn db_sidecars_are_recognized() {
    assert!(is_db_sidecar(".cartog.db"));
    assert!(is_db_sidecar(".cartog.db-wal"));
    assert!(is_db_sidecar(".cartog.db-shm"));
    assert!(is_db_sidecar("sub/db.sqlite"));
    assert!(is_db_sidecar("db.sqlite-wal"));
    assert!(!is_db_sidecar("main.rs"));
    assert!(!is_db_sidecar("app.dart"));
}

#[test]
fn unsupported_files_are_counted_not_silently_dropped() {
    use cartog_db::Database;
    // TempDir names start with '.', which the walker treats as hidden and
    // prunes — nest a non-dot project dir so the walk descends into it.
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("proj");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.join("b.cs"), "class P {}\n").unwrap();
    std::fs::write(dir.join("c.cs"), "class Q {}\n").unwrap();
    std::fs::write(dir.join("d.cpp"), "int main() {}\n").unwrap();
    // cartog's own DB sidecars must NOT count as unsupported languages.
    std::fs::write(dir.join(".cartog.db"), "x").unwrap();
    std::fs::write(dir.join(".cartog.db-wal"), "x").unwrap();

    let db = Database::open_memory().unwrap();
    let r = index_directory(
        &db,
        &dir,
        true,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    assert_eq!(r.files_indexed, 1, "only a.rs is supported");
    assert_eq!(
        r.files_unsupported, 3,
        "2 csharp + 1 cpp, db sidecars excluded"
    );
    // Descending by count, ties broken alphabetically.
    assert_eq!(
        r.unsupported_by_ext,
        vec![("cs".to_string(), 2), ("cpp".to_string(), 1)]
    );
}

#[test]
fn dotted_import_leaf_is_separator_escaped_in_stored_id() {
    // Real-output guard for the symbol-ID escaping fix: a dotted import leaf
    // (os.path) is stored with its separator escaped (os%2Epath), not raw.
    // The parented method (os.path) keeps its raw structural id; the two
    // differ in the kind segment, so this checks escaping is applied in
    // stored output — not a same-kind collision (that's the cartog-core
    // injectivity unit test).
    use cartog_db::Database;

    // TempDir names start with '.', which the walker prunes as hidden — nest
    // a non-dot project dir so the walk descends into it.
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("proj");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("a.py"),
        "import os.path\n\nclass os:\n    def path(self):\n        return 1\n",
    )
    .unwrap();

    let db = Database::open_memory().unwrap();
    index_directory(
        &db,
        &dir,
        true,
        false,
        None,
        None,
        crate::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &crate::WalkFilter::unrestricted(),
    )
    .unwrap();

    let ids: Vec<String> = db
        .outline("a.py")
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();

    assert!(
        ids.contains(&"a.py:import:os%2Epath".to_string()),
        "dotted import leaf must be escaped: {ids:?}"
    );
    assert!(
        ids.contains(&"a.py:method:os.path".to_string()),
        "parented method keeps its raw structural id: {ids:?}"
    );
    // The escaped import id is stored verbatim — the raw dotted form must not
    // appear (that would mean escaping was skipped on the stored path).
    assert!(
        !ids.contains(&"a.py:import:os.path".to_string()),
        "raw (unescaped) dotted import id must not be stored: {ids:?}"
    );
}
