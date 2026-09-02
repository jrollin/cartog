//! Candidate selection for `cartog_search_all`.
//!
//! This logic is **duplicated** from
//! `crates/cartog/src/commands/search_all.rs` — no crate can host a shared
//! version, since `cartog-registry` carries no `cartog-db` dependency by design
//! and `cartog-db` depends only on `cartog-core`. These tests mirror the
//! binary crate's so a change on one side that is not made on the other shows
//! up as a failure here rather than as the CLI and the MCP tool quietly
//! answering differently.

use std::path::{Path, PathBuf};

use cartog_registry::{Markers, ProjectRow};

use crate::tools::search::select_fanout_candidates;

fn row(name: &str, root: &str, symbols: Option<u32>, langs: &[&str]) -> ProjectRow {
    ProjectRow {
        id: format!("serve-{name}"),
        db_path: PathBuf::from(format!("{root}/.cartog/db.sqlite")),
        root: PathBuf::from(root),
        name: name.to_string(),
        declared_name: None,
        description: None,
        languages: langs.iter().map(|l| ((*l).to_string(), 10)).collect(),
        schema_version: Some(8),
        file_count: Some(10),
        symbol_count: symbols,
        edge_count: None,
        resolved_count: None,
        embedding_count: None,
        embed_provider: None,
        embed_model: None,
        embed_dim: None,
        last_indexed: None,
        last_seen: 0,
        markers: Markers::default(),
    }
}

#[test]
fn the_callers_own_project_is_never_queried() {
    // `cartog_search` already covers the current project; including it would
    // double-report every hit.
    let mine = PathBuf::from("/w/a/.cartog/db.sqlite");
    let rows = vec![
        row("a", "/w/a", Some(5), &["rust"]),
        row("b", "/w/b", Some(5), &["rust"]),
    ];

    let (kept, _) = select_fanout_candidates(rows, &mine, None, None, 10);

    assert_eq!(
        kept.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["b"]
    );
}

#[test]
fn a_project_whose_database_is_gone_is_not_queried() {
    let mut gone = row("gone", "/w/gone", Some(5), &["rust"]);
    gone.markers = Markers {
        missing: true,
        ..Markers::default()
    };
    let rows = vec![gone, row("here", "/w/here", Some(5), &["rust"])];

    let (kept, _) = select_fanout_candidates(rows, Path::new("/w/x/db"), None, None, 10);

    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "here");
}

#[test]
fn under_keeps_only_projects_inside_that_subtree() {
    let rows = vec![
        row("in", "/w/team/in", Some(5), &["rust"]),
        row("out", "/other/out", Some(5), &["rust"]),
    ];

    let (kept, _) = select_fanout_candidates(rows, Path::new("/none"), Some("/w/team"), None, 10);

    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "in");
}

#[test]
fn lang_keeps_only_projects_that_indexed_that_language_ignoring_case() {
    let rows = vec![
        row("rb", "/w/rb", Some(5), &["Ruby", "markdown"]),
        row("ts", "/w/ts", Some(5), &["typescript"]),
    ];

    let (kept, _) = select_fanout_candidates(rows, Path::new("/none"), None, Some("ruby"), 10);

    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "rb");
}

#[test]
fn under_and_lang_compose_as_an_and() {
    let rows = vec![
        row("both", "/w/team/both", Some(5), &["ruby"]),
        row("wrong-lang", "/w/team/ts", Some(5), &["typescript"]),
        row("wrong-path", "/other/rb", Some(5), &["ruby"]),
    ];

    let (kept, _) =
        select_fanout_candidates(rows, Path::new("/none"), Some("/w/team"), Some("ruby"), 10);

    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "both");
}

#[test]
fn the_cap_keeps_the_largest_projects_and_reports_what_it_dropped() {
    // A silent truncation would let an agent conclude "that's everything".
    let rows = vec![
        row("small", "/w/small", Some(1), &["rust"]),
        row("big", "/w/big", Some(9000), &["rust"]),
        row("mid", "/w/mid", Some(50), &["rust"]),
    ];

    let (kept, elided) = select_fanout_candidates(rows, Path::new("/none"), None, None, 2);

    assert_eq!(
        kept.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["big", "mid"]
    );
    assert_eq!(elided, 1, "the dropped project must be counted, not hidden");
}

#[test]
fn an_unmeasurable_project_stays_eligible_but_sorts_last() {
    // A name lookup does not need a readable schema.
    let rows = vec![
        row("unknown", "/w/unknown", None, &[]),
        row("known", "/w/known", Some(10), &["rust"]),
    ];

    let (kept, _) = select_fanout_candidates(rows, Path::new("/none"), None, None, 10);

    assert_eq!(
        kept.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["known", "unknown"]
    );
}

#[test]
fn the_project_cap_is_clamped_to_a_sane_range() {
    let rows: Vec<ProjectRow> = (0..60)
        .map(|i| row(&format!("p{i}"), &format!("/w/p{i}"), Some(i), &["rust"]))
        .collect();

    // 0 would otherwise mean "query nothing", silently returning no matches.
    let (none_asked, _) = select_fanout_candidates(rows.clone(), Path::new("/none"), None, None, 0);
    assert_eq!(
        none_asked.len(),
        1,
        "a cap of 0 must still query one project"
    );

    let (huge, elided) = select_fanout_candidates(rows, Path::new("/none"), None, None, usize::MAX);
    assert_eq!(huge.len(), 50, "the cap must be bounded above");
    assert_eq!(elided, 10);
}

#[test]
fn under_expands_a_leading_tilde() {
    // An agent may pass `~/work` literally. Unexpanded, `starts_with` matched
    // nothing and the fan-out silently returned zero projects.
    let home = std::env::var("HOME").expect("HOME must be set");
    let rows = vec![
        row("in", &format!("{home}/work/in"), Some(5), &["rust"]),
        row("out", "/elsewhere/out", Some(5), &["rust"]),
    ];

    let (kept, _) = select_fanout_candidates(rows, Path::new("/none"), Some("~/work"), None, 10);

    assert_eq!(
        kept.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["in"],
        "a tilde in `under` must expand to $HOME"
    );
}
