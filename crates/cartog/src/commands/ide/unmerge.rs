//! Config-file unmerge: drop the `cartog` MCP entry per client format.

use anyhow::{Context, Result};
use serde_json::Value;

use super::merge::value_kind;
use super::MergeStrategy;

/// What removing the `cartog` entry does to a config file's content.
#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    /// The entry was present; this is the content without it.
    Removed(String),
    /// No `cartog` entry: the file stays as it is.
    NotWired,
}

/// Remove the `cartog` entry from a client config (Codex: the `codex_section` table).
///
/// Errors when the content does not parse or has an unexpected shape, so the caller
/// leaves the file untouched. An emptied server map stays in place.
pub fn remove_entry(
    existing: &str,
    strategy: MergeStrategy,
    codex_section: &str,
) -> Result<Removal> {
    if existing.trim().is_empty() {
        return Ok(Removal::NotWired);
    }
    match strategy {
        MergeStrategy::McpServers
        | MergeStrategy::CopilotLocal
        | MergeStrategy::McpServersToggle => remove_json_entry(existing, "mcpServers"),
        MergeStrategy::Mcp => remove_json_entry(existing, "mcp"),
        MergeStrategy::ContextServers => remove_json_entry(existing, "context_servers"),
        MergeStrategy::VsCodeServers => remove_json_entry(existing, "servers"),
        MergeStrategy::CodexToml => remove_codex_section(existing, codex_section),
        MergeStrategy::HermesYaml => remove_hermes_entry(existing),
    }
}

fn remove_json_entry(existing: &str, container: &str) -> Result<Removal> {
    let mut root: Value =
        serde_json::from_str(existing.trim()).context("config file is not valid JSON")?;
    let Value::Object(map) = &mut root else {
        anyhow::bail!("config root must be a JSON object");
    };
    let servers = match map.get_mut(container) {
        None | Some(Value::Null) => return Ok(Removal::NotWired),
        Some(Value::Object(servers)) => servers,
        Some(other) => anyhow::bail!(
            "top-level `{container}` is a {} (expected object); refusing to edit",
            value_kind(other)
        ),
    };
    // `shift_remove` keeps the user's order of the remaining servers.
    if servers.shift_remove("cartog").is_none() {
        return Ok(Removal::NotWired);
    }
    Ok(Removal::Removed(format!(
        "{}\n",
        serde_json::to_string_pretty(&root)?
    )))
}

fn remove_codex_section(existing: &str, section: &str) -> Result<Removal> {
    let mut doc: toml_edit::DocumentMut =
        existing.parse().context("config file is not valid TOML")?;
    let Some(servers) = doc.get_mut("mcp_servers") else {
        return Ok(Removal::NotWired);
    };
    let servers = servers.as_table_like_mut().ok_or_else(|| {
        anyhow::anyhow!("top-level `mcp_servers` is not a TOML table; refusing to edit")
    })?;
    let Some(removed) = servers.remove(section) else {
        return Ok(Removal::NotWired);
    };
    if let Some(table) = removed.as_table() {
        keep_leading_comment(&mut doc, table);
    }
    Ok(Removal::Removed(doc.to_string()))
}

/// A comment written above the removed header often labels a group of servers, so
/// it moves onto the next table in the document (or to its end) instead of vanishing.
fn keep_leading_comment(doc: &mut toml_edit::DocumentMut, removed: &toml_edit::Table) {
    let comment = removed
        .decor()
        .prefix()
        .and_then(|p| p.as_str())
        .filter(|p| p.contains('#'))
        .map(str::to_owned);
    let (Some(comment), Some(at)) = (comment, removed.position()) else {
        return;
    };
    let mut headers = Vec::new();
    collect_headers(doc.as_table(), &mut Vec::new(), &mut headers);
    let next = headers
        .into_iter()
        .filter(|(pos, _)| *pos > at)
        .min_by_key(|(pos, _)| *pos)
        .map(|(_, path)| path);
    match next.and_then(|path| table_at(doc.as_table_mut(), &path)) {
        Some(next) => {
            let own = next
                .decor()
                .prefix()
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_owned();
            next.decor_mut().set_prefix(format!("{comment}{own}"));
        }
        None => {
            let trailing = doc.trailing().as_str().unwrap_or("").to_owned();
            doc.set_trailing(format!("{trailing}{comment}"));
        }
    }
}

/// Document position and key path of every table that prints its own header.
fn collect_headers(
    table: &toml_edit::Table,
    path: &mut Vec<String>,
    out: &mut Vec<(usize, Vec<String>)>,
) {
    for (key, item) in table.iter() {
        let Some(child) = item.as_table() else {
            continue;
        };
        path.push(key.to_string());
        if let (false, Some(pos)) = (child.is_implicit(), child.position()) {
            out.push((pos, path.clone()));
        }
        collect_headers(child, path, out);
        path.pop();
    }
}

fn table_at<'a>(
    mut table: &'a mut toml_edit::Table,
    path: &[String],
) -> Option<&'a mut toml_edit::Table> {
    for key in path {
        table = table.get_mut(key)?.as_table_mut()?;
    }
    Some(table)
}

fn remove_hermes_entry(existing: &str) -> Result<Removal> {
    use serde_norway::Value;

    let mut root: Value =
        serde_norway::from_str(existing).context("config file is not valid YAML")?;
    let map = match &mut root {
        Value::Null => return Ok(Removal::NotWired),
        Value::Mapping(map) => map,
        _ => anyhow::bail!("config root must be a YAML mapping"),
    };
    let servers = match map.get_mut("mcp_servers") {
        None | Some(Value::Null) => return Ok(Removal::NotWired),
        Some(Value::Mapping(servers)) => servers,
        Some(_) => {
            anyhow::bail!("top-level `mcp_servers` is not a YAML mapping; refusing to edit")
        }
    };
    if servers.shift_remove("cartog").is_none() {
        return Ok(Removal::NotWired);
    }
    Ok(Removal::Removed(
        serde_norway::to_string(&root).context("serialize YAML without the cartog entry")?,
    ))
}
