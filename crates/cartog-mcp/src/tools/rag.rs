//! MCP semantic-search tools: rag_search, context.

use std::sync::Arc;

use rmcp::{handler::server::wrapper::Parameters, tool, tool_router, ErrorData as McpError};

use crate::types::*;
use crate::*;
use cartog_core::Compact;
use cartog_db::MAX_SEARCH_LIMIT;
use cartog_rag as rag;

#[tool_router(router = rag_router, vis = "pub(crate)")]
impl CartogServer {
    /// Semantic search over code symbols using hybrid FTS5 + vector search.
    #[tool(
        description = "Search code by concept, keyword, or natural language. Returns ranked symbols with snippet excerpts — locations + previews, not full bodies. Use to LOCATE code matching a concept: 'find code related to...', 'show me the authentication logic'. For 'how does X work?' or understanding an area, prefer cartog_context — it returns full bodies + call neighbors in one call instead of snippets you'd then have to Read. Works even without embeddings (keyword matching alone is already strong). Prefer this over Grep for code discovery. Not for: looking up a known symbol name (use cartog_search instead — more precise). Filter with kind='document' for docs, kind='all' for both. Returns: Symbol[] ranked by relevance with snippet excerpts.",
        annotations(
            title = "Semantic code search",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<rag::search::HybridSearchResult>()
    )]
    pub(crate) async fn cartog_rag_search(
        &self,
        Parameters(params): Parameters<RagSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query;
        let kind_str = params.kind;
        let limit = params.limit.unwrap_or(10).min(MAX_SEARCH_LIMIT);
        let db = Arc::clone(&self.db);
        let provider = Arc::clone(&self.embedding_provider);
        let reranker = Arc::clone(&self.reranker_provider);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            if query.is_empty() {
                return Err(mcp_err("query cannot be empty"));
            }

            debug!(query = %query, kind = ?kind_str, limit, "rag search");
            let db = db.lock().map_err(|_| mcp_err("internal error: database lock poisoned (server restart required)"))?;

            let kind_filter = match kind_str.as_deref() {
                Some("all") => rag::search::KindFilter::All,
                Some(s) => {
                    let kind = s.parse::<cartog_core::SymbolKind>().map_err(|_| {
                        mcp_err(
                            "invalid symbol kind. Valid: function, class, method, variable, import, interface, enum, type-alias, trait, module, document, all",
                        )
                    })?;
                    rag::search::KindFilter::Exact(kind)
                }
                None => rag::search::KindFilter::CodeOnly,
            };

            let mut provider = provider
                .lock()
                .map_err(|_| mcp_err("internal error: embedding provider lock poisoned (server restart required)"))?;
            // First semantic query of the process builds the cross-encoder here.
            let mut reranker = reranker
                .get()
                .map_err(|_| mcp_err("internal error: reranker lock poisoned (server restart required)"))?;
            let mut result = match reranker.as_mut().and_then(|r| r.as_mut()) {
                Some(r) => rag::search::hybrid_search(
                    &db, &query, limit, kind_filter, provider.as_mut(), Some(r.as_mut()),
                ),
                None => rag::search::hybrid_search(
                    &db, &query, limit, kind_filter, provider.as_mut(), None,
                ),
            }.map_err(|e| mcp_err(format!("semantic search failed: {e}")))?;
            // Compact-by-default: bound each body to a snippet (the tool advertises
            // "snippet excerpts"). CARTOG_MCP_COMPACT=0 restores full bodies.
            if mcp_compact() {
                result.snippet_in_place();
            }

            let (results, omitted) = fit_to_budget(result.results, mcp_list_budget());
            result.results = results;
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response(&db, json, structured, "cartog_rag_search", omitted, stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Build a one-shot task-context bundle fusing semantic search, structural
    /// neighbors, and centrality.
    #[tool(
        description = "PRIMARY TOOL — call FIRST for any 'how does X work?', 'understand/survey area Y', 'where do I implement Z?' question. ONE call returns the relevant symbols (semantic + keyword), their 1-hop call neighbors, and high-centrality definitions in the same files, with bodies inline and budgeted to fit. Read-equivalent: do NOT re-open the symbols it returns, and usually the ONLY call you need — answer from its bundle instead of a chain of search/refs/callees/outline/Read. Drill in with the granular tools (cartog_refs, cartog_callees, cartog_trace) only for follow-ups it doesn't cover. Routes through semantic search, so it finds code by concept, not just name. Raise `tokens` (default 6000, max 20000) for a whole subsystem. Not for: a single known symbol (use cartog_search), or one specific call path (use cartog_trace). Returns: {task, entries: [{symbol, reason, score, body?}], approx_tokens}.",
        annotations(
            title = "Task context bundle",
            read_only_hint = true,
            open_world_hint = false
        ),
        output_schema = output_schema_for::<rag::context::TaskContext>()
    )]
    pub(crate) async fn cartog_context(
        &self,
        Parameters(params): Parameters<ContextParams>,
    ) -> Result<CallToolResult, McpError> {
        let task = params.task;
        let tokens = params
            .tokens
            .unwrap_or(DEFAULT_CONTEXT_TOKENS)
            .min(MAX_CONTEXT_TOKENS);
        let db = Arc::clone(&self.db);
        let provider = Arc::clone(&self.embedding_provider);
        let reranker = Arc::clone(&self.reranker_provider);
        let stale = self.stale_snapshot();

        tokio::task::spawn_blocking(move || {
            if task.is_empty() {
                return Err(mcp_err("task description cannot be empty"));
            }
            debug!(task = %task, tokens, "context");
            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;
            let mut provider = provider.lock().map_err(|_| {
                mcp_err(
                    "internal error: embedding provider lock poisoned (server restart required)",
                )
            })?;
            // First semantic query of the process builds the cross-encoder here.
            let mut reranker = reranker.get().map_err(|_| {
                mcp_err("internal error: reranker lock poisoned (server restart required)")
            })?;
            let opts = rag::context::ContextOptions::default();
            let result = match reranker.as_mut().and_then(|r| r.as_mut()) {
                Some(r) => rag::context::build_task_context(
                    &db,
                    &task,
                    tokens,
                    provider.as_mut(),
                    Some(r.as_mut()),
                    &opts,
                ),
                None => rag::context::build_task_context(
                    &db,
                    &task,
                    tokens,
                    provider.as_mut(),
                    None,
                    &opts,
                ),
            }
            .map_err(|e| mcp_err(format!("context build failed: {e}")))?;
            // Compact trims per-entry symbol noise but KEEPS the budgeted bodies —
            // this tool's whole value is its inline bodies.
            let mut result = result;
            if mcp_compact() {
                result.compact_in_place();
            }

            let (entries, omitted) = fit_to_budget(result.entries, mcp_list_budget());
            result.entries = entries;
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            tool_response(&db, json, structured, "cartog_context", omitted, stale)
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }
}
