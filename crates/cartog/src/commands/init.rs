//! `cartog init` — project configuration bootstrap.
//!
//! Scaffolds `.cartog.toml` (if absent) and prints the recommended next steps:
//! run `cartog ide` to wire editor MCP entries, run `cartog index` to build the
//! graph. Keeping the three verbs separate lets users edit `.cartog.toml`
//! before any heavy work and skip MCP entirely if they're CLI-only.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

const TOML_TEMPLATE: &str = r##"# .cartog.toml — project-level configuration for cartog
#
# All sections are commented out by default; defaults apply. Uncomment a
# section and set the keys you want to override. Run `cartog config` to
# print the active configuration. See https://github.com/jrollin/cartog
# for the schema reference.

# [database]
# path = ".cartog/db.sqlite"

# [embedding]
# provider = "local"             # "local" (default) | "ollama" | "openai"
#
# For an OpenAI-compatible /v1 endpoint (OpenAI, Mistral, Voyage, Jina, OVHcloud,
# or a local server like Ollama /v1, LM Studio, vLLM), set provider = "openai"
# and uncomment the [embedding.openai] block below. The API key is read from an
# env var, never stored here; leave the var unset for keyless local endpoints.
# model = "text-embedding-3-small"

# [embedding.openai]                            # only used when provider = "openai"
# base_url    = "https://api.openai.com/v1"     # or http://localhost:11434/v1 (Ollama), etc.
# api_key_env = "OPENAI_API_KEY"                # env var NAME, not the key itself

# [reranker]
# enabled = true

# [rag]
# fts_weight = 0.5
# vector_weight = 0.5

# [security]
# Redact common secret patterns (API keys, tokens, JWTs) from stored symbol
# text. On by default. Sensitive files (.env, *.pem, id_rsa, ...) are always
# excluded regardless of this setting.
# redact_secrets = true
"##;

#[derive(Debug, Serialize)]
struct InitReport {
    toml: TomlStep,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct TomlStep {
    path: String,
    status: TomlStatus,
    message: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum TomlStatus {
    Created,
    Unchanged,
    Skipped,
}

/// Scaffold a `.cartog.toml` template in the current directory and print the
/// recommended next steps (`cartog ide`, `cartog index`).
///
/// `dry_run`: report what would be written without touching the filesystem.
/// `json`: emit the structured `InitReport` instead of human-readable text.
///
/// Returns `Err` only on an unexpected I/O failure; a refused scaffold (e.g.
/// `.cartog.toml` already present) is reported in the result, not as an error.
pub fn cmd_init(dry_run: bool, json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let toml = scaffold_toml(&cwd, dry_run)?;
    let report = InitReport { toml, dry_run };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_human(&report));
        print_next_steps(&report);
    }

    if matches!(report.toml.status, TomlStatus::Skipped) {
        std::process::exit(1);
    }
    Ok(())
}

fn render_human(report: &InitReport) -> String {
    let icon = match report.toml.status {
        TomlStatus::Created | TomlStatus::Unchanged => "+",
        TomlStatus::Skipped => "!",
    };
    format!(
        "{} .cartog.toml ({}): {}\n",
        icon, report.toml.path, report.toml.message
    )
}

/// Print the three-step bootstrap reminder. `init` only handles config; the
/// reminder makes the rest of the flow discoverable without users having to
/// search the docs.
fn print_next_steps(report: &InitReport) {
    if report.dry_run {
        println!("\nDry run only. Re-run without --dry-run to apply.");
        return;
    }

    println!("\nNext steps:");
    if matches!(report.toml.status, TomlStatus::Created) {
        println!(
            "  1. Edit .cartog.toml if you want to change defaults (DB path, embedding provider)."
        );
        println!("  2. Run `cartog ide` to wire cartog into your editor(s).");
        println!("  3. Run `cartog index` to build the code graph.");
    } else {
        println!("  1. Run `cartog ide` to wire (or re-wire) cartog into your editor(s).");
        println!("  2. Run `cartog index` to build (or refresh) the code graph.");
    }
}

fn scaffold_toml(cwd: &Path, dry_run: bool) -> Result<TomlStep> {
    let path = cwd.join(".cartog.toml");
    let path_str = path.display().to_string();

    if path.exists() {
        return Ok(TomlStep {
            path: path_str,
            status: TomlStatus::Unchanged,
            message: "already present, left untouched".into(),
        });
    }

    if dry_run {
        return Ok(TomlStep {
            path: path_str,
            status: TomlStatus::Created,
            message: "would create from template".into(),
        });
    }

    match fs::write(&path, TOML_TEMPLATE) {
        Ok(()) => Ok(TomlStep {
            path: path_str,
            status: TomlStatus::Created,
            message: "created from template".into(),
        }),
        Err(e) => Ok(TomlStep {
            path: path_str,
            status: TomlStatus::Skipped,
            message: format!("could not create: {e}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn toml_scaffold_writes_template_when_absent() {
        let tmp = TempDir::new().unwrap();
        let step = scaffold_toml(tmp.path(), false).unwrap();
        assert_eq!(step.status, TomlStatus::Created);
        let body = fs::read_to_string(tmp.path().join(".cartog.toml")).unwrap();
        assert!(body.contains("[database]"));
        assert!(body.contains("[embedding]"));
    }

    #[test]
    fn toml_template_has_no_duplicate_table_headers() {
        // Each [table] / [table.sub] header must appear once, so uncommenting
        // sibling examples can't produce a duplicate-table TOML parse error.
        let mut headers = Vec::new();
        for line in TOML_TEMPLATE.lines() {
            // Strip the comment marker, then any trailing inline comment, so a
            // header written as `[embedding.openai]  # note` is still detected.
            let t = line.trim_start_matches(['#', ' ']);
            let t = t.split('#').next().unwrap_or(t).trim();
            if t.starts_with('[') && t.ends_with(']') {
                headers.push(t);
            }
        }
        let mut seen = std::collections::HashSet::new();
        for h in &headers {
            assert!(seen.insert(*h), "duplicate table header in template: {h}");
        }
    }

    #[test]
    fn toml_scaffold_preserves_existing_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".cartog.toml"), "# pre-existing\n").unwrap();
        let step = scaffold_toml(tmp.path(), false).unwrap();
        assert_eq!(step.status, TomlStatus::Unchanged);
        let body = fs::read_to_string(tmp.path().join(".cartog.toml")).unwrap();
        assert_eq!(body, "# pre-existing\n");
    }

    #[test]
    fn toml_scaffold_dry_run_does_not_write() {
        let tmp = TempDir::new().unwrap();
        let step = scaffold_toml(tmp.path(), true).unwrap();
        assert_eq!(step.status, TomlStatus::Created);
        assert!(!tmp.path().join(".cartog.toml").exists());
    }
}
