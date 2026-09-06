//! `cartog config`: render the resolved configuration with default-value markers.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::config::{CartogConfig, McpConfig};

/// Display the current configuration with default-value indicators.
pub fn cmd_config(
    config: &CartogConfig,
    config_path: Option<&Path>,
    config_rejected: bool,
    db_path: &Path,
    json: bool,
) -> Result<()> {
    // When the config file was found but rejected at parse time, `config`
    // is the empty default — displaying it as the active config would
    // silently lie. Show an explicit error and bail with the path so the
    // user knows which file to fix.
    if config_rejected {
        // Invariant: ConfigLoad::Rejected always carries a path, and main
        // propagates it as `config_path`. Bail explicitly so this surfaces
        // even if the invariant ever changes.
        let p = config_path
            .ok_or_else(|| anyhow::anyhow!("config rejected but path missing — invariant break"))?;
        anyhow::bail!(
            "configuration file {} was rejected (see earlier stderr for the \
             underlying reason). `cartog config` cannot display a meaningful \
             view until the file is fixed.",
            p.display()
        );
    }
    use crate::config::{
        DEFAULT_EMBEDDING_PROVIDER, DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_MODEL,
        DEFAULT_RERANKER_PROVIDER,
    };

    let embed = config.embedding.as_ref();
    let ollama = embed.and_then(|e| e.ollama.as_ref());
    let local = embed.and_then(|e| e.local.as_ref());
    let reranker = config.reranker.as_ref();
    let rag = config.rag.as_ref();
    let security = config.security.as_ref();
    let project = config.project.as_ref();
    let tuning_defaults = cartog_rag::search::SearchTuning::default();
    // `to_search_tuning()` applies the clamps (retrieval_multiplier.max(1),
    // rerank_min.min(rerank_max)) so what we show matches what the search
    // pipeline will actually use.
    let effective_tuning = rag.map(|r| r.to_search_tuning()).unwrap_or(tuning_defaults);

    let rag_value = |set: Option<u32>, effective: u32, default: u32| ValueDisplay {
        value: effective.to_string(),
        is_default: set.is_none(),
        default: default.to_string(),
    };

    let display = ConfigDisplay {
        config_file: config_path.map(|p| p.to_string_lossy().into_owned()),
        db_path: db_path.to_string_lossy().into_owned(),
        project: resolve_project_display(project, config_path),
        mcp: McpDisplay {
            federated: config.mcp.as_ref().is_some_and(McpConfig::federated),
            is_default: config.mcp.as_ref().and_then(|m| m.federated).is_none(),
        },
        embedding: EmbeddingDisplay {
            provider: ValueDisplay {
                value: embed.map_or(DEFAULT_EMBEDDING_PROVIDER.into(), |e| {
                    e.provider().to_string()
                }),
                is_default: embed.map_or(true, |e| e.provider.is_none()),
                default: DEFAULT_EMBEDDING_PROVIDER.into(),
            },
            model: embed.and_then(|e| e.model.clone()),
            dimension: embed.and_then(|e| e.dimension),
            local: LocalEmbeddingDisplay {
                query_prefix: local.and_then(|l| l.query_prefix.clone()),
                document_prefix: local.and_then(|l| l.document_prefix.clone()),
            },
            ollama: OllamaDisplay {
                base_url: ValueDisplay {
                    value: ollama
                        .map_or(DEFAULT_OLLAMA_BASE_URL.into(), |o| o.base_url().to_string()),
                    is_default: ollama.map_or(true, |o| o.base_url.is_none()),
                    default: DEFAULT_OLLAMA_BASE_URL.into(),
                },
                model: ValueDisplay {
                    value: ollama.map_or(DEFAULT_OLLAMA_MODEL.into(), |o| o.model().to_string()),
                    is_default: ollama.map_or(true, |o| o.model.is_none()),
                    default: DEFAULT_OLLAMA_MODEL.into(),
                },
            },
        },
        reranker: RerankerDisplay {
            provider: ValueDisplay {
                value: reranker.map_or(DEFAULT_RERANKER_PROVIDER.into(), |r| {
                    r.provider().to_string()
                }),
                // `enabled = false` resolves the provider to "none"; without it
                // in the check, an explicit opt-out reads back as "(default)".
                is_default: reranker.map_or(true, |r| r.provider.is_none() && r.enabled.is_none()),
                default: DEFAULT_RERANKER_PROVIDER.into(),
            },
        },
        rag: RagDisplay {
            retrieval_multiplier: rag_value(
                rag.and_then(|r| r.retrieval_multiplier),
                effective_tuning.retrieval_multiplier,
                tuning_defaults.retrieval_multiplier,
            ),
            retrieval_floor: rag_value(
                rag.and_then(|r| r.retrieval_floor),
                effective_tuning.retrieval_floor,
                tuning_defaults.retrieval_floor,
            ),
            rerank_max: rag_value(
                rag.and_then(|r| r.rerank_max),
                effective_tuning.rerank_max,
                tuning_defaults.rerank_max,
            ),
            rerank_min: rag_value(
                rag.and_then(|r| r.rerank_min),
                effective_tuning.rerank_min,
                tuning_defaults.rerank_min,
            ),
        },
        security: SecurityDisplay {
            redact_secrets: ValueDisplay {
                value: security.map_or(true, |s| s.redact_secrets()).to_string(),
                is_default: security.map_or(true, |s| s.redact_secrets.is_none()),
                default: "true".into(),
            },
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&display)?);
    } else {
        print!("{}", format_config_human(&display));
    }
    Ok(())
}

fn format_value(v: &ValueDisplay) -> String {
    if v.is_default {
        format!("{} (default)", v.value)
    } else {
        format!("{} (default: {})", v.value, v.default)
    }
}

/// Resolve `[project]` exactly as the registry writers do (config, then the
/// README fallback), so `cartog config` shows what `projects list` will show.
///
/// The root is the config file's directory; with no config file, cwd. A
/// missing description is rendered as `(none)` rather than `-`: it is a state
/// the user is meant to notice and fill in.
fn resolve_project_display(
    project: Option<&crate::config::ProjectConfig>,
    config_path: Option<&Path>,
) -> ProjectDisplay {
    let root = config_path
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let declared = super::shared::declared_for(project, &root);
    let (name, name_source) = match declared.name {
        Some(n) => (n, "config"),
        None => (directory_name(&root), "directory"),
    };
    let description_source = declared.description.as_ref().map(|d| d.source.as_str());
    ProjectDisplay {
        name,
        name_source,
        description: declared.description.map(|d| d.text),
        description_source,
    }
}

/// Basename of the project root, the registry's default display name.
fn directory_name(root: &Path) -> String {
    root.canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

fn format_described(v: &Option<String>, source: Option<&str>) -> String {
    match (v, source) {
        (Some(s), Some(src)) => format!("{s} ({src})"),
        (Some(s), None) => s.clone(),
        (None, _) => "(none)".to_string(),
    }
}

fn format_optional(v: &Option<String>) -> &str {
    match v {
        Some(s) => s.as_str(),
        None => "-",
    }
}

fn format_config_human(d: &ConfigDisplay) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    writeln!(
        out,
        "Config file: {}",
        d.config_file.as_deref().unwrap_or("none")
    )
    .unwrap();
    writeln!(out, "Database:    {}", d.db_path).unwrap();

    writeln!(out, "\n[project]").unwrap();
    let name_note = match d.project.name_source {
        "config" => "(config)".to_string(),
        other => format!("(default: {other} name)"),
    };
    writeln!(out, "  name:              {} {name_note}", d.project.name).unwrap();
    writeln!(
        out,
        "  description:       {}",
        format_described(&d.project.description, d.project.description_source)
    )
    .unwrap();

    writeln!(out, "\n[mcp]").unwrap();
    let federated_note = if d.mcp.is_default { " (default)" } else { "" };
    writeln!(
        out,
        "  federated:         {}{federated_note}  # cross-project MCP tools; \
         `cartog serve --federated` also enables them",
        d.mcp.federated
    )
    .unwrap();

    writeln!(out, "\n[embedding]").unwrap();
    writeln!(
        out,
        "  provider:          {}",
        format_value(&d.embedding.provider)
    )
    .unwrap();
    writeln!(
        out,
        "  model:             {}",
        format_optional(&d.embedding.model)
    )
    .unwrap();
    writeln!(
        out,
        "  dimension:         {}",
        d.embedding.dimension.map_or("-".into(), |v| v.to_string())
    )
    .unwrap();

    writeln!(out, "\n[embedding.local]").unwrap();
    writeln!(
        out,
        "  query_prefix:      {}",
        format_optional(&d.embedding.local.query_prefix)
    )
    .unwrap();
    writeln!(
        out,
        "  document_prefix:   {}",
        format_optional(&d.embedding.local.document_prefix)
    )
    .unwrap();

    writeln!(out, "\n[embedding.ollama]").unwrap();
    writeln!(
        out,
        "  base_url:          {}",
        format_value(&d.embedding.ollama.base_url)
    )
    .unwrap();
    writeln!(
        out,
        "  model:             {}",
        format_value(&d.embedding.ollama.model)
    )
    .unwrap();

    writeln!(out, "\n[reranker]").unwrap();
    writeln!(
        out,
        "  provider:          {}",
        format_value(&d.reranker.provider)
    )
    .unwrap();

    writeln!(out, "\n[rag]").unwrap();
    writeln!(
        out,
        "  retrieval_multiplier: {}",
        format_value(&d.rag.retrieval_multiplier)
    )
    .unwrap();
    writeln!(
        out,
        "  retrieval_floor:      {}",
        format_value(&d.rag.retrieval_floor)
    )
    .unwrap();
    writeln!(
        out,
        "  rerank_max:           {}",
        format_value(&d.rag.rerank_max)
    )
    .unwrap();
    writeln!(
        out,
        "  rerank_min:           {}",
        format_value(&d.rag.rerank_min)
    )
    .unwrap();

    writeln!(out, "\n[security]").unwrap();
    writeln!(
        out,
        "  redact_secrets:    {}",
        format_value(&d.security.redact_secrets)
    )
    .unwrap();

    out
}

#[derive(Serialize)]
struct ConfigDisplay {
    config_file: Option<String>,
    db_path: String,
    project: ProjectDisplay,
    mcp: McpDisplay,
    embedding: EmbeddingDisplay,
    reranker: RerankerDisplay,
    rag: RagDisplay,
    security: SecurityDisplay,
}

/// `[mcp]` state. Shown because `federated` is a privacy switch with two
/// independent sources (the flag and the config key), and a rejected or
/// salvaged config silently resolves it to `false` — so "why are the
/// cross-project tools missing?" needs an answer the CLI can give.
#[derive(Serialize)]
struct McpDisplay {
    /// Whether `[mcp] federated` is set in the config. The `--federated` flag
    /// can still enable the tools for one `serve` launch; this is the config
    /// half only, which is the half a user cannot otherwise inspect.
    federated: bool,
    is_default: bool,
}

#[derive(Serialize)]
struct ProjectDisplay {
    /// Display name: `[project] name`, else the root directory's basename.
    name: String,
    /// `"config"` or `"directory"`.
    name_source: &'static str,
    description: Option<String>,
    /// `"config"` or `"readme"`; absent when there is no description.
    #[serde(skip_serializing_if = "Option::is_none")]
    description_source: Option<&'static str>,
}

#[derive(Serialize)]
struct SecurityDisplay {
    redact_secrets: ValueDisplay,
}

#[derive(Serialize)]
struct RagDisplay {
    retrieval_multiplier: ValueDisplay,
    retrieval_floor: ValueDisplay,
    rerank_max: ValueDisplay,
    rerank_min: ValueDisplay,
}

#[derive(Serialize)]
struct EmbeddingDisplay {
    provider: ValueDisplay,
    model: Option<String>,
    dimension: Option<usize>,
    local: LocalEmbeddingDisplay,
    ollama: OllamaDisplay,
}

#[derive(Serialize)]
struct LocalEmbeddingDisplay {
    query_prefix: Option<String>,
    document_prefix: Option<String>,
}

#[derive(Serialize)]
struct OllamaDisplay {
    base_url: ValueDisplay,
    model: ValueDisplay,
}

#[derive(Serialize)]
struct RerankerDisplay {
    provider: ValueDisplay,
}

#[derive(Serialize)]
struct ValueDisplay {
    value: String,
    is_default: bool,
    default: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn default_config_display() -> ConfigDisplay {
        use crate::config::{
            DEFAULT_EMBEDDING_PROVIDER, DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_MODEL,
            DEFAULT_RERANKER_PROVIDER,
        };
        ConfigDisplay {
            config_file: None,
            db_path: "/tmp/test.db".into(),
            project: ProjectDisplay {
                name: "myrepo".into(),
                name_source: "directory",
                description: None,
                description_source: None,
            },
            mcp: McpDisplay {
                federated: false,
                is_default: true,
            },
            embedding: EmbeddingDisplay {
                provider: ValueDisplay {
                    value: DEFAULT_EMBEDDING_PROVIDER.into(),
                    is_default: true,
                    default: DEFAULT_EMBEDDING_PROVIDER.into(),
                },
                model: None,
                dimension: None,
                local: LocalEmbeddingDisplay {
                    query_prefix: None,
                    document_prefix: None,
                },
                ollama: OllamaDisplay {
                    base_url: ValueDisplay {
                        value: DEFAULT_OLLAMA_BASE_URL.into(),
                        is_default: true,
                        default: DEFAULT_OLLAMA_BASE_URL.into(),
                    },
                    model: ValueDisplay {
                        value: DEFAULT_OLLAMA_MODEL.into(),
                        is_default: true,
                        default: DEFAULT_OLLAMA_MODEL.into(),
                    },
                },
            },
            reranker: RerankerDisplay {
                provider: ValueDisplay {
                    value: DEFAULT_RERANKER_PROVIDER.into(),
                    is_default: true,
                    default: DEFAULT_RERANKER_PROVIDER.into(),
                },
            },
            rag: {
                let t = cartog_rag::search::SearchTuning::default();
                let v = |n: u32| ValueDisplay {
                    value: n.to_string(),
                    is_default: true,
                    default: n.to_string(),
                };
                RagDisplay {
                    retrieval_multiplier: v(t.retrieval_multiplier),
                    retrieval_floor: v(t.retrieval_floor),
                    rerank_max: v(t.rerank_max),
                    rerank_min: v(t.rerank_min),
                }
            },
            security: SecurityDisplay {
                redact_secrets: ValueDisplay {
                    value: "true".into(),
                    is_default: true,
                    default: "true".into(),
                },
            },
        }
    }

    #[test]
    fn test_format_config_human_all_defaults() {
        let d = default_config_display();
        let out = format_config_human(&d);
        assert!(out.contains("Config file: none"));
        assert!(out.contains("Database:    /tmp/test.db"));
        assert!(out.contains("local (default)"));
        assert!(out.contains("model:             -"));
        assert!(out.contains("dimension:         -"));
        assert!(out.contains("query_prefix:      -"));
        assert!(out.contains("document_prefix:   -"));
    }

    #[test]
    fn test_format_config_human_custom_values() {
        let mut d = default_config_display();
        d.config_file = Some("/project/.cartog.toml".into());
        d.embedding.provider = ValueDisplay {
            value: "ollama".into(),
            is_default: false,
            default: "local".into(),
        };
        d.embedding.model = Some("nomic-embed-text".into());
        d.embedding.dimension = Some(768);

        let out = format_config_human(&d);
        assert!(out.contains("Config file: /project/.cartog.toml"));
        assert!(out.contains("ollama (default: local)"));
        assert!(out.contains("model:             nomic-embed-text"));
        assert!(out.contains("dimension:         768"));
    }

    #[test]
    fn project_section_shows_the_directory_default_and_none_when_undeclared() {
        let d = default_config_display();

        let out = format_config_human(&d);

        assert!(out.contains("[project]"));
        assert!(out.contains("name:              myrepo (default: directory name)"));
        assert!(out.contains("description:       (none)"));
    }

    /// A user whose cross-project tools are missing has no other way to tell
    /// whether the config half resolved on: a rejected or salvaged config
    /// silently yields `false`.
    #[test]
    fn mcp_section_shows_federated_and_marks_the_default() {
        let d = default_config_display();
        let out = format_config_human(&d);
        assert!(out.contains("[mcp]"), "output has an [mcp] section: {out}");
        assert!(
            out.contains("federated:         false (default)"),
            "an unset federated is shown as a default: {out}"
        );
    }

    #[test]
    fn mcp_federated_set_in_config_is_not_marked_default() {
        let mut d = default_config_display();
        d.mcp = McpDisplay {
            federated: true,
            is_default: false,
        };
        let out = format_config_human(&d);
        assert!(
            out.contains("federated:         true"),
            "a configured federated is shown: {out}"
        );
        assert!(
            !out.contains("federated:         true (default)"),
            "an explicitly-set federated must not be labelled default: {out}"
        );
    }

    #[test]
    fn project_section_labels_each_value_with_its_source() {
        let mut d = default_config_display();
        d.project.name = "svc-billing".into();
        d.project.name_source = "config";
        d.project.description = Some("Invoice generation.".into());
        d.project.description_source = Some("readme");

        let out = format_config_human(&d);

        assert!(out.contains("name:              svc-billing (config)"));
        assert!(out.contains("description:       Invoice generation. (readme)"));
    }

    #[test]
    fn project_section_serializes_to_json() {
        let mut d = default_config_display();
        d.project.name = "svc-billing".into();
        d.project.name_source = "config";

        let json = serde_json::to_value(&d).unwrap();

        assert_eq!(json["project"]["name"], "svc-billing");
        assert_eq!(json["project"]["name_source"], "config");
        assert!(json["project"]["description"].is_null());
        assert!(json["project"].get("description_source").is_none());
    }

    #[test]
    fn resolve_project_display_falls_back_to_the_readme_beside_the_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# T\n\nBills customers.\n").unwrap();
        let config_path = dir.path().join(".cartog.toml");

        let d = resolve_project_display(None, Some(&config_path));

        assert_eq!(d.description.as_deref(), Some("Bills customers."));
        assert_eq!(d.description_source, Some("readme"));
        assert_eq!(d.name_source, "directory");
    }

    #[test]
    fn resolve_project_display_prefers_the_config_description() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# T\n\nFrom readme.\n").unwrap();
        let project = crate::config::ProjectConfig {
            name: Some("svc-billing".into()),
            description: Some("From config.".into()),
        };

        let d = resolve_project_display(Some(&project), Some(&dir.path().join(".cartog.toml")));

        assert_eq!(d.name, "svc-billing");
        assert_eq!(d.name_source, "config");
        assert_eq!(d.description.as_deref(), Some("From config."));
        assert_eq!(d.description_source, Some("config"));
    }

    #[test]
    fn test_format_value_default() {
        let v = ValueDisplay {
            value: "local".into(),
            is_default: true,
            default: "local".into(),
        };
        assert_eq!(format_value(&v), "local (default)");
    }

    #[test]
    fn test_format_value_overridden() {
        let v = ValueDisplay {
            value: "ollama".into(),
            is_default: false,
            default: "local".into(),
        };
        assert_eq!(format_value(&v), "ollama (default: local)");
    }

    #[test]
    fn test_format_optional_some() {
        let v = Some("value".to_string());
        assert_eq!(format_optional(&v), "value");
    }

    #[test]
    fn test_format_optional_none() {
        let v: Option<String> = None;
        assert_eq!(format_optional(&v), "-");
    }

    #[test]
    fn test_config_display_json_serialization() {
        let d = default_config_display();
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["db_path"], "/tmp/test.db");
        assert_eq!(json["embedding"]["provider"]["value"], "local");
        assert_eq!(json["embedding"]["provider"]["is_default"], true);
        assert!(json["config_file"].is_null());
        assert!(json["embedding"]["model"].is_null());
    }
}
