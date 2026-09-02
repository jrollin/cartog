//! Request parameter and JSON response-wrapper types for the MCP tools.

use rmcp::schemars;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Role;

// ── Parameter types ──

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IndexParams {
    /// Directory to index relative to project root (defaults to ".")
    #[serde(default = "default_dot")]
    pub path: String,
    /// Force full re-index, bypassing change detection
    #[serde(default)]
    pub force: bool,
}

fn default_dot() -> String {
    ".".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OutlineParams {
    /// File path relative to project root
    pub file: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RefsParams {
    /// Symbol name to find references for
    pub name: String,
    /// Filter by edge kind: calls, imports, inherits, references, raises
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CalleesParams {
    /// Symbol name to find callees of
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImpactParams {
    /// Symbol name to analyze impact for
    pub name: String,
    /// Maximum traversal depth (default 3, max 10)
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TraceParams {
    /// Starting symbol (the caller end of the path)
    pub from: String,
    /// Target symbol (the callee end of the path)
    pub to: String,
    /// Maximum path length to search (default 8, max 20)
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HierarchyParams {
    /// Class name to show hierarchy for
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DepsParams {
    /// File path to show import dependencies for
    pub file: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Case-insensitive query string (prefix + substring match against symbol names)
    pub query: String,
    /// Filter by symbol kind: function, class, method, variable, import, interface, enum, enum_member, type_alias, trait, module, macro, component, document
    pub kind: Option<String>,
    /// Filter to a specific file path relative to project root
    pub file: Option<String>,
    /// Maximum results to return (default 30, max 100)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RagIndexParams {
    /// Directory to index relative to project root (defaults to ".")
    #[serde(default = "default_dot")]
    pub path: String,
    /// Force re-embed all symbols (ignore existing embeddings)
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChangesParams {
    /// Number of recent commits to consider (default 5)
    pub commits: Option<u32>,
    /// Filter by symbol kind: function, class, method, variable, import, interface, enum, enum_member, type_alias, trait, module, macro, component, document
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RagSearchParams {
    /// Natural language query for semantic code search
    pub query: String,
    /// Filter by symbol kind: function, class, method, variable, import, interface, enum, enum_member, type_alias, trait, module, macro, component, document, all. Defaults to code only (excludes documents).
    pub kind: Option<String>,
    /// Maximum results to return (default 10)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextParams {
    /// Natural-language description of the task you're about to work on
    pub task: String,
    /// Approximate token budget for the returned bundle (default 6000)
    pub tokens: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MapParams {
    /// Maximum top-ranked symbols to include in the map (default 50).
    /// Symbols are ranked by in-degree centrality so the most-referenced
    /// definitions surface first.
    pub limit: Option<u32>,
}

// ── Response wrappers for JSON serialization ──
//
// MCP `structuredContent` must be a JSON object, and `schema_for_output`
// rejects non-object output schemas, so every tool returns an object — list
// tools wrap their array under a `results` field. The text content block keeps
// the original (bare-array) shape for clients without schema support.

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RefEntry {
    pub(crate) edge: cartog_core::Edge,
    pub(crate) source: Option<cartog_core::Symbol>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ImpactEntry {
    pub(crate) edge: cartog_core::Edge,
    pub(crate) depth: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct HierarchyEntry {
    pub(crate) child: String,
    pub(crate) parent: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SymbolList {
    pub(crate) results: Vec<cartog_core::Symbol>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct EdgeList {
    pub(crate) results: Vec<cartog_core::Edge>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RefList {
    pub(crate) results: Vec<RefEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ImpactList {
    pub(crate) results: Vec<ImpactEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TraceHop {
    pub(crate) source_name: String,
    pub(crate) target_name: String,
    pub(crate) kind: String,
    pub(crate) file_path: String,
    pub(crate) line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) body: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TraceList {
    /// Empty when `from == to`; absent path is reported as `found: false`.
    pub(crate) found: bool,
    pub(crate) hops: Vec<TraceHop>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct HierarchyList {
    pub(crate) results: Vec<HierarchyEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct MapResult {
    pub(crate) files: Vec<String>,
    pub(crate) top_symbols: Vec<cartog_core::Symbol>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct StatsResult {
    #[serde(flatten)]
    pub(crate) stats: cartog_db::IndexStats,
    pub(crate) role: Role,
    pub(crate) watcher_active: bool,
    /// True when the server started degraded: no `.cartog.toml`, no index, and
    /// `CARTOG_AUTO_INIT` unset, so no `.cartog/` was created. Distinct from an
    /// empty index (which exists on disk) and from a read-only secondary. Only
    /// serialized when true, so non-degraded `--json` output is unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) degraded: bool,
}

/// Result of `cartog_update` (arm a deferred self-update).
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct UpdateResult {
    /// The currently-running cartog version.
    pub(crate) current: String,
    /// The version that will be installed at the next boundary, when armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<String>,
    /// One of `armed`, `up-to-date`, `cargo-refused`, or `error`.
    pub(crate) status: String,
    /// When the armed update takes effect.
    pub(crate) apply: String,
    /// Human-readable summary for display.
    pub(crate) message: String,
}

/// One project as `cartog_list_projects` reports it.
///
/// Deliberately mirrors `cartog projects list --json` so one contract is tested
/// from two directions. `db_path` is the field that matters: with it an agent
/// runs any cartog CLI command against another project via `--db`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectEntry {
    /// Registry id (the project's serve slot). Stable across re-indexes.
    pub(crate) id: String,
    /// Project name — the root directory's basename. Phase 1 has no
    /// configured name or description, so this plus `languages` is the whole
    /// routing signal.
    pub(crate) name: String,
    pub(crate) root: String,
    /// Path to this project's index. Pass it as `--db <path>` to query the
    /// project without leaving the current session.
    pub(crate) db_path: String,
    /// Languages by symbol count, most-populous first.
    pub(crate) languages: Vec<ProjectLanguage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) symbol_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) edge_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) embedding_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schema_version: Option<u32>,
    /// RFC3339 timestamp of the last indexing pass, absent if never indexed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_indexed: Option<String>,
    /// True when this is the project **this server** is serving. An agent must
    /// not re-route to itself.
    pub(crate) current: bool,
    /// A `cartog serve`/`watch` peer holds this project's lock. Advisory.
    pub(crate) live: bool,
    /// The index was written at a different schema version than this binary's,
    /// so querying it needs a re-index. Its cached counts still describe the
    /// last known state.
    pub(crate) stale_schema: bool,
    /// The database file is gone; `cartog projects prune` drops the row.
    pub(crate) missing: bool,
    /// This project's embedding provider/model/dimension differs from most
    /// others, so its vectors are not comparable with theirs.
    pub(crate) embed_mismatch: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectLanguage {
    pub(crate) language: String,
    pub(crate) symbols: u32,
}

/// Result of `cartog_list_projects`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ListProjectsResult {
    /// False when there is no registry at all — nothing has been indexed on
    /// this machine, or `CARTOG_REGISTRY` is disabled. An empty `projects`
    /// list means something different in each case, so never infer one from
    /// the other.
    pub(crate) registry_available: bool,
    pub(crate) projects: Vec<ProjectEntry>,
}
