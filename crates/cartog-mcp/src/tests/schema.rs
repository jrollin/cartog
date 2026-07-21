//! Tool-annotation and output-schema / structured-content tests.

use super::snap;
use crate::*;

// ── Tool annotation tests ──

/// Tools without side effects advertise `read_only_hint = true`; the three
/// side-effecting tools advertise `false`. Two of those write the DB
/// (`cartog_index`, `cartog_rag_index`); `cartog_update` arms a machine-level
/// deferred update. Clients use the hint to skip approval prompts for safe
/// tools and flag the rest.
#[test]
fn tool_annotations_mark_read_only_correctly() {
    let side_effecting = ["cartog_index", "cartog_rag_index", "cartog_update"];
    let tools = CartogServer::tool_router().list_all();

    assert!(!tools.is_empty(), "router exposes tools");
    for tool in &tools {
        let ann = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
        let expected_read_only = !side_effecting.contains(&tool.name.as_ref());
        assert_eq!(
            ann.read_only_hint,
            Some(expected_read_only),
            "{} read_only_hint",
            tool.name
        );
    }
}

/// Every tool exposes a human-readable title for client tool pickers.
#[test]
fn every_tool_has_a_title() {
    for tool in CartogServer::tool_router().list_all() {
        let title = tool
            .annotations
            .as_ref()
            .and_then(|a| a.title.as_deref())
            .or(tool.title.as_deref());
        assert!(
            title.is_some_and(|t| !t.is_empty()),
            "{} has no title",
            tool.name
        );
    }
}

// ── Output schema / structured content tests ──

/// Every read tool advertises an output schema so schema-aware clients can
/// validate `structuredContent`. The two write tools have no typed output.
#[test]
fn read_tools_advertise_output_schemas() {
    let writers = ["cartog_index", "cartog_rag_index"];
    for tool in CartogServer::tool_router().list_all() {
        let has_schema = tool.output_schema.is_some();
        let expected = !writers.contains(&tool.name.as_ref());
        assert_eq!(has_schema, expected, "{} output_schema", tool.name);
    }
}

/// `schema_for_output` rejects non-object schemas, so list tools must wrap
/// their arrays. Building each schema proves the wrappers stay object-typed
/// (a regression here would panic the tool macro at startup).
#[test]
fn output_schemas_are_objects() {
    output_schema_for::<SymbolList>();
    output_schema_for::<EdgeList>();
    output_schema_for::<RefList>();
    output_schema_for::<ImpactList>();
    output_schema_for::<HierarchyList>();
    output_schema_for::<MapResult>();
    output_schema_for::<StatsResult>();
    output_schema_for::<cartog_core::ChangesResult>();
    output_schema_for::<rag::search::HybridSearchResult>();
}

/// Non-standard integer formats (`uint32`, …) are stripped so strict client
/// validators don't warn, while the field stays typed as an integer.
#[test]
fn output_schema_strips_nonstandard_int_formats() {
    let schema = output_schema_for::<SymbolList>();
    let value = serde_json::Value::Object((*schema).clone());

    fn collect_formats(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(f)) = map.get("format") {
                    out.push(f.clone());
                }
                map.values().for_each(|v| collect_formats(v, out));
            }
            serde_json::Value::Array(items) => {
                items.iter().for_each(|v| collect_formats(v, out));
            }
            _ => {}
        }
    }
    let mut formats = Vec::new();
    collect_formats(&value, &mut formats);
    assert!(
        !formats
            .iter()
            .any(|f| NONSTANDARD_INT_FORMATS.contains(&f.as_str())),
        "non-standard int formats leaked: {formats:?}"
    );

    // The integer field survives, just without the bogus format.
    let start_line = &value["$defs"]["Symbol"]["properties"]["start_line"];
    assert_eq!(start_line["type"], "integer");
    assert!(start_line.get("format").is_none());
}

fn populated_memory_db() -> Database {
    let db = Database::open_memory().expect("in-memory DB");
    db.upsert_file(&cartog_core::FileInfo {
        path: "test.py".to_string(),
        last_modified: 0.0,
        hash: "h".to_string(),
        language: "python".to_string(),
        num_symbols: 1,
    })
    .expect("upsert file");
    db
}

/// A full (under-budget) response mirrors its payload into
/// `structuredContent` while keeping the bare-array text block.
#[test]
fn tool_response_attaches_structured_content_under_budget() {
    let db = populated_memory_db();
    let symbols = db.search("anything", None, None, 30).expect("search");
    let json = serde_json::to_string_pretty(&symbols).expect("json");
    let structured = serde_json::to_value(SymbolList { results: symbols }).ok();

    let result = tool_response(&db, json, structured, "cartog_search", 0, None).expect("response");

    let structured = result
        .structured_content
        .expect("structured content present");
    assert!(
        structured.get("results").is_some(),
        "structured content is the object wrapper"
    );
    assert_eq!(result.content.len(), 1, "text block retained");
}

/// `fit_to_budget` keeps a whole list that already fits, and drops trailing
/// items (reporting the count) when the serialized form exceeds the budget.
/// This is the element-level bound that keeps both the text block and the
/// structured wrapper under the cap while staying schema-valid.
#[test]
fn fit_to_budget_trims_list_to_fit() {
    let items: Vec<String> = (0..100).map(|i| format!("item-{i:04}")).collect();

    // A budget that comfortably holds all items keeps them untouched.
    let full = serde_json::to_string_pretty(&items).expect("json").len();
    let (kept, omitted) = fit_to_budget(items.clone(), full);
    assert_eq!(omitted, 0, "nothing dropped when the whole list fits");
    assert_eq!(kept.len(), 100, "all items kept");

    // A tight budget drops trailing items; what remains must actually fit.
    let (kept, omitted) = fit_to_budget(items.clone(), 200);
    assert!(omitted > 0, "some items dropped under a tight budget");
    assert_eq!(kept.len() + omitted, 100, "omitted count is exact");
    assert!(
        serde_json::to_string_pretty(&kept).expect("json").len() <= 200,
        "kept slice fits the budget"
    );
}

/// The regression this whole change fixes: a large result is bounded at the
/// element level, so `structuredContent` is present (MCP spec) AND the whole
/// response — text plus the structured mirror — stays under the cap. Builds
/// both from the same trimmed slice via `fit_to_budget`, exactly as the
/// handlers do, and asserts the serialized structured payload is capped (not
/// shipped in full).
#[test]
fn oversized_result_bounds_text_and_structured_together() {
    let db = populated_memory_db();
    let cap = mcp_max_bytes();
    // Far more symbol-sized items than fit: each row is ~1KB, so hundreds
    // overflow the 64KB cap and must be trimmed.
    let rows: Vec<String> = (0..500)
        .map(|i| format!("{i:04}-{}", "z".repeat(1024)))
        .collect();

    let (kept, omitted) = fit_to_budget(rows, cap);
    assert!(omitted > 0, "the oversized list was trimmed");

    // Build both text and structured from the same trimmed slice, as the
    // handlers do.
    let json = serde_json::to_string_pretty(&kept).expect("json");
    let structured = Some(serde_json::json!({ "results": kept }));

    let result =
        tool_response(&db, json, structured, "cartog_search", omitted, None).expect("response");

    let structured = result
        .structured_content
        .expect("structuredContent kept — required for a schema-bearing tool");
    let structured_bytes = serde_json::to_string(&structured).expect("json").len();
    assert!(
        structured_bytes <= cap,
        "structured mirror is bounded, not shipped in full: {structured_bytes} > {cap}"
    );
    let text = match &result.content.first().expect("content").raw {
        RawContent::Text(t) => &t.text,
        _ => panic!("expected text content"),
    };
    assert!(
        text.len() <= cap,
        "text block bounded: {} > {cap}",
        text.len()
    );
    assert!(
        text.contains(&format!("{omitted} result(s) omitted")),
        "honest omitted-count notice: {text}"
    );
}

/// A staleness banner must not push a response over the cap: the final clamp
/// trims the text so banner + body ≤ cap even for an oversized body.
#[test]
fn tool_response_with_banner_stays_under_cap() {
    let db = populated_memory_db();
    let big = "x".repeat(mcp_max_bytes() + 1024);
    let json = format!("[\"{big}\"]");
    // rag_pending on a rag tool fires the longest banner.
    let stale = Some(snap(9, 0, 0));
    let result = tool_response(&db, json, None, "cartog_rag_search", 0, stale).expect("response");
    let text = match &result.content.first().expect("content").raw {
        RawContent::Text(t) => &t.text,
        _ => panic!("expected text content"),
    };
    assert!(text.starts_with("⚠️"), "banner present: {}", &text[..40]);
    assert!(
        text.len() <= mcp_max_bytes(),
        "banner + body must stay under the {}-byte cap, got {}",
        mcp_max_bytes(),
        text.len()
    );
}

/// The final clamp also covers the NON-truncated path: a body just under the
/// cap, plus a banner and an appended suggestion, must still end up ≤ the cap
/// (suffixes aren't individually budgeted).
#[test]
fn tool_response_banner_plus_suffix_stays_under_cap() {
    let db = populated_memory_db();
    let cap = mcp_max_bytes();
    // Body sized so banner + body alone is just under cap; the appended
    // suggestion would push it over without the final clamp.
    let payload = "y".repeat(cap - 200);
    let json = format!("[\"{payload}\"]");
    let stale = Some(snap(3, 0, 0));
    let result = tool_response(&db, json, None, "cartog_rag_search", 0, stale).expect("response");
    let text = match &result.content.first().expect("content").raw {
        RawContent::Text(t) => &t.text,
        _ => panic!("expected text content"),
    };
    assert!(
        text.len() <= cap,
        "banner + body + suffix must stay under {cap}, got {}",
        text.len()
    );
}

/// `StatsResult` flattens `IndexStats` and adds role + watcher fields at the
/// top level (no nested `stats` object), matching the documented shape.
#[test]
fn stats_result_flattens_index_stats() {
    let db = populated_memory_db();
    let stats = db.stats().expect("stats");
    let result = StatsResult {
        stats,
        role: Role::ReadOnly,
        watcher_active: false,
        degraded: false,
    };
    let value = serde_json::to_value(&result).expect("serialize");
    let obj = value.as_object().expect("object");
    assert!(obj.contains_key("num_files"), "flattened stats field");
    // Role serializes to the exact wire string (hyphen preserved).
    assert_eq!(obj.get("role").and_then(|v| v.as_str()), Some("read-only"));
    assert_eq!(
        obj.get("watcher_active").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(!obj.contains_key("stats"), "stats is flattened, not nested");
    // `degraded: false` is skipped, so non-degraded `--json` output is unchanged.
    assert!(
        !obj.contains_key("degraded"),
        "degraded must be omitted when false"
    );
}
