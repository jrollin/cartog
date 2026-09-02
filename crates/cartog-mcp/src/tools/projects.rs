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
            let mut result = build_result(&listing, &db_path);

            // Trim at the element level, then render — so the text block and
            // `structuredContent` are bounded *together* and cannot diverge.
            // An outputSchema tool must always return structuredContent, so
            // capping only the text would leave the structured half unbounded
            // (the defect PR #151 fixed for the other list tools).
            //
            // A machine with many registered projects is exactly the case this
            // tool exists for, so the list is genuinely unbounded in principle.
            let omitted = trim_to_budget(&mut result);

            let mut text = render_projects(&result);
            if omitted > 0 {
                // Say so rather than silently showing a subset: an agent
                // concluding "that's every project" would route wrongly.
                text.push_str(&format!(
                    "\n({omitted} more project(s) omitted to fit the response budget.)\n"
                ));
            }
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
                last_indexed: row.last_indexed.map(cartog_registry::format_timestamp),
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

/// Trim the project list so **both** the text block and `structuredContent`
/// fit the response cap, returning how many were dropped.
///
/// Budgets against the *envelope*, not the bare array: pretty-printed JSON
/// re-indents every line when the array sits one level deeper inside
/// `ListProjectsResult`, measured ~9% larger — enough that a list trimmed to
/// the bare-array budget still overshot the cap.
///
/// This bound matters more here than for the other list tools because
/// `tool_response_named`'s final clamp truncates only the text; the structured
/// half is never re-clamped, so this trim is the only thing bounding it.
pub(crate) fn trim_to_budget(result: &mut ListProjectsResult) -> usize {
    let envelope_budget = mcp_list_budget().saturating_sub(mcp_list_budget() / 8);
    let projects = std::mem::take(&mut result.projects);
    let (kept, omitted) = fit_to_budget(projects, envelope_budget);
    result.projects = kept;
    omitted
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

#[cfg(test)]
mod tests {
    use super::*;

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
