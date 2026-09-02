//! `cartog projects list` — every indexed project on this machine.

use anyhow::Result;
use serde::Serialize;

use cartog_registry::{Listing, ProjectRow};

/// A project as `--json` reports it.
///
/// Shaped to match the `cartog_list_projects` MCP tool so one contract is
/// tested from two directions. `db_path` is the field that matters: with it a
/// consumer runs any cartog command against another project via `--db`.
#[derive(Debug, Serialize)]
pub(crate) struct ProjectJson {
    pub id: String,
    pub name: String,
    pub root: String,
    pub db_path: String,
    pub languages: Vec<LanguageJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_count: Option<u32>,
    /// `resolved / edges`, or `None` when either is unknown or there are no
    /// edges. Derived rather than stored — it is what a consumer wants and it
    /// must never disagree with the two counts beside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_dim: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed: Option<String>,
    pub last_seen: String,
    pub live: bool,
    pub stale_schema: bool,
    pub missing: bool,
    pub embed_mismatch: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct LanguageJson {
    pub language: String,
    pub symbols: u32,
}

/// `--json` envelope.
///
/// `registry_available` is load-bearing: an empty `projects` list means
/// something different when the registry is absent (nothing has been indexed on
/// this machine, or `CARTOG_REGISTRY` is disabled) than when it exists and holds
/// no rows.
#[derive(Debug, Serialize)]
pub(crate) struct ProjectsJson {
    pub registry_available: bool,
    pub projects: Vec<ProjectJson>,
}

/// List every project in the machine-local registry.
pub fn cmd_projects_list(json: bool, tokens: Option<u32>) -> Result<()> {
    let listing = cartog_registry::list_projects(cartog_db::CURRENT_SCHEMA_VERSION);
    let payload = to_json(&listing);
    super::super::shared::output(&payload, json, tokens, |_| render(&listing))
}

pub(crate) fn to_json(listing: &Listing) -> ProjectsJson {
    ProjectsJson {
        registry_available: listing.available,
        projects: listing.projects.iter().map(row_to_json).collect(),
    }
}

fn row_to_json(row: &ProjectRow) -> ProjectJson {
    ProjectJson {
        id: row.id.clone(),
        name: row.name.clone(),
        root: row.root.display().to_string(),
        db_path: row.db_path.display().to_string(),
        languages: row
            .languages
            .iter()
            .map(|(language, symbols)| LanguageJson {
                language: language.clone(),
                symbols: *symbols,
            })
            .collect(),
        file_count: row.file_count,
        symbol_count: row.symbol_count,
        edge_count: row.edge_count,
        resolved_count: row.resolved_count,
        resolution_rate: resolution_rate(row.resolved_count, row.edge_count),
        embedding_count: row.embedding_count,
        embed_provider: row.embed_provider.clone(),
        embed_model: row.embed_model.clone(),
        embed_dim: row.embed_dim,
        schema_version: row.schema_version,
        last_indexed: row.last_indexed.map(format_unix),
        last_seen: format_unix(row.last_seen),
        live: row.markers.live,
        stale_schema: row.markers.stale_schema,
        missing: row.markers.missing,
        embed_mismatch: row.markers.embed_mismatch,
    }
}

/// `resolved / edges`, or `None` when the ratio would be meaningless.
pub(crate) fn resolution_rate(resolved: Option<u32>, edges: Option<u32>) -> Option<f64> {
    match (resolved, edges) {
        // Zero edges is not "0% resolved", it is "nothing to resolve".
        (Some(r), Some(e)) if e > 0 => Some(f64::from(r) / f64::from(e)),
        _ => None,
    }
}

/// Render a registry timestamp as RFC3339.
///
/// Delegates to `cartog-registry`, which owns these timestamps, so the CLI and
/// the `cartog_list_projects` MCP tool render them identically.
fn format_unix(secs: i64) -> String {
    cartog_registry::format_timestamp(secs)
}

/// Human-readable listing, one line per project.
pub(crate) fn render(listing: &Listing) -> String {
    if !listing.available {
        return "No project registry found — nothing has been indexed on this machine yet, \
                or CARTOG_REGISTRY is disabled.\n"
            .to_string();
    }
    if listing.projects.is_empty() {
        return "No projects registered yet. Run `cartog index` in a project to add it.\n"
            .to_string();
    }

    let mut out = String::new();
    for row in &listing.projects {
        out.push_str(&render_row(row));
    }
    out.push_str(&format!(
        "\n{} project{} registered. Query another with `cartog <command> --db <db_path>`.\n",
        listing.projects.len(),
        if listing.projects.len() == 1 { "" } else { "s" }
    ));
    out
}

fn render_row(row: &ProjectRow) -> String {
    let symbols = row
        .symbol_count
        .map_or_else(|| "?".to_string(), |n| n.to_string());
    let files = row
        .file_count
        .map_or_else(|| "?".to_string(), |n| n.to_string());
    let langs = if row.languages.is_empty() {
        "—".to_string()
    } else {
        row.languages
            .iter()
            .take(3)
            .map(|(l, _)| l.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let when = row
        .last_indexed
        .map_or_else(|| "never".to_string(), cartog::time_fmt::format_relative);

    let mut markers = Vec::new();
    if row.markers.live {
        markers.push("live".to_string());
    }
    if row.markers.stale_schema {
        // Name the version: "stale-schema" alone gives the user nothing to act on.
        markers.push(match row.schema_version {
            Some(v) => format!("stale-schema v{v}"),
            None => "stale-schema".to_string(),
        });
    }
    if row.markers.missing {
        markers.push("missing".to_string());
    }
    if row.markers.embed_mismatch {
        markers.push("embed-mismatch".to_string());
    }
    let marker_text = if markers.is_empty() {
        String::new()
    } else {
        format!("  [{}]", markers.join(", "))
    };

    format!(
        "{:<24} {:>9} symbols {:>6} files  {:<24} {:>10}{}\n  {}\n",
        truncate(&row.name, 24),
        symbols,
        files,
        truncate(&langs, 24),
        when,
        marker_text,
        row.db_path.display(),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    // Ellipsis so a truncated value is never mistaken for a real one.
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}…")
}
