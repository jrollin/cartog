//! MCP discovery tool: cartog_list_projects.

use rmcp::{tool, tool_router, ErrorData as McpError};

use crate::types::*;
use crate::*;

#[tool_router(router = projects_router, vis = "pub(crate)")]
impl CartogServer {
    /// List the other cartog-indexed projects on this machine.
    ///
    /// Gated by **neither** `refuse_if_degraded` nor `refuse_if_read_only`,
    /// for the reason `cartog_update` already records: this tool does not touch
    /// the index DB, so gating it on the index's state would be a category
    /// error.
    ///
    /// - `refuse_if_read_only`: a read-only secondary is read-only with respect
    ///   to *its project's* database. The registry is a different file.
    /// - `refuse_if_degraded`: a degraded server has no index for *this*
    ///   project — which is precisely when knowing "this repo isn't indexed,
    ///   but here are the twelve that are" is most useful. Refusing would make
    ///   the tool useless exactly when it matters most. Its own `current` flag
    ///   is simply false, which is the honest answer.
    ///
    /// Reads the registry and the state directory's PID files only. It **never
    /// opens a foreign project's database**, so its cost is independent of how
    /// many projects are registered and it cannot contend with another
    /// project's writer.
    #[tool(
        description = "List the OTHER cartog-indexed projects on this machine, with each project's \
                       database path, languages, and size. Use when a question is about a different \
                       repository than the current one — a sibling service, a shared library — so you \
                       can run a cartog CLI command against it with `--db <db_path>` instead of \
                       guessing or reading files. Routing signal is name + languages + size only: this \
                       does NOT describe what each project does. `current: true` marks the project \
                       this server already serves — use the normal tools for that one, not --db. \
                       Not for: searching within the current project (use cartog_search / \
                       cartog_rag_search). Returns: {registry_available, projects[]}.",
        annotations(
            title = "List indexed projects",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            // Reads only local files this machine already wrote.
            open_world_hint = false
        ),
        output_schema = output_schema_for::<ListProjectsResult>()
    )]
    pub(crate) async fn cartog_list_projects(&self) -> Result<CallToolResult, McpError> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            debug!("list_projects");
            let listing = cartog_registry::list_projects(cartog_db::CURRENT_SCHEMA_VERSION);
            let result = build_result(&listing, &db_path);
            let text = render_projects(&result);
            // Bound text and structured content together: an outputSchema tool
            // must always return structuredContent, so the two cannot diverge.
            let structured = serde_json::to_value(&result).ok();
            Ok(success_result(text, structured))
        })
        .await
        .map_err(|e| mcp_err(format!("list_projects task panicked: {e}")))?
    }
}

/// Convert a registry listing into the tool's result shape.
///
/// Split out so tests drive it with an explicit `Listing` and `db_path`, with
/// no dependence on the process-global `CARTOG_REGISTRY`. That matters because
/// this crate has two independent test-serialization mechanisms, so an
/// env-mutating test can never be reliably isolated from the other.
pub(crate) fn build_result(
    listing: &cartog_registry::Listing,
    db_path: &Path,
) -> ListProjectsResult {
    // Identify this project by slot, not by string compare: a relative or
    // symlinked db_path must still match the row written for it.
    let current_slot =
        (!db_path.as_os_str().is_empty()).then(|| cartog_registry::slot_for_db("serve", db_path));

    let projects: Vec<ProjectEntry> = listing
        .projects
        .iter()
        .map(|row| {
            let row_slot = cartog_registry::slot_for_db("serve", &row.db_path);
            let current = current_slot
                .as_deref()
                .is_some_and(|mine| mine == row_slot || mine == row.id);
            ProjectEntry {
                id: row.id.clone(),
                name: row.name.clone(),
                root: row.root.display().to_string(),
                db_path: row.db_path.display().to_string(),
                languages: row
                    .languages
                    .iter()
                    .map(|(language, symbols)| ProjectLanguage {
                        language: language.clone(),
                        symbols: *symbols,
                    })
                    .collect(),
                file_count: row.file_count,
                symbol_count: row.symbol_count,
                edge_count: row.edge_count,
                resolved_count: row.resolved_count,
                embedding_count: row.embedding_count,
                schema_version: row.schema_version,
                last_indexed: row.last_indexed.map(format_unix_rfc3339),
                current,
                live: row.markers.live,
                stale_schema: row.markers.stale_schema,
                missing: row.markers.missing,
                embed_mismatch: row.markers.embed_mismatch,
            }
        })
        .collect();

    ListProjectsResult {
        registry_available: listing.available,
        projects,
    }
}

/// Human-readable rendering, kept compact: an agent pays for every token.
fn render_projects(result: &ListProjectsResult) -> String {
    if !result.registry_available {
        return "No project registry on this machine — nothing else has been indexed yet, \
                or the registry is disabled (CARTOG_REGISTRY).\n"
            .to_string();
    }
    if result.projects.is_empty() {
        return "No other cartog-indexed projects on this machine.\n".to_string();
    }

    let mut out = String::new();
    for p in &result.projects {
        let langs = if p.languages.is_empty() {
            "—".to_string()
        } else {
            p.languages
                .iter()
                .take(3)
                .map(|l| l.language.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let symbols = p
            .symbol_count
            .map_or_else(|| "?".to_string(), |n| n.to_string());
        let mut flags = Vec::new();
        if p.current {
            flags.push("current");
        }
        if p.live {
            flags.push("live");
        }
        if p.stale_schema {
            flags.push("stale-schema");
        }
        if p.missing {
            flags.push("missing");
        }
        if p.embed_mismatch {
            flags.push("embed-mismatch");
        }
        let flag_text = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };
        out.push_str(&format!(
            "{} — {symbols} symbols, {langs}{flag_text}\n  --db {}\n",
            p.name, p.db_path
        ));
    }
    out.push_str("\nQuery another project by passing its --db path to a cartog CLI command.\n");
    out
}

/// RFC3339 from a unix second. Local to this crate so the tool does not depend
/// on the binary's `time_fmt`.
fn format_unix_rfc3339(secs: i64) -> String {
    // Delegates to the same conversion the CLI uses, via chrono-free math.
    let secs = secs.max(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Days since the Unix epoch → `(year, month, day)`. Howard Hinnant's
/// `civil_from_days`, the standard branch-free algorithm; handles leap years.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc3339_formatter_agrees_with_known_timestamps() {
        // Two independent date implementations now exist (this one and the
        // binary's time_fmt). Pin this one against known values so they cannot
        // silently disagree about what a registry timestamp means.
        assert_eq!(format_unix_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        // A leap day, the case naive date math gets wrong.
        assert_eq!(format_unix_rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
        // A century non-leap year boundary.
        assert_eq!(format_unix_rfc3339(4_107_542_400), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn a_pre_epoch_timestamp_clamps_rather_than_panicking() {
        // A corrupted stored value must not crash a tool call.
        assert_eq!(format_unix_rfc3339(-1), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn an_unavailable_registry_renders_an_explanation_not_an_empty_list() {
        let text = render_projects(&ListProjectsResult {
            registry_available: false,
            projects: vec![],
        });
        assert!(text.contains("No project registry"));
        assert!(text.contains("CARTOG_REGISTRY"));
    }

    #[test]
    fn an_available_empty_registry_says_so_distinctly() {
        let text = render_projects(&ListProjectsResult {
            registry_available: true,
            projects: vec![],
        });
        assert!(text.contains("No other cartog-indexed projects"));
        assert!(!text.contains("CARTOG_REGISTRY"));
    }

    fn entry(name: &str, current: bool) -> ProjectEntry {
        ProjectEntry {
            id: format!("serve-{name}"),
            name: name.to_string(),
            root: format!("/w/{name}"),
            db_path: format!("/w/{name}/.cartog/db.sqlite"),
            languages: vec![ProjectLanguage {
                language: "rust".to_string(),
                symbols: 400,
            }],
            file_count: Some(400),
            symbol_count: Some(8134),
            edge_count: Some(19_000),
            resolved_count: Some(13_000),
            embedding_count: Some(8134),
            schema_version: Some(8),
            last_indexed: Some("2023-11-14T22:13:20Z".to_string()),
            current,
            live: false,
            stale_schema: false,
            missing: false,
            embed_mismatch: false,
        }
    }

    #[test]
    fn the_rendering_always_shows_the_db_path_as_a_usable_flag() {
        // The whole point of the tool: the agent must see how to query it.
        let text = render_projects(&ListProjectsResult {
            registry_available: true,
            projects: vec![entry("svc-shipping", false)],
        });
        assert!(text.contains("--db /w/svc-shipping/.cartog/db.sqlite"));
    }

    #[test]
    fn the_serving_project_is_flagged_current_so_an_agent_does_not_reroute_to_itself() {
        let text = render_projects(&ListProjectsResult {
            registry_available: true,
            projects: vec![entry("mine", true), entry("other", false)],
        });
        let mine = text.lines().find(|l| l.starts_with("mine")).unwrap();
        let other = text.lines().find(|l| l.starts_with("other")).unwrap();
        assert!(mine.contains("[current]"));
        assert!(!other.contains("current"));
    }

    #[test]
    fn an_unknown_symbol_count_renders_as_a_question_mark_not_zero() {
        let mut e = entry("bare", false);
        e.symbol_count = None;
        let text = render_projects(&ListProjectsResult {
            registry_available: true,
            projects: vec![e],
        });
        assert!(text.contains("? symbols"));
    }

    #[test]
    fn every_marker_renders_when_set() {
        let mut e = entry("all", true);
        e.live = true;
        e.stale_schema = true;
        e.missing = true;
        e.embed_mismatch = true;
        let text = render_projects(&ListProjectsResult {
            registry_available: true,
            projects: vec![e],
        });
        for f in [
            "current",
            "live",
            "stale-schema",
            "missing",
            "embed-mismatch",
        ] {
            assert!(text.contains(f), "missing flag: {f}");
        }
    }
}
