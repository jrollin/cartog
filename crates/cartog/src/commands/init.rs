//! `cartog init` — one-step project bootstrap.
//!
//! Runs `cartog index` on the current directory, scaffolds a `.cartog.toml`
//! template if absent, and wires project-scoped MCP entries (Claude Code's
//! `.mcp.json` and Cursor's `.cursor/mcp.json`) by delegating to
//! [`super::ide::run_ide`] with `scope = Project`.

use std::fs;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};
use cartog_db::Database;
use cartog_indexer::{self as indexer, IndexResult};
use serde::Serialize;

use crate::cli::IdeScope;

use super::ide;

const TOML_TEMPLATE: &str = r##"# .cartog.toml — project-level configuration for cartog
#
# All sections are commented out by default; defaults apply. Uncomment a
# section and set the keys you want to override. Run `cartog config` to
# print the active configuration. See https://github.com/jrollin/cartog
# for the schema reference.

# [database]
# path = ".cartog/db.sqlite"

# [embedding]
# provider = "local"

# [reranker]
# enabled = true

# [rag]
# fts_weight = 0.5
# vector_weight = 0.5
"##;

#[derive(Debug, Serialize)]
struct InitReport {
    index: IndexStep,
    toml: TomlStep,
    ide: ide::IdeReport,
    summary: InitSummary,
}

#[derive(Debug, Serialize)]
struct IndexStep {
    status: IndexStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<IndexResult>,
    /// True when `--dry-run` was set: the message describes what *would*
    /// happen rather than what did. Mirrors the convention used by `TomlStatus`
    /// and `IdeStatus`, where `Created` + `dry_run` means "would create".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    message: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum IndexStatus {
    Indexed,
    Skipped,
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

#[derive(Debug, Serialize, Default)]
struct InitSummary {
    errors: usize,
}

pub fn cmd_init(
    db_path: &Path,
    yes: bool,
    dry_run: bool,
    no_index: bool,
    no_watch: bool,
    json: bool,
    embedding_dim: usize,
) -> Result<()> {
    let interactive = !yes && !dry_run && !json && std::io::stdin().is_terminal();
    let cwd = std::env::current_dir()?;

    let index = run_index_step(db_path, no_index, dry_run, embedding_dim)?;
    let toml = scaffold_toml(&cwd, dry_run)?;

    let homes = ide::HomeDirs::detect();
    let ide_report = ide::run_ide(
        None,
        IdeScope::Project,
        interactive,
        dry_run,
        no_watch,
        &cwd,
        &homes,
    )?;

    let summary = InitSummary {
        errors: ide_report.summary.error,
    };

    let report = InitReport {
        index,
        toml,
        ide: ide_report,
        summary,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_human(&report));
        print_next_steps(&report);
    }

    if report.summary.errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn run_index_step(
    db_path: &Path,
    no_index: bool,
    dry_run: bool,
    embedding_dim: usize,
) -> Result<IndexStep> {
    if no_index {
        return Ok(IndexStep {
            status: IndexStatus::Skipped,
            result: None,
            dry_run: false,
            message: "skipped via --no-index".into(),
        });
    }
    if dry_run {
        return Ok(IndexStep {
            status: IndexStatus::Indexed,
            result: None,
            dry_run: true,
            message: "would run `cartog index .`".into(),
        });
    }
    let db = Database::open(db_path, embedding_dim).context("Failed to open cartog database")?;
    let result = indexer::index_directory(&db, Path::new("."), false, true)?;
    let message = format!(
        "{} files indexed ({} unchanged), {} symbols, {} edges",
        result.files_indexed,
        result.files_skipped,
        result.symbols_added + result.symbols_modified + result.symbols_unchanged,
        result.edges_added,
    );
    Ok(IndexStep {
        status: IndexStatus::Indexed,
        result: Some(result),
        dry_run: false,
        message,
    })
}

fn render_human(report: &InitReport) -> String {
    let mut out = String::new();
    let idx = &report.index;
    let icon = match idx.status {
        IndexStatus::Indexed => "+",
        IndexStatus::Skipped => "!",
    };
    out.push_str(&format!("{icon} index: {}\n", idx.message));
    let toml_icon = match report.toml.status {
        TomlStatus::Created | TomlStatus::Unchanged => "+",
        TomlStatus::Skipped => "!",
    };
    out.push_str(&format!(
        "{} .cartog.toml ({}): {}\n",
        toml_icon, report.toml.path, report.toml.message
    ));
    out.push_str(&report.ide.render_human());
    out
}

/// Markdown block users can paste into `AGENTS.md`, `CLAUDE.md`, or any
/// agent-instruction file to prime LLM clients on cartog's tools. Kept short
/// (under 30 lines) so it doesn't drown the user's own prose. Cartog never
/// writes this file itself — the user pastes it, owning their AGENTS.md.
const AGENTS_SNIPPET: &str = "\
## Code navigation (cartog)

This project is indexed by cartog. Prefer cartog's MCP tools over grep/find for
code navigation — they return structured results with ~10× lower token cost.

- `cartog_map` — orient yourself in the codebase (file list + top symbols)
- `cartog_outline <file>` — file structure before reading
- `cartog_search <name>` — find a symbol by name
- `cartog_rag_search <query>` — concept/keyword search across code
- `cartog_refs <name>` — every usage of a symbol
- `cartog_callees <name>` — what a function calls
- `cartog_impact <name>` — what breaks if you change it
- `cartog_changes` — symbols touched by recent commits

Run `cartog_index` once at session start if results are empty or stale.
";

/// Print a one-or-two-line follow-up to help users know what to do next.
fn print_next_steps(report: &InitReport) {
    let s = &report.ide.summary;
    if report.index.dry_run {
        println!("\nDry run only. Re-run without --dry-run to apply.");
        return;
    }
    if s.created + s.updated > 0 {
        println!("\nNext: open your editor and try a cartog tool (e.g. `search`, `refs`).");
        println!("\nOptional — paste this into AGENTS.md / CLAUDE.md so agents know cartog is available:");
        println!();
        for line in AGENTS_SNIPPET.lines() {
            println!("  {line}");
        }
        println!("\nRe-run `cartog ide` after installing more editors.");
    } else if s.skipped > 0 && s.created + s.updated + s.unchanged == 0 {
        println!(
            "\nNo MCP clients were configured. Install an MCP-aware editor \
             (Claude Code, Cursor, Windsurf, Zed) and re-run `cartog ide`."
        );
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
