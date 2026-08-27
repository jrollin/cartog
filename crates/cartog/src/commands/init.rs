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
#
# ollama/openai only: in-flight HTTP embed requests (1..16, default 4). Env
# CARTOG_EMBED_CONCURRENCY overrides; ignored for local. (Currently gated: both
# providers run serially until a live batch-composition parity test passes.)
# max_concurrent_requests = 4
#
# Watcher auto-embed under `serve --watch` / `watch`. Omit for auto-detect (embed
# only if the repo already has embeddings). Precedence: CARTOG_WATCH_RAG > this > --rag.
# auto_embed = true

# [embedding.openai]                            # only used when provider = "openai"
# base_url    = "https://api.openai.com/v1"     # or http://localhost:11434/v1 (Ollama), etc.
# api_key_env = "OPENAI_API_KEY"                # env var NAME, not the key itself

# [lsp]
# Max LSP servers run concurrently during the indexer's edge-resolution pass.
# 0 or omitted = auto (min(languages, 4)). Each server is RAM-heavy
# (rust-analyzer ~1-2GB). Env CARTOG_LSP_MAX_SERVERS overrides; 1 = serial.
# max_concurrent_servers = 2
# Per-language server override (Dockerized server, custom binary, ...):
# [lsp.dart]
# command = ["docker", "run", "--rm", "-i", "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-dart:stable"]

# [reranker]
# enabled = true                 # false turns re-ranking off (wins over `provider`)
# provider = "local"             # "local" (default) or "none"
# model    = "jinaai/jina-reranker-v1-turbo-en"

# [rag]
# retrieval_multiplier = 3  # over-retrieve N× before fusion (default: 3)
# rerank_max = 50            # max candidates sent to reranker (default: 50)

# [remote]
# Opt in to push/pull of the prebuilt index over an S3-compatible bucket.
# Credentials are NEVER read from here — use env vars / ~/.aws profile / IMDS.
# url        = "s3://my-team-bucket/cartog/main"
# endpoint   = "https://minio.example.com"   # MinIO / R2 / floci
# path_style = true                          # set true for most non-AWS endpoints

# [security]
# Redact common secret patterns (API keys, tokens, JWTs) from stored symbol
# text. On by default. Sensitive files (.env, *.pem, id_rsa, ...) are always
# excluded regardless of this setting.
# redact_secrets = true

# [index]
# cartog honors .gitignore (incl. nested) and .cartogignore by default. These
# globs skip ADDITIONAL repo-root-relative paths (matched dirs are pruned), on
# top of .gitignore and the built-in dependency/build-dir prune list.
# exclude = ["vendor/**", "**/*.generated.*"]
# Set false to index files git ignores (e.g. committed generated code); the
# prune list and `exclude` still apply.
# respect_gitignore = true
# Parse worker threads. 0 or omitted = auto (CPU count); clamped 1..=64; use 1
# for serial. Cap it on low-CPU hosts. Overridden by CARTOG_JOBS and --jobs N.
# jobs = 4
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

    /// Uncomment every commented directive in a template, dropping prose.
    fn uncomment_template(src: &str) -> String {
        let mut out = String::new();
        for line in src.lines() {
            let t = line.trim_start_matches(['#', ' ']).trim();
            // Strip a trailing inline comment so `[embedding.openai]  # note`
            // is still recognized as a header.
            let t = if t.starts_with('[') {
                t.split('#').next().unwrap_or(t).trim()
            } else {
                t
            };
            let is_header = t.starts_with('[') && t.ends_with(']');
            // An assignment, not prose that happens to contain '=' ("0 or
            // omitted = auto"): the text left of the first '=' must be a single
            // bare TOML key.
            let is_assignment = !t.starts_with('[')
                && t.split_once('=').is_some_and(|(k, _)| {
                    let k = k.trim();
                    !k.is_empty()
                        && k.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
                });
            if is_header || is_assignment {
                out.push_str(t);
                out.push('\n');
            }
        }
        out
    }

    /// Every key a user-facing template teaches must deserialize into a real
    /// config field. `[reranker] enabled` shipped in both templates for
    /// releases while no such field existed, so anyone who followed them was
    /// silently left with the cross-encoder loaded. Uncommenting and parsing
    /// under `deny_unknown_fields` is what catches that class of drift.
    ///
    /// Covers `.cartog.toml.example` too — a second shipped template, and the
    /// only place `[embedding.local]` / `[embedding.ollama]` appear.
    #[test]
    fn every_template_key_parses_into_a_real_config_field() {
        let example = fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(".cartog.toml.example"),
        )
        .expect("`.cartog.toml.example` must exist at the repo root");

        for (name, src) in [
            ("init.rs TOML_TEMPLATE", TOML_TEMPLATE),
            (".cartog.toml.example", example.as_str()),
        ] {
            let rendered = uncomment_template(src);
            let cfg: Result<crate::config::CartogConfig, _> = toml::from_str(&rendered);
            assert!(
                cfg.is_ok(),
                "{name}: key is not a real config field: {}\n--- rendered ---\n{rendered}",
                cfg.unwrap_err()
            );
        }
    }

    /// A freshly-scaffolded config must load clean — no unknown-section
    /// warnings on a user's very first run.
    #[test]
    fn scaffolded_template_has_no_unknown_sections() {
        let tmp = TempDir::new().unwrap();
        scaffold_toml(tmp.path(), false).unwrap();
        let body = fs::read_to_string(tmp.path().join(".cartog.toml")).unwrap();
        let raw: toml::value::Table = toml::from_str(&body).unwrap();
        assert!(
            crate::config::unknown_sections(&raw).is_empty(),
            "template emits unknown sections: {:?}",
            crate::config::unknown_sections(&raw)
        );
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
