//! `cartog search --all` — one symbol query, fanned out across the machine's
//! other indexed projects.
//!
//! **Fan out and group; never consolidate.** The registry supplies the
//! candidate database paths, each is opened **read-only** and queried on its
//! own, and results stay grouped under the project they came from. Three things
//! this deliberately does not do:
//!
//! - **No merged database.** Merging graphs is a documented non-goal: it adds a
//!   second staleness surface and breaks per-repo `.cartog/` deletability,
//!   gitignore and per-project remote sync. Every hit here is read live from the
//!   project that owns it.
//! - **No merged ranking.** `in_degree` centrality is per-graph and dominates
//!   ordering, so a flat cross-project list cannot be ranked defensibly without
//!   a ranking benchmark that does not exist. Grouping sidesteps the question
//!   rather than guessing at it.
//! - **No writes.** A registry row grants discovery, not write access. The
//!   read-only open is the enforcement, not a convention.
//!
//! Exact-symbol search federates precisely because names match or they do not,
//! with no score to normalize. Semantic search does not share that property and
//! is not built here.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::Serialize;

use cartog_core::{Compact, Symbol};
use cartog_db::{Database, MAX_SEARCH_LIMIT};
use cartog_registry::ProjectRow;

use crate::cli::SymbolKindFilter;

/// Databases queried when the user names no filter.
///
/// A fan-out is cheap (a read-only open plus one indexed lookup — measured at
/// ~2ms per project), but it is not free and it is unbounded by nature: the
/// registry grows with every project on the machine. The cap keeps a default
/// `--all` predictable; `--max-projects` raises it, and the response says when
/// the cap elided anything so a missing project is never silent.
const DEFAULT_MAX_PROJECTS: usize = 10;

/// Upper bound on `--max-projects`, mirroring the MCP tool's clamp.
const MAX_FANOUT_PROJECTS: usize = 50;

/// How a project's databases are chosen for one federated query.
#[derive(Debug, Clone, Default)]
pub struct FanoutFilter {
    /// Keep only projects whose root is inside this directory.
    pub under: Option<PathBuf>,
    /// Keep only projects that indexed this language.
    pub lang: Option<String>,
    pub max_projects: Option<usize>,
}

/// How the result is rendered, grouped so the fan-out signature stays readable.
#[derive(Debug, Clone, Copy)]
pub struct OutputOpts {
    pub json: bool,
    pub compact: bool,
    pub token_budget: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct FederatedResults {
    /// One entry per project that answered, most-populous project first.
    /// Ranking is **within** a project only.
    projects: Vec<ProjectHits>,
    /// Projects whose database could not be read, each with the reason. Named
    /// rather than dropped: silently omitting one would read as "no matches
    /// there".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unreadable: Vec<UnreadableJson>,
    queried: usize,
    /// Candidates left unqueried by the cap, so a partial answer says so.
    #[serde(skip_serializing_if = "super::shared::is_zero")]
    elided_by_cap: usize,
    total_matches: usize,
}

/// A project that matched the filter but could not be queried.
#[derive(Debug, Serialize)]
struct UnreadableJson {
    name: String,
    /// Why the read failed. A schema drift, a corrupt file, an `EACCES` on the
    /// `.cartog/` directory and a `SQLITE_BUSY` are all different problems with
    /// different fixes, so collapsing them to one guessed cause sent the reader
    /// after the wrong one.
    reason: String,
}

#[derive(Debug, Serialize)]
struct ProjectHits {
    name: String,
    root: String,
    db_path: String,
    /// Repository-authored text: data on every surface, never instructions.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    symbols: Vec<Symbol>,
}

/// Run `query` against every project the filter selects.
pub fn cmd_search_all(
    current_db: &Path,
    query: &str,
    kind: Option<SymbolKindFilter>,
    limit: u32,
    filter: &FanoutFilter,
    out: OutputOpts,
) -> Result<()> {
    let OutputOpts {
        json,
        compact,
        token_budget,
    } = out;
    let listing = cartog_registry::list_projects(cartog_db::CURRENT_SCHEMA_VERSION);
    if !listing.available {
        bail!(
            "no project registry on this machine, so there are no other projects to search \
             (CARTOG_REGISTRY is disabled, or nothing has been indexed yet)"
        );
    }

    let (candidates, elided_by_cap) = select_candidates(listing.projects, current_db, filter);
    if candidates.is_empty() {
        // Not an error: an empty selection is a real answer, and the filter the
        // user typed is the thing to report back.
        let empty = FederatedResults {
            projects: Vec::new(),
            unreadable: Vec::new(),
            queried: 0,
            elided_by_cap,
            total_matches: 0,
        };
        return super::shared::output(&empty, json, token_budget, |e| {
            if e.elided_by_cap > 0 {
                return format!(
                    "{} project(s) matched {} but none were queried — raise --max-projects.\n",
                    e.elided_by_cap,
                    describe(filter),
                );
            }
            format!("No other indexed project matches {}.\n", describe(filter))
        });
    }

    let kind_filter = match kind {
        Some(SymbolKindFilter::All) | None => None,
        Some(k) => Some(cartog_core::SymbolKind::from(k)),
    };
    // Same ceiling as the single-project search: a fan-out must not become a
    // way to ask for more rows per project than `cartog search` allows.
    let limit = limit.min(MAX_SEARCH_LIMIT);

    let mut projects = Vec::new();
    let mut unreadable = Vec::new();
    let queried = candidates.len();
    for row in candidates {
        match query_one(&row, query, kind_filter, limit, compact) {
            Ok(symbols) if symbols.is_empty() => {}
            Ok(symbols) => projects.push(ProjectHits {
                name: row.display_name().to_string(),
                root: row.root.display().to_string(),
                db_path: row.db_path.display().to_string(),
                description: row.description.as_ref().map(|d| d.text.clone()),
                symbols,
            }),
            Err(e) => unreadable.push(UnreadableJson {
                name: row.display_name().to_string(),
                // Root cause, not the wrapper: `open_readonly`'s outer message
                // says little, and the schema-drift/IO detail is what the
                // reader needs.
                reason: e.root_cause().to_string(),
            }),
        }
    }

    let total_matches = projects.iter().map(|p| p.symbols.len()).sum();
    let results = FederatedResults {
        projects,
        unreadable,
        queried,
        elided_by_cap,
        total_matches,
    };

    let query = query.to_string();
    super::shared::output(&results, json, token_budget, |r| render(r, &query))
}

/// Choose which projects to query, and count what the cap left out.
///
/// **Duplicated** as `select_fanout_candidates` in
/// `crates/cartog-mcp/src/tools/search.rs`, because no existing crate can host
/// the shared version: `cartog-registry` deliberately carries no `cartog-db`
/// dependency (so a graph-schema bump never forces a registry migration) and
/// `cartog-db` depends only on `cartog-core`. A behaviour change here needs the
/// same change there, or the CLI and the MCP tool answer differently.
///
/// Excludes the project the caller is already in: `cartog search` without
/// `--all` covers that, and listing it twice would double-report every hit.
/// Ordered most-symbols-first so the cap keeps the substantial projects rather
/// than whichever the registry happened to return first.
fn select_candidates(
    rows: Vec<ProjectRow>,
    current_db: &Path,
    filter: &FanoutFilter,
) -> (Vec<ProjectRow>, usize) {
    let current = canonical(current_db);
    let mut kept: Vec<ProjectRow> = rows
        .into_iter()
        .filter(|r| canonical(&r.db_path) != current)
        .filter(|r| !r.markers.missing)
        .filter(|r| matches_filter(r, filter))
        .collect();

    // Descending by symbol count; an unmeasurable project sorts last but stays
    // eligible, since a name lookup does not need a readable schema.
    kept.sort_by(|a, b| {
        b.symbol_count
            .unwrap_or(0)
            .cmp(&a.symbol_count.unwrap_or(0))
            .then_with(|| a.display_name().cmp(b.display_name()))
    });

    // Clamped to match `select_fanout_candidates` in cartog-mcp: an unclamped 0
    // truncates every candidate away, and the caller then reports "nothing
    // matched the filter" when something did match and was elided.
    let cap = filter
        .max_projects
        .unwrap_or(DEFAULT_MAX_PROJECTS)
        .clamp(1, MAX_FANOUT_PROJECTS);
    let elided = kept.len().saturating_sub(cap);
    kept.truncate(cap);
    (kept, elided)
}

/// Whether one row passes the `--under` / `--lang` filters.
///
/// `--under` compares canonicalized paths so `~/work` and `~/work/` behave the
/// same and a symlinked root cannot slip past a prefix test.
fn matches_filter(row: &ProjectRow, filter: &FanoutFilter) -> bool {
    if let Some(under) = &filter.under {
        if !canonical(&row.root).starts_with(canonical(under)) {
            return false;
        }
    }
    if let Some(lang) = &filter.lang {
        let wanted = lang.to_ascii_lowercase();
        if !row
            .languages
            .iter()
            .any(|(l, _)| l.eq_ignore_ascii_case(&wanted))
        {
            return false;
        }
    }
    true
}

/// Query one project's database, read-only.
///
/// Read-only is the write-access boundary, not a convention: a registry row
/// grants discovery only. `open_readonly` also refuses a schema this binary
/// does not own, which is the right outcome — a drifted graph's rows cannot be
/// trusted, so the project is reported unreadable rather than half-answered.
fn query_one(
    row: &ProjectRow,
    query: &str,
    kind: Option<cartog_core::SymbolKind>,
    limit: u32,
    compact: bool,
) -> Result<Vec<Symbol>> {
    let db = Database::open_readonly(&row.db_path)?;
    let mut symbols = db.search(query, kind, None, limit)?;
    if compact {
        symbols.compact_in_place();
    }
    Ok(symbols)
}

/// `canonicalize` when the path exists, else the path as given — so a filter
/// still behaves sensibly for a project whose database has been removed.
fn canonical(p: &Path) -> PathBuf {
    // Expand `~` first: a quoted or config-sourced `~/work` reaches here
    // literally (the shell only expands it unquoted), and `canonicalize` leaves
    // it alone — so the `starts_with` test would match nothing and the fan-out
    // would silently return zero projects. `backfill.rs` and `[database] path`
    // expand the same way.
    let expanded = crate::config::expand_tilde(p.to_path_buf());
    expanded.canonicalize().unwrap_or(expanded)
}

/// The active filter in words, for a "nothing matched" message that names what
/// was actually applied rather than just reporting emptiness.
fn describe(filter: &FanoutFilter) -> String {
    let mut parts = Vec::new();
    if let Some(u) = &filter.under {
        parts.push(format!("under {}", u.display()));
    }
    if let Some(l) = &filter.lang {
        parts.push(format!("language {l}"));
    }
    if parts.is_empty() {
        return "this machine's registry".to_string();
    }
    parts.join(" and ")
}

fn render(r: &FederatedResults, query: &str) -> String {
    if r.projects.is_empty() {
        return format!(
            "No symbols matching '{query}' in {} other project{}.\n",
            r.queried,
            if r.queried == 1 { "" } else { "s" },
        );
    }

    let mut out = format!(
        "{} match{} for '{query}' across {} of {} project{}:\n",
        r.total_matches,
        if r.total_matches == 1 { "" } else { "es" },
        r.projects.len(),
        r.queried,
        if r.queried == 1 { "" } else { "s" },
    );

    for p in &r.projects {
        out.push('\n');
        out.push_str(&format!("{} ({})\n", p.name, p.root));
        if let Some(d) = &p.description {
            out.push_str(&format!("  {d}\n"));
        }
        for s in &p.symbols {
            out.push_str(&format!(
                "  {:<28} {}:{}\n",
                s.name, s.file_path, s.start_line
            ));
        }
        // The actionable field: this is what the reader passes to --db next.
        out.push_str(&format!("  --db {}\n", p.db_path));
    }

    if !r.unreadable.is_empty() {
        out.push_str(&format!(
            "\n{} project{} could not be read:\n",
            r.unreadable.len(),
            if r.unreadable.len() == 1 { "" } else { "s" },
        ));
        for u in &r.unreadable {
            out.push_str(&format!("  {}: {}\n", u.name, u.reason));
        }
    }
    if r.elided_by_cap > 0 {
        out.push_str(&format!(
            "\n{} more project{} matched but were not queried — raise --max-projects to include them.\n",
            r.elided_by_cap,
            if r.elided_by_cap == 1 { "" } else { "s" },
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartog_registry::{Description, DescriptionSource, Markers};

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
        // `cartog search` already covers the current project; including it here
        // would double-report every hit.
        let mine = PathBuf::from("/w/a/.cartog/db.sqlite");
        let rows = vec![
            row("a", "/w/a", Some(5), &["rust"]),
            row("b", "/w/b", Some(5), &["rust"]),
        ];

        let (kept, _) = select_candidates(rows, &mine, &FanoutFilter::default());

        let names: Vec<&str> = kept.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["b"],
            "the caller's own project must be excluded"
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

        let (kept, _) = select_candidates(rows, Path::new("/w/x/db"), &FanoutFilter::default());

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "here");
    }

    #[test]
    fn under_keeps_only_projects_inside_that_subtree() {
        let rows = vec![
            row("in", "/w/team/in", Some(5), &["rust"]),
            row("out", "/other/out", Some(5), &["rust"]),
        ];
        let filter = FanoutFilter {
            under: Some(PathBuf::from("/w/team")),
            ..FanoutFilter::default()
        };

        let (kept, _) = select_candidates(rows, Path::new("/none"), &filter);

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "in");
    }

    #[test]
    fn lang_keeps_only_projects_that_indexed_that_language() {
        let rows = vec![
            row("rb", "/w/rb", Some(5), &["ruby", "markdown"]),
            row("ts", "/w/ts", Some(5), &["typescript"]),
        ];
        let filter = FanoutFilter {
            lang: Some("ruby".to_string()),
            ..FanoutFilter::default()
        };

        let (kept, _) = select_candidates(rows, Path::new("/none"), &filter);

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "rb");
    }

    #[test]
    fn lang_matching_ignores_case() {
        let rows = vec![row("ts", "/w/ts", Some(5), &["TypeScript"])];
        let filter = FanoutFilter {
            lang: Some("typescript".to_string()),
            ..FanoutFilter::default()
        };

        assert_eq!(
            select_candidates(rows, Path::new("/none"), &filter).0.len(),
            1
        );
    }

    #[test]
    fn under_and_lang_compose_as_an_and() {
        let rows = vec![
            row("both", "/w/team/both", Some(5), &["ruby"]),
            row("wrong-lang", "/w/team/ts", Some(5), &["typescript"]),
            row("wrong-path", "/other/rb", Some(5), &["ruby"]),
        ];
        let filter = FanoutFilter {
            under: Some(PathBuf::from("/w/team")),
            lang: Some("ruby".to_string()),
            max_projects: None,
        };

        let (kept, _) = select_candidates(rows, Path::new("/none"), &filter);

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
        let filter = FanoutFilter {
            max_projects: Some(2),
            ..FanoutFilter::default()
        };

        let (kept, elided) = select_candidates(rows, Path::new("/none"), &filter);

        assert_eq!(
            kept.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["big", "mid"],
            "the cap must keep the most-populous projects"
        );
        assert_eq!(elided, 1, "the dropped project must be counted, not hidden");
    }

    #[test]
    fn an_unmeasurable_project_stays_eligible_but_sorts_last() {
        // A name lookup does not need a readable schema, so a project with no
        // recorded counts must still be searchable.
        let rows = vec![
            row("unknown", "/w/unknown", None, &[]),
            row("known", "/w/known", Some(10), &["rust"]),
        ];

        let (kept, _) = select_candidates(rows, Path::new("/none"), &FanoutFilter::default());

        assert_eq!(
            kept.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["known", "unknown"]
        );
    }

    #[test]
    fn the_project_cap_is_clamped_to_the_same_range_as_the_mcp_tool() {
        // Parity with `select_fanout_candidates` in cartog-mcp. Unclamped, a 0
        // truncated every candidate away and the caller then reported "nothing
        // matched the filter" while --json showed a non-zero elided count.
        let rows: Vec<ProjectRow> = (0..60)
            .map(|i| row(&format!("p{i}"), &format!("/w/p{i}"), Some(i), &["rust"]))
            .collect();

        let zero = FanoutFilter {
            max_projects: Some(0),
            ..FanoutFilter::default()
        };
        let (kept, _) = select_candidates(rows.clone(), Path::new("/none"), &zero);
        assert_eq!(kept.len(), 1, "a cap of 0 must still query one project");

        let huge = FanoutFilter {
            max_projects: Some(usize::MAX),
            ..FanoutFilter::default()
        };
        let (kept, elided) = select_candidates(rows, Path::new("/none"), &huge);
        assert_eq!(kept.len(), 50, "the cap must be bounded above");
        assert_eq!(elided, 10);
    }

    #[test]
    fn under_expands_a_leading_tilde() {
        // A quoted or config-sourced `~/work` reaches the filter literally.
        // Unexpanded, `starts_with` matched nothing and the fan-out silently
        // returned zero projects.
        let home = std::env::var("HOME").expect("HOME must be set");
        let rows = vec![
            row("in", &format!("{home}/work/in"), Some(5), &["rust"]),
            row("out", "/elsewhere/out", Some(5), &["rust"]),
        ];
        let filter = FanoutFilter {
            under: Some(PathBuf::from("~/work")),
            ..FanoutFilter::default()
        };

        let (kept, _) = select_candidates(rows, Path::new("/none"), &filter);

        assert_eq!(
            kept.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["in"],
            "a tilde in --under must expand to $HOME"
        );
    }

    #[test]
    fn describe_names_the_filter_that_matched_nothing() {
        // An empty result must say what was actually applied, not just "none".
        let filter = FanoutFilter {
            under: Some(PathBuf::from("/w/team")),
            lang: Some("ruby".to_string()),
            max_projects: None,
        };
        let text = describe(&filter);

        assert!(text.contains("/w/team"), "got {text}");
        assert!(text.contains("ruby"), "got {text}");
    }

    #[test]
    fn a_description_is_carried_through_for_routing() {
        // The description is why an agent can pick the right project, so it
        // must survive into the response.
        let mut r = row("svc", "/w/svc", Some(5), &["ruby"]);
        r.description = Some(Description {
            text: "Handles billing.".to_string(),
            source: DescriptionSource::Config,
        });

        let (kept, _) = select_candidates(vec![r], Path::new("/none"), &FanoutFilter::default());

        assert_eq!(
            kept[0].description.as_ref().map(|d| d.text.as_str()),
            Some("Handles billing.")
        );
    }
}
