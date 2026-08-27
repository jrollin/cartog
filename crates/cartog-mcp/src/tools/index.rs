//! MCP indexing tools: cartog_index, cartog_rag_index.

use std::sync::Arc;

use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router,
    ErrorData as McpError, RoleServer,
};

use crate::types::*;
use crate::*;
use cartog_indexer as indexer;
use cartog_rag as rag;

#[tool_router(router = index_router, vis = "pub(crate)")]
impl CartogServer {
    /// Build or rebuild the code graph index for a directory.
    #[tool(
        description = "Build or rebuild the code graph index. Run this first before any other cartog tool, or after making code changes to keep the graph current. Incremental by default — only re-indexes changed files. Use force=true if results seem stale. Not for: routine queries (call once per session, not before every read). Returns: {files_indexed, files_skipped, symbols_added, edges_added, edges_resolved, edges_lsp_resolved, edges_marked_unresolvable, edges_marked_external}.",
        annotations(
            title = "Index codebase",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn cartog_index(
        &self,
        Parameters(params): Parameters<IndexParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Consent gate before the read-only check: a degraded start has no
        // index to write to and a tool call is not opt-in.
        if let Some(err) = self.refuse_if_degraded("cartog_index") {
            return Err(err);
        }
        if let Some(err) = self.refuse_if_read_only("cartog_index") {
            return Err(err);
        }
        let path = params.path;
        let force = params.force;
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);
        let redact = self.redact;
        let walk_filter = Arc::clone(&self.walk_filter);
        #[cfg(feature = "lsp")]
        let lsp_manager = Arc::clone(&self.lsp_manager);
        #[cfg(not(feature = "lsp"))]
        let lsp_manager: () = ();
        #[cfg(feature = "lsp")]
        let lsp_unavailable = Arc::clone(&self.lsp_unavailable);
        #[cfg(not(feature = "lsp"))]
        let lsp_unavailable: () = ();

        let (progress_tx, forwarder) = match ctx.meta.get_progress_token() {
            Some(token) => {
                let notifier = progress::peer_notifier(ctx.peer.clone());
                let fwd = progress::spawn_forwarder(token, notifier);
                (Some(fwd.tx.clone()), Some(fwd))
            }
            None => (None, None),
        };

        let cancel = ctx.ct.clone();
        let join = tokio::task::spawn_blocking(move || {
            let validated = validate_path_within_cwd_canonical(&path, &cwd).map_err(mcp_err)?;
            debug!(path = %validated.display(), force, "indexing directory");
            let probe = || cancel.is_cancelled();
            let probe_ref: Option<&(dyn Fn() -> bool + Send + Sync)> = Some(&probe);
            let result = index_with_optional_lsp(
                &db,
                &lsp_manager,
                &validated,
                force,
                progress_tx.clone(),
                probe_ref,
                redact,
                walk_filter.as_ref(),
            )?;
            let result = catch_up_lsp(
                &db,
                &lsp_manager,
                &lsp_unavailable,
                &validated,
                progress_tx,
                probe_ref,
                result,
            )?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            // Human summary first, then the raw JSON, so the agent sees counts at a glance.
            let mut text = format!("{}\n{json}", indexer::render_index_summary(&result));
            if let Some(hint) = suggestions_for("cartog_index") {
                text.push_str("\n\n");
                text.push_str(hint);
            }
            Ok(CallToolResult::success(vec![Content::text(text)]))
        })
        .await;

        // Drain the forwarder unconditionally — even on join error or tool
        // error — so the spawned task doesn't leak waiting on `rx.recv()`.
        // The blocking closure already dropped its progress_tx clone; the
        // Forwarder still holds one, so we drop it here to close the channel.
        if let Some(fwd) = forwarder {
            drop(fwd.tx);
            let _ = fwd.join.await;
        }

        join.map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }

    /// Build embedding index for semantic code search.
    #[tool(
        description = "Build the embedding index for semantic search. Optional — cartog_rag_search ALREADY works at FTS5 (BM25) quality without embeddings; only run this when you want vector recall on top. Requires `cartog rag setup` from the CLI first to download the model. Not for: first-time setup of cartog (cartog_index is what you want). Returns: {embedded, skipped, failed, dim}.",
        annotations(
            title = "Build embedding index",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn cartog_rag_index(
        &self,
        Parameters(params): Parameters<RagIndexParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(err) = self.refuse_if_degraded("cartog_rag_index") {
            return Err(err);
        }
        if let Some(err) = self.refuse_if_read_only("cartog_rag_index") {
            return Err(err);
        }
        let path = params.path;
        let force = params.force;
        let db = Arc::clone(&self.db);
        let cwd = Arc::clone(&self.cwd);
        let redact = self.redact;
        let walk_filter = Arc::clone(&self.walk_filter);
        let provider = Arc::clone(&self.embedding_provider);

        let (progress_tx, forwarder) = match ctx.meta.get_progress_token() {
            Some(token) => {
                let notifier = progress::peer_notifier(ctx.peer.clone());
                let fwd = progress::spawn_forwarder(token, notifier);
                (Some(fwd.tx.clone()), Some(fwd))
            }
            None => (None, None),
        };

        let cancel = ctx.ct.clone();
        let join = tokio::task::spawn_blocking(move || {
            let validated = validate_path_within_cwd_canonical(&path, &cwd).map_err(mcp_err)?;
            debug!(path = %validated.display(), force, "rag index");

            let db = db.lock().map_err(|_| {
                mcp_err("internal error: database lock poisoned (server restart required)")
            })?;

            let probe = || cancel.is_cancelled();
            let probe_ref: Option<&(dyn Fn() -> bool + Send + Sync)> = Some(&probe);

            // Ensure the code graph index is up to date first. Inner phases are
            // intentionally suppressed: the RAG tool exposes only rag-specific
            // phases (preparing/embedding/storing) so the client-facing
            // vocabulary stays stable.
            let _ = indexer::index_directory(
                &db,
                &validated,
                false,
                false,
                None,
                probe_ref,
                redact,
                &std::collections::HashMap::new(),
                walk_filter.as_ref(),
            )
            .map_err(|e| mcp_err(format!("code graph indexing failed: {e}")))?;

            let mut provider = provider.lock().map_err(|_| {
                mcp_err(
                    "internal error: embedding provider lock poisoned (server restart required)",
                )
            })?;

            let rag_cb = progress_tx
                .as_ref()
                .map(|tx| progress::rag_callback(tx.clone()));
            let cb_ref: Option<&(dyn Fn(rag::indexer::ProgressUpdate) + Send + Sync)> =
                rag_cb.as_ref().map(|f| f as _);

            let result =
                rag::indexer::index_embeddings(&db, provider.as_mut(), force, cb_ref, probe_ref)
                    .map_err(|e| mcp_err(format!("embedding indexing failed: {e}")))?;

            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            Ok(CallToolResult::success(vec![Content::text(json)]))
        })
        .await;

        // Drain the forwarder unconditionally — see cartog_index for rationale.
        if let Some(fwd) = forwarder {
            drop(fwd.tx);
            let _ = fwd.join.await;
        }

        join.map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }
}
