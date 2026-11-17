//! MCP management tool: cartog_update.

use rmcp::{tool, tool_router, ErrorData as McpError};

use crate::types::*;
use crate::*;

/// The cartog binary installed where this process was started from.
///
/// After an in-place swap, Linux reports a running process's executable at its
/// renamed path (`<bin>.old`, or `<bin>.old (deleted)` once a later swap replaced
/// that), so spawning `current_exe()` would run the old binary or fail. The file
/// now at `<bin>` is the installed one.
pub(crate) fn installed_exe(running: &std::path::Path) -> std::path::PathBuf {
    let Some(name) = running.file_name().and_then(|n| n.to_str()) else {
        return running.to_path_buf();
    };
    let name = name.strip_suffix(" (deleted)").unwrap_or(name);
    let name = name.strip_suffix(".old").unwrap_or(name);
    running.with_file_name(name)
}

#[tool_router(router = manage_router, vis = "pub(crate)")]
impl CartogServer {
    /// Arm a deferred cartog self-update.
    ///
    /// Deliberately NOT gated by `refuse_if_read_only`: arming writes the
    /// machine-level state file, not the index DB, so a read-only secondary
    /// may arm just like the primary. The guard exists only to stop a
    /// secondary from writing the DB — adding it here would be a category
    /// error.
    ///
    /// Never swaps the binary in-session: this server IS the live peer that
    /// `cartog self update` refuses to overwrite. It shells out to
    /// `self update --defer`, which records the target and exits without
    /// touching the binary; the boundary swap happens at SessionEnd.
    #[tool(
        description = "Arm a deferred cartog self-update. Does NOT upgrade in this session — the running server keeps its current binary; with the Claude Code plugin the new version is applied when this session ends; other clients have no hook that applies it, so the result's message gives a command for the user to run in their own terminal. Use when the user confirms they want to update cartog. When cartog is installed as a Claude Code plugin, this arms the plugin's PINNED version (read from the plugin manifest at server start); otherwise it arms the latest stable release. Not for: indexing or search. Returns: {current, target, status, apply, message}.",
        annotations(
            title = "Update cartog",
            read_only_hint = false,
            destructive_hint = false,
            // Not idempotent: a latest-release arm re-fetches the tag and each
            // arm rewrites armed_at / last_update_check timestamps, so repeated
            // calls change state.toml even when the armed target is unchanged.
            idempotent_hint = false,
            open_world_hint = true
        ),
        output_schema = output_schema_for::<UpdateResult>()
    )]
    pub(crate) async fn cartog_update(&self) -> Result<CallToolResult, McpError> {
        let pin = self.plugin_pin.as_ref().map(|p| p.version.clone());
        let plugin = pin.is_some();
        tokio::task::spawn_blocking(move || {
            debug!("update (arm deferred)");
            let exe = std::env::current_exe()
                .map(|running| installed_exe(&running))
                .map_err(|e| mcp_err(format!("cannot resolve cartog binary: {e}")))?;
            // Arm the plugin's pinned version when the binary resolved one so we
            // can't overshoot the pin; fall back to latest stable otherwise.
            let mut args = vec!["self", "update", "--defer", "--json"];
            if let Some(ref v) = pin {
                args.push("--to");
                args.push(v);
            }
            let output = std::process::Command::new(exe)
                .args(&args)
                .output()
                .map_err(|e| mcp_err(format!("failed to run self update --defer: {e}")))?;

            let result = parse_arm_output(&output, plugin);
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| mcp_err(format!("serialization failed: {e}")))?;
            let structured = serde_json::to_value(&result).ok();
            Ok(success_result(json, structured))
        })
        .await
        .map_err(|e| mcp_err(format!("task join failed: {e}")))?
    }
}
