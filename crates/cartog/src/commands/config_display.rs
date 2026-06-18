//! `cartog config`: render the resolved configuration with default-value markers.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::config::CartogConfig;

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
                is_default: reranker.map_or(true, |r| r.provider.is_none()),
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
    embedding: EmbeddingDisplay,
    reranker: RerankerDisplay,
    rag: RagDisplay,
    security: SecurityDisplay,
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
