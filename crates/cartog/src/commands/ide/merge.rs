//! Config-file merge: insert the `cartog` MCP entry per client format.

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::path::Path;

use super::{Action, MergeOutcome, MergeStrategy};

/// Merge a `cartog` server entry into a client's config file (JSON or TOML).
///
/// Dispatches to the per-format implementation based on `strategy`. The Codex
/// TOML branch uses `toml_edit` for round-trip-safe edits that preserve
/// formatting and comments in the rest of the file. JSON branches use
/// `serde_json::Value` and pretty-print.
pub fn merge_entry(
    existing: Option<&str>,
    strategy: MergeStrategy,
    args: &[String],
) -> Result<MergeOutcome> {
    if strategy == MergeStrategy::CodexToml {
        return merge_codex_toml(existing, args, "cartog");
    }
    if strategy == MergeStrategy::HermesYaml {
        return merge_hermes_yaml(existing, args);
    }
    let trimmed = existing.map(str::trim).unwrap_or("");
    let parsed_prev: Option<Value> = if trimmed.is_empty() {
        None
    } else {
        let v = serde_json::from_str(trimmed).context("config file is not valid JSON")?;
        match &v {
            Value::Object(_) => Some(v),
            _ => anyhow::bail!("config root must be a JSON object"),
        }
    };

    let mut root = match &parsed_prev {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    apply_strategy(&mut root, strategy, args)?;
    let new_json = format!("{}\n", serde_json::to_string_pretty(&Value::Object(root))?);

    let action = match (existing, parsed_prev) {
        (None, _) => Action::Created,
        (Some(_), Some(prev)) => {
            let canonical_prev = format!("{}\n", serde_json::to_string_pretty(&prev)?);
            if canonical_prev == new_json {
                Action::Unchanged
            } else {
                Action::Updated
            }
        }
        // Empty / whitespace-only file: behave like Created.
        (Some(_), None) => Action::Updated,
    };

    Ok(MergeOutcome { new_json, action })
}

fn apply_strategy(
    root: &mut Map<String, Value>,
    strategy: MergeStrategy,
    args: &[String],
) -> Result<()> {
    match strategy {
        MergeStrategy::McpServers => {
            let servers = ensure_object(root, "mcpServers")?;
            servers.insert(
                "cartog".into(),
                json!({ "command": "cartog", "args": args }),
            );
        }
        MergeStrategy::Mcp => {
            let mcp = ensure_object(root, "mcp")?;
            let mut command_array: Vec<Value> = Vec::with_capacity(1 + args.len());
            command_array.push(Value::String("cartog".into()));
            command_array.extend(args.iter().cloned().map(Value::String));
            mcp.insert(
                "cartog".into(),
                json!({
                    "type": "local",
                    "command": Value::Array(command_array),
                    "enabled": true,
                }),
            );
        }
        MergeStrategy::ContextServers => {
            let servers = ensure_object(root, "context_servers")?;
            servers.insert(
                "cartog".into(),
                json!({ "command": "cartog", "args": args }),
            );
        }
        MergeStrategy::VsCodeServers => {
            let servers = ensure_object(root, "servers")?;
            servers.insert(
                "cartog".into(),
                json!({ "type": "stdio", "command": "cartog", "args": args }),
            );
        }
        // Dispatched out of `merge_entry` before this function is called;
        // return an error rather than panic if the dispatch is ever bypassed.
        MergeStrategy::CodexToml => {
            anyhow::bail!("internal: CodexToml must go through merge_codex_toml")
        }
        MergeStrategy::HermesYaml => {
            anyhow::bail!("internal: HermesYaml must go through merge_hermes_yaml")
        }
    }
    Ok(())
}

/// Merge a `[mcp_servers.<section>]` table into a Codex `config.toml` document.
///
/// Uses `toml_edit` so the rest of the user's Codex config (other servers,
/// hooks, comments, formatting) survives untouched. The section name is the
/// caller-supplied `section` (typically `cartog-<project-slug>-<hash8>` so
/// multiple cartog projects coexist in `~/.codex/config.toml`).
pub(super) fn merge_codex_toml(
    existing: Option<&str>,
    args: &[String],
    section: &str,
) -> Result<MergeOutcome> {
    use toml_edit::{value, Array, DocumentMut, Item, Table};

    let mut doc: DocumentMut = match existing {
        None => DocumentMut::new(),
        Some(s) if s.trim().is_empty() => DocumentMut::new(),
        Some(s) => s
            .parse::<DocumentMut>()
            .context("config file is not valid TOML")?,
    };

    let canonical_prev = doc.to_string();

    // Ensure top-level `mcp_servers` is a table (dotted-key syntax). Refuse
    // to overwrite if the user put something else (string, array, etc.) there.
    if !doc.contains_key("mcp_servers") {
        let mut t = Table::new();
        t.set_implicit(true);
        doc["mcp_servers"] = Item::Table(t);
    }
    let mcp_servers = doc["mcp_servers"].as_table_mut().ok_or_else(|| {
        anyhow::anyhow!("top-level `mcp_servers` is not a TOML table; refusing to overwrite")
    })?;

    // If our section already exists, refuse to clobber a non-table value
    // (some past hand-edit). A pre-existing table is fine — we replace it.
    if let Some(existing_section) = mcp_servers.get(section) {
        if !existing_section.is_table() {
            anyhow::bail!(
                "section `[mcp_servers.{section}]` exists but is not a table; refusing to overwrite"
            );
        }
    }

    let mut entry = Table::new();
    entry["command"] = value("cartog");
    let mut arr = Array::new();
    for a in args {
        arr.push(a.clone());
    }
    entry["args"] = value(arr);
    mcp_servers.insert(section, Item::Table(entry));

    let new_json = doc.to_string();

    let action = match existing {
        None => Action::Created,
        Some(_) => {
            if new_json == canonical_prev {
                Action::Unchanged
            } else {
                Action::Updated
            }
        }
    };

    Ok(MergeOutcome { new_json, action })
}

/// Merge a `cartog` entry into Hermes' `mcp_servers` YAML map (`~/.hermes/config.yaml`).
///
/// Shared config file: only the `cartog` server is upserted; every other key and
/// server is preserved. Hermes' own CLI rewrites this file without preserving
/// comments, so a value round-trip (parse → upsert → re-serialize) matches its
/// behavior and won't surprise users.
fn merge_hermes_yaml(existing: Option<&str>, args: &[String]) -> Result<MergeOutcome> {
    use serde_norway::{Mapping, Value};

    let trimmed = existing.map(str::trim).unwrap_or("");
    let mut root: Value = if trimmed.is_empty() {
        Value::Mapping(Mapping::new())
    } else {
        let v: Value = serde_norway::from_str(trimmed).context("config file is not valid YAML")?;
        match v {
            Value::Mapping(_) => v,
            // A null doc (e.g. just comments) parses to Null; treat as empty map.
            Value::Null => Value::Mapping(Mapping::new()),
            _ => anyhow::bail!("config root must be a YAML mapping"),
        }
    };

    let canonical_prev = serde_norway::to_string(&root).context("re-serialize previous YAML")?;

    let map = root
        .as_mapping_mut()
        .expect("root normalized to a mapping above");
    let servers_key = Value::String("mcp_servers".into());
    let servers = map
        .entry(servers_key)
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let servers = servers.as_mapping_mut().ok_or_else(|| {
        anyhow::anyhow!("top-level `mcp_servers` is not a YAML mapping; refusing to overwrite")
    })?;

    let mut entry = Mapping::new();
    entry.insert(
        Value::String("command".into()),
        Value::String("cartog".into()),
    );
    entry.insert(
        Value::String("args".into()),
        Value::Sequence(args.iter().map(|a| Value::String(a.clone())).collect()),
    );
    servers.insert(Value::String("cartog".into()), Value::Mapping(entry));

    // serde_norway terminates with a newline, so no manual `\n` like the JSON branch.
    let new_json = serde_norway::to_string(&root).context("serialize merged YAML")?;

    let action = match existing {
        None => Action::Created,
        Some(_) if trimmed.is_empty() => Action::Updated,
        Some(_) if new_json == canonical_prev => Action::Unchanged,
        Some(_) => Action::Updated,
    };

    Ok(MergeOutcome { new_json, action })
}

/// Build a Codex section name unique to this project. Codex shares one TOML
/// file across all projects, so we suffix with a short hash of the absolute
/// path to keep multiple cartog setups from clobbering each other.
pub(super) fn codex_section_name(project_root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let abs = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let project_slug = abs
        .file_name()
        .and_then(|s| s.to_str())
        .map(slugify)
        .unwrap_or_else(|| "project".to_string());
    let mut h = Sha256::new();
    h.update(abs.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    let hash_short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("cartog-{project_slug}-{hash_short}")
}

fn slugify(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Borrow (and lazily create) the top-level Object at `key`. If the slot is
/// occupied by a non-Object value (string, array, number, bool) we refuse to
/// overwrite it — that would silently destroy whatever the user had. The
/// error surfaces as `Skipped` at the caller, so the file is left untouched.
/// `null` is treated like an absent key and replaced with an empty object.
fn ensure_object<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    // Reject non-Object, non-Null values up-front so we never touch them.
    if let Some(existing) = root.get(key) {
        if !matches!(existing, Value::Object(_) | Value::Null) {
            anyhow::bail!(
                "top-level `{key}` is a {} (expected object); refusing to overwrite",
                value_kind(existing),
            );
        }
    }
    // Replace Null with an empty object; leave an existing Object alone.
    match root.get(key) {
        None | Some(Value::Null) => {
            root.insert(key.into(), Value::Object(Map::new()));
        }
        _ => {}
    }
    // SAFETY: prior arms ensure the slot is now `Value::Object`. Map back via
    // `get_mut` + `as_object_mut` so a future maintainer doesn't have to think
    // about an `unreachable!` branch.
    let slot = root
        .get_mut(key)
        .expect("just inserted or kept an existing entry");
    slot.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("internal: ensure_object slot is not Object after fixup"))
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
