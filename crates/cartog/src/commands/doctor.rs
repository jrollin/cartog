//! `cartog doctor`: environment/config/database/provider health checks.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

#[cfg(feature = "remote-s3")]
use super::remote;
use crate::config::CartogConfig;
use cartog_db::Database;
use cartog_rag as rag;

#[derive(Serialize)]
struct CheckResult {
    name: String,
    status: CheckStatus,
    message: String,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum CheckStatus {
    Ok,
    Warn,
    Error,
}

impl CheckStatus {
    fn icon(self) -> &'static str {
        match self {
            CheckStatus::Ok => "+",
            CheckStatus::Warn => "!",
            CheckStatus::Error => "x",
        }
    }
}

#[derive(Serialize)]
struct DoctorReport {
    checks: Vec<CheckResult>,
    summary: DoctorSummary,
}

#[derive(Serialize)]
struct DoctorSummary {
    total: usize,
    ok: usize,
    warn: usize,
    error: usize,
}

fn check_git_repo() -> CheckResult {
    let mut dir = std::env::current_dir().unwrap_or_default();
    loop {
        if dir.join(".git").exists() {
            return CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: format!("git repository at {}", dir.display()),
            };
        }
        if !dir.pop() {
            break;
        }
    }
    CheckResult {
        name: "git".into(),
        status: CheckStatus::Error,
        message: "not inside a git repository".into(),
    }
}

fn check_config(config_path: Option<&Path>, rejected: bool) -> CheckResult {
    match (config_path, rejected) {
        (Some(p), true) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Error,
            message: format!(
                "{} was REJECTED (see stderr at startup for the reason). \
                 cartog is running with defaults; other check rows below \
                 reflect defaults, not your config file.",
                p.display()
            ),
        },
        (Some(p), false) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Ok,
            message: format!("loaded from {}", p.display()),
        },
        (None, _) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Warn,
            message: "no .cartog.toml found (using defaults)".into(),
        },
    }
}

fn check_database(db_path: &Path, embedding_dim: usize) -> CheckResult {
    if !db_path.exists() {
        return CheckResult {
            name: "database".into(),
            status: CheckStatus::Warn,
            message: format!(
                "database not found at {}, run 'cartog index'",
                db_path.display()
            ),
        };
    }
    match Database::open(db_path, embedding_dim) {
        Ok(db) => match db.stats() {
            Ok(stats) if stats.num_files > 0 => CheckResult {
                name: "database".into(),
                status: CheckStatus::Ok,
                message: format!(
                    "{} files, {} symbols at {}",
                    stats.num_files,
                    stats.num_symbols,
                    db_path.display()
                ),
            },
            Ok(_) => CheckResult {
                name: "database".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "database exists but is empty, run 'cartog index' ({})",
                    db_path.display()
                ),
            },
            Err(e) => CheckResult {
                name: "database".into(),
                status: CheckStatus::Error,
                message: format!("failed to query database at {}: {e}", db_path.display()),
            },
        },
        Err(e) => CheckResult {
            name: "database".into(),
            status: CheckStatus::Error,
            message: format!("failed to open database at {}: {e}", db_path.display()),
        },
    }
}

/// Parse "http://host:port" into a "host:port" string for TCP probing.
fn parse_host_port(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let host_port = without_scheme.trim_end_matches('/');
    if host_port.contains(':') {
        Some(host_port.to_string())
    } else {
        Some(format!("{host_port}:80"))
    }
}

fn check_embedding_provider(config: &rag::EmbeddingProviderConfig) -> CheckResult {
    match config.provider.as_str() {
        "local" => {
            let model = config.model.as_deref();
            if rag::is_embedding_model_cached(model) {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Ok,
                    message: "local model cached".into(),
                }
            } else {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Warn,
                    message: "local model not downloaded, run 'cartog rag setup'".into(),
                }
            }
        }
        "ollama" => {
            let base_url = config
                .base_url
                .as_deref()
                .unwrap_or(rag::providers::DEFAULT_OLLAMA_BASE_URL);
            match parse_host_port(base_url) {
                Some(addr) => {
                    let resolve_result: Result<std::net::SocketAddr, _> =
                        std::net::ToSocketAddrs::to_socket_addrs(&addr.as_str())
                            .map(|mut addrs| addrs.next())
                            .and_then(|opt| {
                                opt.ok_or_else(|| {
                                    std::io::Error::new(
                                        std::io::ErrorKind::AddrNotAvailable,
                                        format!("no addresses resolved for {addr}"),
                                    )
                                })
                            });
                    let socket_addr = match resolve_result {
                        Ok(sa) => sa,
                        Err(e) => {
                            return CheckResult {
                                name: "embedding".into(),
                                status: CheckStatus::Error,
                                message: format!("cannot resolve ollama host '{addr}': {e}"),
                            };
                        }
                    };
                    match std::net::TcpStream::connect_timeout(&socket_addr, Duration::from_secs(3))
                    {
                        Ok(_) => CheckResult {
                            name: "embedding".into(),
                            status: CheckStatus::Ok,
                            message: format!("ollama reachable at {base_url}"),
                        },
                        Err(e) => CheckResult {
                            name: "embedding".into(),
                            status: CheckStatus::Error,
                            message: format!("cannot reach ollama at {base_url}: {e}"),
                        },
                    }
                }
                None => CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Error,
                    message: format!("cannot parse ollama URL: {base_url}"),
                },
            }
        }
        "openai" => {
            let base_url = config
                .base_url
                .as_deref()
                .unwrap_or(rag::providers::DEFAULT_OPENAI_BASE_URL);
            let env = config
                .api_key_env
                .as_deref()
                .unwrap_or(rag::providers::DEFAULT_OPENAI_API_KEY_ENV);
            // No TCP probe: hosted endpoints sit behind auth and CDNs, so a
            // socket connect is misleading. But a base_url without an
            // http(s):// scheme + host is a config typo we can catch here.
            if parse_host_port(base_url).is_none() {
                return CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Error,
                    message: format!(
                        "invalid [embedding.openai].base_url {base_url:?} — expected an \
                         http(s):// URL ending in /v1"
                    ),
                };
            }
            // Report the endpoint + key presence; an unset OR empty key is
            // fine for keyless local /v1 servers.
            if std::env::var(env).is_ok_and(|v| !v.is_empty()) {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Ok,
                    message: format!("openai endpoint {base_url} (key from ${env})"),
                }
            } else {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Warn,
                    message: format!(
                        "openai endpoint {base_url}; ${env} unset (ok for keyless local endpoints)"
                    ),
                }
            }
        }
        other => CheckResult {
            name: "embedding".into(),
            status: CheckStatus::Error,
            message: format!("unknown provider '{other}'"),
        },
    }
}

fn check_reranker(config: &rag::EmbeddingProviderConfig) -> CheckResult {
    let model = config.reranker_model.as_deref();
    let name = model.unwrap_or(rag::DEFAULT_RERANKER_MODEL);
    match config.reranker_provider.as_str() {
        "none" => CheckResult {
            name: "reranker".into(),
            status: CheckStatus::Ok,
            message: "disabled".into(),
        },
        "local" => match rag::resolve_reranker_model(model) {
            // Unparseable model = config error, not a missing download.
            Err(e) => CheckResult {
                name: "reranker".into(),
                status: CheckStatus::Error,
                message: format!("unknown reranker model '{name}': {e}"),
            },
            Ok(rm) => {
                if rag::is_reranker_resolved_cached(&rm) {
                    CheckResult {
                        name: "reranker".into(),
                        status: CheckStatus::Ok,
                        message: format!("{name} cached{}", orphan_bge_hint(name)),
                    }
                } else {
                    CheckResult {
                        name: "reranker".into(),
                        status: CheckStatus::Warn,
                        message: format!("{name} not downloaded, run 'cartog rag setup'"),
                    }
                }
            }
        },
        other => CheckResult {
            name: "reranker".into(),
            status: CheckStatus::Error,
            message: format!("unknown provider '{other}'"),
        },
    }
}

/// Reclaim hint when the old bge-base weights are orphaned under the new default.
fn orphan_bge_hint(active: &str) -> String {
    let orphan = rag::legacy_bge_reranker_cache_dir();
    if active == rag::DEFAULT_RERANKER_MODEL && orphan.is_dir() {
        format!(
            " (old bge-reranker-base ~1.1GB reclaimable: rm -rf {})",
            orphan.display()
        )
    } else {
        String::new()
    }
}

/// Doctor check for the optional `[remote]` S3-compatible sync.
///
/// Status semantics:
/// - **Ok** when `[remote]` is unset (the default — feature is inert; no
///   network traffic happens unless the user opts in). We do not warn here:
///   the absence of remote config is the expected baseline.
/// - **Ok** when `[remote].url` resolves and a HEAD against the configured
///   object succeeds (200 or 404 — both prove the bucket + creds work).
/// - **Warn** for any reachability failure (creds missing, wrong region,
///   network unreachable, 403). Push/pull would fail with the same error;
///   doctor surfaces it before the user discovers it the hard way.
/// - **Error** only when the feature was disabled at build time but a
///   `[remote]` section exists — config will be silently ignored otherwise.
fn check_remote(config: &CartogConfig, config_rejected: bool) -> CheckResult {
    // When the config file itself was rejected, the `config.remote` view is
    // always None (default). Reporting "not configured" here would be
    // misleading — the user might have had a perfectly valid [remote]
    // section before some other unrelated key got rejected. Surface this
    // explicitly so doctor doesn't lie.
    if config_rejected {
        return CheckResult {
            name: "remote".into(),
            status: CheckStatus::Warn,
            message: "[remote] status unknown — config file was rejected; \
                      fix the config and re-run doctor"
                .into(),
        };
    }
    let remote = match config.remote.as_ref() {
        Some(r) => r,
        None => {
            return CheckResult {
                name: "remote".into(),
                status: CheckStatus::Ok,
                message: "not configured (local-only)".into(),
            }
        }
    };

    if remote.url.as_deref().unwrap_or("").is_empty() {
        return CheckResult {
            name: "remote".into(),
            status: CheckStatus::Warn,
            message: "[remote] section present but `url` is empty".into(),
        };
    }

    #[cfg(not(feature = "remote-s3"))]
    {
        let _ = remote; // url presence already checked above
        CheckResult {
            name: "remote".into(),
            status: CheckStatus::Error,
            message: "[remote] configured but cartog was built without `remote-s3` feature".into(),
        }
    }

    #[cfg(feature = "remote-s3")]
    match remote::check_remote_reachable(remote) {
        Ok(()) => CheckResult {
            name: "remote".into(),
            status: CheckStatus::Ok,
            message: format!("{} reachable", remote.url.as_deref().unwrap_or("<unset>")),
        },
        Err(e) => CheckResult {
            name: "remote".into(),
            status: CheckStatus::Warn,
            message: format!("unreachable: {e}"),
        },
    }
}

fn build_report(checks: Vec<CheckResult>) -> DoctorReport {
    let ok = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Ok)
        .count();
    let warn = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    let error = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Error)
        .count();

    DoctorReport {
        summary: DoctorSummary {
            total: checks.len(),
            ok,
            warn,
            error,
        },
        checks,
    }
}

fn format_report_human(report: &DoctorReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    for check in &report.checks {
        writeln!(
            out,
            "  [{}] {}: {}",
            check.status.icon(),
            check.name,
            check.message,
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    let s = &report.summary;
    if s.error > 0 {
        writeln!(
            out,
            "{} checks passed, {} warnings, {} errors",
            s.ok, s.warn, s.error
        )
        .unwrap();
    } else if s.warn > 0 {
        writeln!(out, "{} checks passed, {} warnings", s.ok, s.warn).unwrap();
    } else {
        writeln!(out, "All {} checks passed", s.ok).unwrap();
    }

    out
}

/// Check that requirements are met and everything is working.
pub fn cmd_doctor(
    config: &CartogConfig,
    config_path: Option<&Path>,
    config_rejected: bool,
    db_path: &Path,
    json: bool,
    embedding_dim: usize,
    provider_config: &rag::EmbeddingProviderConfig,
) -> Result<()> {
    let checks = vec![
        check_git_repo(),
        check_config(config_path, config_rejected),
        check_database(db_path, embedding_dim),
        check_embedding_provider(provider_config),
        check_reranker(provider_config),
        check_remote(config, config_rejected),
    ];

    let report = build_report(checks);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", format_report_human(&report));
    }

    if report.summary.error > 0 {
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    #[test]
    #[serial]
    fn test_check_git_repo_inside_git() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let subdir = dir.path().join("sub");
        std::fs::create_dir(&subdir).unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&subdir).unwrap();
        let result = check_git_repo();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(result.status, CheckStatus::Ok);
        assert_eq!(result.name, "git");
    }

    #[test]
    #[serial]
    fn test_check_git_repo_outside_git() {
        let dir = tempfile::TempDir::new().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = check_git_repo();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(result.status, CheckStatus::Error);
    }

    #[test]
    fn test_check_config_present() {
        let result = check_config(Some(Path::new("/project/.cartog.toml")), false);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains(".cartog.toml"));
    }

    #[test]
    fn test_check_config_absent() {
        let result = check_config(None, false);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("defaults"));
    }

    #[test]
    fn test_check_config_rejected_reports_error() {
        let result = check_config(Some(Path::new("/project/.cartog.toml")), true);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("REJECTED"));
    }

    #[test]
    fn test_check_database_missing() {
        let result = check_database(Path::new("/nonexistent/path.db"), 384);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("not found"));
    }

    #[test]
    fn test_check_database_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let _db = Database::open(&db_path, 384).unwrap();
        let result = check_database(&db_path, 384);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("empty"));
    }

    #[test]
    fn test_check_reranker_disabled() {
        let config = rag::EmbeddingProviderConfig {
            reranker_provider: "none".into(),
            ..Default::default()
        };
        let result = check_reranker(&config);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("disabled"));
    }

    #[test]
    fn test_check_reranker_unknown_provider() {
        let config = rag::EmbeddingProviderConfig {
            reranker_provider: "foobar".into(),
            ..Default::default()
        };
        let result = check_reranker(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("foobar"));
    }

    #[test]
    fn test_check_embedding_unknown_provider() {
        let config = rag::EmbeddingProviderConfig {
            provider: "unknown".into(),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("unknown"));
    }

    #[test]
    fn test_check_embedding_ollama_unreachable() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: Some("http://127.0.0.1:19999".into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("cannot reach"));
    }

    // These mutate the process-global env, so #[serial] keeps them off the
    // shared environment at the same time as any other env-touching test.
    #[test]
    #[serial]
    fn test_check_embedding_openai_key_present_is_ok() {
        let env = "CARTOG_TEST_OPENAI_KEY_PRESENT";
        std::env::set_var(env, "sk-test");
        let config = rag::EmbeddingProviderConfig {
            provider: "openai".into(),
            api_key_env: Some(env.into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        std::env::remove_var(env);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("openai endpoint"));
    }

    #[test]
    #[serial]
    fn test_check_embedding_openai_key_unset_warns() {
        let env = "CARTOG_TEST_OPENAI_KEY_UNSET";
        std::env::remove_var(env);
        let config = rag::EmbeddingProviderConfig {
            provider: "openai".into(),
            api_key_env: Some(env.into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("unset"));
    }

    #[test]
    fn test_check_embedding_openai_malformed_base_url_errors() {
        let config = rag::EmbeddingProviderConfig {
            provider: "openai".into(),
            base_url: Some("api.openai.com/v1".into()), // missing scheme
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("invalid"));
    }

    #[test]
    #[serial]
    fn test_check_embedding_openai_empty_key_warns() {
        // An exported-but-empty key is treated as absent, not a usable token.
        let env = "CARTOG_TEST_OPENAI_KEY_EMPTY";
        std::env::set_var(env, "");
        let config = rag::EmbeddingProviderConfig {
            provider: "openai".into(),
            api_key_env: Some(env.into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        std::env::remove_var(env);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("unset"));
    }

    #[test]
    fn test_check_status_icons() {
        assert_eq!(CheckStatus::Ok.icon(), "+");
        assert_eq!(CheckStatus::Warn.icon(), "!");
        assert_eq!(CheckStatus::Error.icon(), "x");
    }

    #[test]
    fn test_parse_host_port_standard() {
        assert_eq!(
            parse_host_port("http://localhost:11434"),
            Some("localhost:11434".into())
        );
    }

    #[test]
    fn test_parse_host_port_no_port() {
        assert_eq!(
            parse_host_port("http://example.com"),
            Some("example.com:80".into())
        );
    }

    #[test]
    fn test_parse_host_port_https() {
        assert_eq!(
            parse_host_port("https://example.com:443"),
            Some("example.com:443".into())
        );
    }

    #[test]
    fn test_parse_host_port_trailing_slash() {
        assert_eq!(
            parse_host_port("http://localhost:11434/"),
            Some("localhost:11434".into())
        );
    }

    #[test]
    fn test_parse_host_port_no_scheme() {
        assert_eq!(parse_host_port("localhost:11434"), None);
    }

    #[test]
    fn test_doctor_report_json_serialization() {
        let report = DoctorReport {
            checks: vec![
                CheckResult {
                    name: "git".into(),
                    status: CheckStatus::Ok,
                    message: "git repository".into(),
                },
                CheckResult {
                    name: "config".into(),
                    status: CheckStatus::Warn,
                    message: "no config".into(),
                },
            ],
            summary: DoctorSummary {
                total: 2,
                ok: 1,
                warn: 1,
                error: 0,
            },
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["checks"][0]["status"], "ok");
        assert_eq!(json["checks"][1]["status"], "warn");
        assert_eq!(json["summary"]["total"], 2);
        assert_eq!(json["summary"]["ok"], 1);
    }

    // ── build_report tests ──

    #[test]
    fn test_build_report_all_ok() {
        let checks = vec![
            CheckResult {
                name: "a".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
        ];
        let report = build_report(checks);
        assert_eq!(report.summary.total, 2);
        assert_eq!(report.summary.ok, 2);
        assert_eq!(report.summary.warn, 0);
        assert_eq!(report.summary.error, 0);
    }

    #[test]
    fn test_build_report_mixed() {
        let checks = vec![
            CheckResult {
                name: "a".into(),
                status: CheckStatus::Ok,
                message: "fine".into(),
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Warn,
                message: "meh".into(),
            },
            CheckResult {
                name: "c".into(),
                status: CheckStatus::Error,
                message: "bad".into(),
            },
        ];
        let report = build_report(checks);
        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.ok, 1);
        assert_eq!(report.summary.warn, 1);
        assert_eq!(report.summary.error, 1);
    }

    #[test]
    fn test_build_report_empty() {
        let report = build_report(vec![]);
        assert_eq!(report.summary.total, 0);
        assert_eq!(report.summary.ok, 0);
        assert_eq!(report.summary.warn, 0);
        assert_eq!(report.summary.error, 0);
    }

    // ── format_report_human tests ──

    #[test]
    fn test_format_report_human_all_ok() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "git repository".into(),
            },
            CheckResult {
                name: "db".into(),
                status: CheckStatus::Ok,
                message: "42 files".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[+] git: git repository"));
        assert!(out.contains("[+] db: 42 files"));
        assert!(out.contains("All 2 checks passed"));
    }

    #[test]
    fn test_format_report_human_with_warnings() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "config".into(),
                status: CheckStatus::Warn,
                message: "missing".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[!] config: missing"));
        assert!(out.contains("1 checks passed, 1 warnings"));
        assert!(!out.contains("errors"));
    }

    #[test]
    fn test_format_report_human_with_errors() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "embed".into(),
                status: CheckStatus::Warn,
                message: "not cached".into(),
            },
            CheckResult {
                name: "db".into(),
                status: CheckStatus::Error,
                message: "broken".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[x] db: broken"));
        assert!(out.contains("1 checks passed, 1 warnings, 1 errors"));
    }

    // ── check_database with indexed data ──

    #[test]
    fn test_check_database_with_data() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        // Insert a minimal file so stats.num_files > 0
        db.upsert_file(&cartog_core::FileInfo {
            path: "test.py".into(),
            last_modified: 0.0,
            hash: "abc123".into(),
            language: "python".into(),
            num_symbols: 0,
        })
        .unwrap();
        drop(db);

        let result = check_database(&db_path, 384);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("1 files"));
    }

    // ── check_embedding_provider local variants ──

    #[test]
    fn test_check_embedding_local_cached() {
        // This test reflects actual machine state — the local model is cached on CI/dev
        let config = rag::EmbeddingProviderConfig::default();
        let result = check_embedding_provider(&config);
        // Either Ok (cached) or Warn (not cached) — never Error for "local"
        assert_ne!(result.status, CheckStatus::Error);
        assert_eq!(result.name, "embedding");
    }

    #[test]
    fn test_check_reranker_local() {
        let config = rag::EmbeddingProviderConfig::default();
        let result = check_reranker(&config);
        // Either Ok (cached) or Warn (not cached) — never Error for "local".
        assert_ne!(result.status, CheckStatus::Error);
        assert_eq!(result.name, "reranker");
        // The message names the resolved default model regardless of cache state.
        assert!(result.message.contains(rag::DEFAULT_RERANKER_MODEL));
    }

    #[test]
    fn test_check_reranker_local_custom_model() {
        let config = rag::EmbeddingProviderConfig {
            reranker_model: Some("BAAI/bge-reranker-base".into()),
            ..Default::default()
        };
        let result = check_reranker(&config);
        assert_ne!(result.status, CheckStatus::Error);
        assert!(result.message.contains("BAAI/bge-reranker-base"));
    }

    #[test]
    fn test_check_reranker_invalid_model_is_error_not_missing_download() {
        // An unparseable model must surface as an Error naming the bad value, not a
        // misleading "not downloaded, run 'cartog rag setup'" (setup would also fail).
        let config = rag::EmbeddingProviderConfig {
            reranker_model: Some("totally/not-a-real-reranker".into()),
            ..Default::default()
        };
        let result = check_reranker(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("unknown reranker model"));
        assert!(result.message.contains("totally/not-a-real-reranker"));
        assert!(!result.message.contains("rag setup"));
    }

    // ── check_embedding_provider ollama with bad URL ──

    #[test]
    fn test_check_embedding_ollama_bad_url() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: Some("not-a-url".into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("cannot parse"));
    }

    // ── check_embedding_provider ollama with default URL (unreachable in test) ──

    #[test]
    fn test_check_embedding_ollama_default_url() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: None,
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        // On machines without ollama running, this will be Error
        // On machines with ollama running, this will be Ok
        assert_eq!(result.name, "embedding");
        assert!(
            result.status == CheckStatus::Ok || result.status == CheckStatus::Error,
            "ollama check should be Ok or Error, not Warn"
        );
    }
}
