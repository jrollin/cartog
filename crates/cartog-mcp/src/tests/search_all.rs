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

use crate::tools::search::{canonical_path, select_fanout_candidates};

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

// ── the renderer must not turn an unsearchable fan-out into "no match" ──

use crate::tools::search::render_search_all;
use crate::types::{ProjectMatches, SearchAllResult};

fn result(
    projects: Vec<ProjectMatches>,
    unreadable: Vec<String>,
    queried: usize,
    elided_by_cap: usize,
) -> SearchAllResult {
    SearchAllResult {
        registry_available: true,
        projects,
        queried,
        unreadable,
        elided_by_cap,
    }
}

#[test]
fn a_search_where_every_candidate_was_unreadable_does_not_claim_no_match() {
    // "No symbols matching X" implies the projects were searched. When none
    // could be read, that is a false negative — an agent would conclude the
    // symbol does not exist in any other project.
    let r = result(
        Vec::new(),
        vec!["legacy-service: schema_version mismatch: expects 8, DB has 3".to_string()],
        1,
        0,
    );

    let out = render_search_all(&r, "CreateOrder");

    assert!(
        !out.contains("No symbols matching"),
        "must not claim a genuine no-match, got: {out}"
    );
    assert!(
        out.contains("legacy-service") && out.contains("schema_version"),
        "the reason must survive an empty match list, got: {out}"
    );
}

/// The structured half must fit the cap, not just the bare array.
///
/// `success_result` never re-clamps `structuredContent`, so the element trim is
/// its only bound — and this payload nests one level deeper than the other list
/// tools (`projects[]` each holding `symbols[]`) plus four wrapper fields, so a
/// bare-array budget overshot the cap by ~4 KB. Mirrors the same assertion on
/// `cartog_list_projects`.
#[test]
fn a_maximal_fan_out_fits_both_halves_of_the_envelope() {
    let long = "q".repeat(120);
    let projects: Vec<ProjectMatches> = (0..400)
        .map(|i| ProjectMatches {
            name: format!("project-{i}-{long}"),
            root: format!("/home/u/work/project-{i}/{long}"),
            db_path: format!("/home/u/work/project-{i}/.cartog/db.sqlite"),
            description: Some(long.clone()),
            symbols: Vec::new(),
        })
        .collect();
    let total = projects.len();

    // 50 root causes is the documented worst case, and they share the envelope.
    let unreadable = (0..50)
        .map(|i| format!("project-{i}: {long}"))
        .collect::<Vec<_>>();

    let envelope_budget = crate::mcp_list_budget().saturating_sub(crate::mcp_list_budget() / 8);
    let (kept, omitted) = crate::fit_to_budget(projects, envelope_budget);
    assert!(
        omitted > 0,
        "precondition: this fan-out must exceed the budget"
    );
    assert_eq!(kept.len() + omitted, total, "nothing may be lost silently");

    let trimmed = result(kept, unreadable, total, 0);
    let structured = serde_json::to_string_pretty(&trimmed).unwrap();
    assert!(
        structured.len() <= crate::mcp_max_bytes(),
        "structuredContent must stay under the response cap, got {} bytes",
        structured.len()
    );
}

/// Selecting no candidate is not the same as searching some and finding
/// nothing. "No symbols matching X" implies a search happened; when a filter
/// excluded every project, an agent reads that as "the symbol exists nowhere
/// else" and stops looking. The CLI already distinguishes the two.
#[test]
fn a_fan_out_that_selected_no_candidate_does_not_claim_no_match() {
    let r = result(Vec::new(), Vec::new(), 0, 0);

    let out = render_search_all(&r, "CreateOrder");

    assert!(
        !out.contains("No symbols matching"),
        "must not imply the other projects were searched, got: {out}"
    );
    assert!(
        out.contains("no other indexed project") || out.contains("No other indexed project"),
        "must say the selection was empty, got: {out}"
    );
}

/// The `~` contract must match the CLI's `config::expand_tilde`.
///
/// The two expanders are deliberately duplicated (no crate can host the shared
/// logic), and they drifted: this side expanded a bare `~`, the CLI did not, so
/// `--under '~'` searched everything here and nothing there. A comment claiming
/// they "mirror" each other did not stop it, so the contract is asserted.
#[test]
fn a_bare_tilde_expands_and_a_user_tilde_does_not() {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .expect("HOME must be set");

    // A bare `~` and `~/x` both resolve under $HOME.
    for (input, suffix) in [("~", ""), ("~/work", "work")] {
        let got = canonical_path(std::path::Path::new(input));
        let want = std::path::PathBuf::from(&home).join(suffix);
        // `canonical_path` canonicalizes, so compare against the same treatment.
        let want = want.canonicalize().unwrap_or(want);
        assert_eq!(got, want, "input: {input}");
    }

    // `~user` is another account's home: neither surface guesses at it.
    let got = canonical_path(std::path::Path::new("~other/work"));
    assert_eq!(
        got,
        std::path::PathBuf::from("~other/work"),
        "`~user` must be left alone"
    );
}

/// The cap can never elide *every* candidate, which is why the zero-candidate
/// message names only the filter.
///
/// `cap = max_projects.clamp(1, 50)` then `kept.truncate(cap)`, so an elision
/// means `kept.len() > cap >= 1` and at least one project was queried. A
/// previous version of this file asserted the opposite state — zero queried
/// with a non-zero elision — which `select_fanout_candidates` cannot emit; the
/// assertion passed only because an unrelated trailing notice satisfied it.
#[test]
fn the_cap_cannot_elide_every_candidate() {
    let rows: Vec<ProjectRow> = (0..7)
        .map(|i| row(&format!("p{i}"), &format!("/w/p{i}"), Some(100), &["rust"]))
        .collect();
    let total = rows.len();

    // Across every cap the clamp can yield, including 0 and an over-max value.
    for requested in [0usize, 1, 3, 7, 50, 999] {
        let (kept, elided) = select_fanout_candidates(
            rows.clone(),
            std::path::Path::new("/nonexistent/current/db.sqlite"),
            None,
            None,
            requested,
        );
        assert_eq!(
            kept.len() + elided,
            total,
            "requested {requested}: nothing lost"
        );
        if elided > 0 {
            assert!(
                !kept.is_empty(),
                "requested {requested}: an elision must leave a candidate queried"
            );
        }
    }
}

#[test]
fn an_empty_result_still_reports_projects_elided_by_the_cap() {
    let r = result(Vec::new(), Vec::new(), 2, 7);

    let out = render_search_all(&r, "Widget");

    assert!(
        out.contains('7') && out.contains("max_projects"),
        "the elision notice must survive an empty match list, got: {out}"
    );
}

#[test]
fn a_readable_project_with_no_hit_still_reports_a_genuine_no_match() {
    // The complement: don't over-correct into never saying "no match".
    let r = result(Vec::new(), Vec::new(), 3, 0);

    assert!(
        render_search_all(&r, "Nope").contains("No symbols matching"),
        "a real no-match must still read as one"
    );
}
