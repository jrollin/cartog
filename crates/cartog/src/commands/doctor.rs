//! `cartog doctor`: environment/config/database/provider health checks.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

#[cfg(feature = "remote-s3")]
use super::remote;
use super::self_cmd::github_latest_url;
use crate::config::CartogConfig;
use cartog::semver::{compare_stable_versions, parse_release_tag};
use cartog_db::Database;
use cartog_rag as rag;

#[derive(Serialize)]
struct CheckResult {
    name: String,
    status: CheckStatus,
    message: String,
    /// Extra lines rendered indented under `message` in human output and kept
    /// as discrete strings in `--json` (never newline-joined into `message`,
    /// which machine consumers would have to re-split).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    details: Vec<String>,
}

// Only the two multi-line rows (`paths`, `lsp`) carry `details`; every other
// check omits the field and relies on this default.
impl Default for CheckResult {
    fn default() -> Self {
        Self {
            name: String::new(),
            status: CheckStatus::Ok,
            message: String::new(),
            details: Vec::new(),
        }
    }
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
                details: Vec::new(),
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
        details: Vec::new(),
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
            ..Default::default()
        },
        (Some(p), false) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Ok,
            message: format!("loaded from {}", p.display()),
            details: Vec::new(),
        },
        (None, _) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Warn,
            message: "no .cartog.toml found (using defaults)".into(),
            details: Vec::new(),
        },
    }
}

fn check_database(
    db_path: &Path,
    embedding_dim: usize,
    consent: bool,
    db_path_unknown: bool,
) -> CheckResult {
    if !db_path.exists() {
        // `consent` mirrors the runtime gate, so the hint never names a command
        // that would refuse. Three distinct states, three different fixes:
        // a config exists but is unreadable (fix it or pick a path), no opt-in
        // at all (init), or good to go (index).
        let hint = if db_path_unknown {
            "the config was rejected so its `[database] path` is unknown — fix \
             the error reported above, or pass --db <PATH>"
        } else if consent {
            "run 'cartog index'"
        } else {
            "run 'cartog init' then 'cartog index' (or set CARTOG_AUTO_INIT=1)"
        };
        return CheckResult {
            name: "database".into(),
            status: CheckStatus::Warn,
            message: format!("database not found at {}, {hint}", db_path.display()),
            details: Vec::new(),
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
                ..Default::default()
            },
            Ok(_) => CheckResult {
                name: "database".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "database exists but is empty, run 'cartog index' ({})",
                    db_path.display()
                ),
                ..Default::default()
            },
            Err(e) => CheckResult {
                name: "database".into(),
                status: CheckStatus::Error,
                message: format!("failed to query database at {}: {e}", db_path.display()),
                details: Vec::new(),
            },
        },
        Err(e) => CheckResult {
            name: "database".into(),
            status: CheckStatus::Error,
            message: format!("failed to open database at {}: {e}", db_path.display()),
            details: Vec::new(),
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
                    details: Vec::new(),
                }
            } else {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Warn,
                    message: "local model not downloaded, run 'cartog rag setup'".into(),
                    details: Vec::new(),
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
                                details: Vec::new(),
                            };
                        }
                    };
                    match std::net::TcpStream::connect_timeout(&socket_addr, Duration::from_secs(3))
                    {
                        Ok(_) => CheckResult {
                            name: "embedding".into(),
                            status: CheckStatus::Ok,
                            message: format!("ollama reachable at {base_url}"),
                            details: Vec::new(),
                        },
                        Err(e) => CheckResult {
                            name: "embedding".into(),
                            status: CheckStatus::Error,
                            message: format!("cannot reach ollama at {base_url}: {e}"),
                            details: Vec::new(),
                        },
                    }
                }
                None => CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Error,
                    message: format!("cannot parse ollama URL: {base_url}"),
                    details: Vec::new(),
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
                    ..Default::default()
                };
            }
            // Report the endpoint + key presence; an unset OR empty key is
            // fine for keyless local /v1 servers.
            if std::env::var(env).is_ok_and(|v| !v.is_empty()) {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Ok,
                    message: format!("openai endpoint {base_url} (key from ${env})"),
                    details: Vec::new(),
                }
            } else {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Warn,
                    message: format!(
                        "openai endpoint {base_url}; ${env} unset (ok for keyless local endpoints)"
                    ),
                    ..Default::default()
                }
            }
        }
        other => CheckResult {
            name: "embedding".into(),
            status: CheckStatus::Error,
            message: format!("unknown provider '{other}'"),
            details: Vec::new(),
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
            details: Vec::new(),
        },
        "local" => match rag::resolve_reranker_model(model) {
            // Unparseable model = config error, not a missing download.
            Err(e) => CheckResult {
                name: "reranker".into(),
                status: CheckStatus::Error,
                message: format!("unknown reranker model '{name}': {e}"),
                details: Vec::new(),
            },
            Ok(rm) => {
                if rag::is_reranker_resolved_cached(&rm) {
                    CheckResult {
                        name: "reranker".into(),
                        status: CheckStatus::Ok,
                        message: format!("{name} cached{}", orphan_bge_hint(name)),
                        details: Vec::new(),
                    }
                } else {
                    CheckResult {
                        name: "reranker".into(),
                        status: CheckStatus::Warn,
                        message: format!("{name} not downloaded, run 'cartog rag setup'"),
                        details: Vec::new(),
                    }
                }
            }
        },
        other => CheckResult {
            name: "reranker".into(),
            status: CheckStatus::Error,
            message: format!("unknown provider '{other}'"),
            details: Vec::new(),
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
            ..Default::default()
        };
    }
    let remote = match config.remote.as_ref() {
        Some(r) => r,
        None => {
            return CheckResult {
                name: "remote".into(),
                status: CheckStatus::Ok,
                message: "not configured (local-only)".into(),
                details: Vec::new(),
            }
        }
    };

    if remote.url.as_deref().unwrap_or("").is_empty() {
        return CheckResult {
            name: "remote".into(),
            status: CheckStatus::Warn,
            message: "[remote] section present but `url` is empty".into(),
            details: Vec::new(),
        };
    }

    #[cfg(not(feature = "remote-s3"))]
    {
        let _ = remote; // url presence already checked above
        CheckResult {
            name: "remote".into(),
            status: CheckStatus::Error,
            message: "[remote] configured but cartog was built without `remote-s3` feature".into(),
            details: Vec::new(),
        }
    }

    #[cfg(feature = "remote-s3")]
    match remote::check_remote_reachable(remote) {
        Ok(()) => CheckResult {
            name: "remote".into(),
            status: CheckStatus::Ok,
            message: format!("{} reachable", remote.url.as_deref().unwrap_or("<unset>")),
            details: Vec::new(),
        },
        Err(e) => CheckResult {
            name: "remote".into(),
            status: CheckStatus::Warn,
            message: format!("unreachable: {e}"),
            details: Vec::new(),
        },
    }
}

/// `CARTOG_NO_UPDATE_CHECK` (any non-empty value) skips the release probe.
fn update_check_disabled() -> bool {
    std::env::var_os("CARTOG_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty())
}

/// How long the release probe may block the whole report.
///
/// Deliberately much shorter than the 5s `cartog self update` allows: there,
/// waiting for an answer *is* the task, whereas doctor is what you run because
/// something is already broken, and it prints nothing until every row is in.
/// On a blackholed network (packets dropped, no fast RST) the user waits the
/// whole budget, so this is the one row that can make the report feel hung —
/// measured against a ~0.02-0.3s local-only run, 800ms keeps it unnoticeable
/// while still clearing a normal transatlantic round trip.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// Fetch the latest release tag under [`VERSION_PROBE_TIMEOUT`].
///
/// Mirrors `self_cmd::fetch_latest_version` but with doctor's shorter budget;
/// the shared helper keeps the longer one because `self update` needs it.
fn fetch_latest_version_quick(url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("cartog/", env!("CARGO_PKG_VERSION")))
        // Both budgets: the total cap alone still let a dropped-SYN connect
        // stall well past it, which is exactly the blackholed-network case.
        .connect_timeout(VERSION_PROBE_TIMEOUT)
        .timeout(VERSION_PROBE_TIMEOUT)
        .build()
        .context("building the HTTP client for the release probe")?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .with_context(|| format!("release probe to {url} failed"))?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("GitHub API returned status {status}");
    }
    let body = response
        .text()
        .with_context(|| format!("reading the release-probe response from {url}"))?;
    parse_release_tag(&body)
        .ok_or_else(|| anyhow::anyhow!("no stable release tag in the response from {url}"))
}

/// Compare the running version against the latest stable GitHub release.
///
/// A failed probe is `Ok`, not `Warn`: being offline is not a broken
/// environment, and doctor exits 1 on any error row. Both `disabled` and
/// `fetch` are injected rather than read here, so the function is pure and
/// the tests cannot be flipped by a `CARTOG_NO_UPDATE_CHECK` that happens to
/// be set in the developer's or CI runner's environment.
fn check_version(
    current: &str,
    disabled: bool,
    fetch: impl FnOnce() -> Result<String>,
) -> CheckResult {
    if disabled {
        return CheckResult {
            name: "version".into(),
            status: CheckStatus::Ok,
            message: format!("{current} (update check disabled)"),
            details: Vec::new(),
        };
    }
    match fetch() {
        Ok(latest) => {
            if compare_stable_versions(current, &latest) == std::cmp::Ordering::Less {
                CheckResult {
                    name: "version".into(),
                    status: CheckStatus::Warn,
                    message: format!(
                        "update available: {current} -> {latest}, run 'cartog self update'"
                    ),
                    ..Default::default()
                }
            } else {
                CheckResult {
                    name: "version".into(),
                    status: CheckStatus::Ok,
                    message: format!("{current} is up to date"),
                    details: Vec::new(),
                }
            }
        }
        Err(e) => CheckResult {
            name: "version".into(),
            status: CheckStatus::Ok,
            // `{:#}` so the whole context chain shows — the outer context
            // alone names the probe but hides why it failed.
            message: format!("{current} (latest unknown: {e:#})"),
            details: Vec::new(),
        },
    }
}

/// Inventory row: every path and identity a bug report needs, in one place.
/// Always `Ok` — it reports where things are, not whether they are healthy.
/// Advisory: does this project say what it is for?
///
/// Never an error and never a warning-with-teeth: a project with no
/// description indexes and registers exactly as before. What it loses is
/// *routing* — `cartog_list_projects` and `cartog projects list` show a
/// sibling session a name and a language mix but nothing about intent, which
/// is the one thing a cross-project question needs.
///
/// Mirrors the resolution `cartog index` performs (`[project] description`
/// first, then the README), so the row reports what would actually be stored.
fn check_project_description(config: &CartogConfig, project_root: &Path) -> CheckResult {
    let resolved = super::shared::declared_for(config.project.as_ref(), project_root);

    match resolved.description {
        Some(d) => CheckResult {
            name: "description".into(),
            status: CheckStatus::Ok,
            // Quoted so a description that happens to read like a status
            // message is visibly the project's own text, not doctor's.
            message: format!("{:?} ({})", d.text, d.source.as_str()),
            ..Default::default()
        },
        None => CheckResult {
            name: "description".into(),
            status: CheckStatus::Warn,
            message: "no [project] description and no README paragraph — other sessions \
                      cannot see what this project does. Add `description` under `[project]` \
                      in .cartog.toml, or an opening paragraph to README.md, then re-index."
                .into(),
            ..Default::default()
        },
    }
}

fn check_paths(config_path: Option<&Path>, db_path: &Path, project_root: &Path) -> CheckResult {
    let show = |p: Option<std::path::PathBuf>| {
        p.map_or_else(|| "unavailable".to_string(), |p| p.display().to_string())
    };
    let config = config_path.map_or_else(
        || "none (using defaults)".to_string(),
        |p| p.display().to_string(),
    );
    // `registry_path()` collapses three causes into `None`; a user debugging
    // "why is my registry empty" needs to know which one, since the fixes
    // differ (unset the env var vs. make it absolute vs. no $HOME at all).
    fn registry_path_display() -> String {
        if let Some(path) = cartog_registry::registry_path() {
            return path.display().to_string();
        }
        match std::env::var_os(cartog_registry::REGISTRY_ENV) {
            Some(v) if v.is_empty() => {
                format!(
                    "disabled ({} is set to an empty value)",
                    cartog_registry::REGISTRY_ENV
                )
            }
            Some(v) => format!(
                "disabled ({} is {:?}, which is not an absolute path)",
                cartog_registry::REGISTRY_ENV,
                v.to_string_lossy()
            ),
            None => "unavailable (no state directory could be resolved)".to_string(),
        }
    }

    let details = vec![
        format!("project root:  {}", project_root.display()),
        format!("config:        {config}"),
        format!("database:      {}", db_path.display()),
        format!(
            "state file:    {}",
            show(crate::state::default_state_file())
        ),
        format!("registry:      {}", registry_path_display()),
        format!("model cache:   {}", rag::model_cache_dir().display()),
        format!(
            "install:       {} ({})",
            super::self_cmd::effective_install_source(),
            super::self_cmd::TARGET_TRIPLE,
        ),
    ];
    CheckResult {
        name: "paths".into(),
        status: CheckStatus::Ok,
        message: String::new(),
        details,
    }
}

/// Whether clangd can see a compile database covering `dir`. Without one it
/// guesses bare flags and cross-file includes go unresolved — measured at ~59%
/// of the LSP edge gain on the C++ fixture, silently.
///
/// clangd searches the file's own directory and every ancestor, so this walks
/// upward from the C/C++ sources rather than testing the project root alone:
/// in a polyglot repo (a Rust workspace with a C fixture, a vendored native
/// dependency) the root is exactly where a compile database would be
/// meaningless, and demanding one there is unactionable advice.
#[cfg(feature = "lsp")]
fn has_compile_database(dir: &Path, stop_at: &Path) -> bool {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join("compile_commands.json").exists() || d.join("compile_flags.txt").exists() {
            return true;
        }
        if d == stop_at {
            break;
        }
        cur = d.parent();
    }
    false
}

/// Report LSP server availability for the languages actually indexed.
///
/// `languages` comes from the index itself, so the row never nags about a
/// language this project does not use. A `[lsp.<lang>] command` override counts
/// as available without a PATH probe — the binary lives in a container.
#[cfg(feature = "lsp")]
fn check_lsp(
    languages: &[(String, u32)],
    overrides: &std::collections::HashMap<String, Vec<String>>,
    c_family_dirs: &[std::path::PathBuf],
    project_root: &Path,
) -> CheckResult {
    use cartog_lsp::servers;

    let mut missing: Vec<String> = Vec::new();
    let mut available: Vec<&str> = Vec::new();
    let mut clangd_ready = false;

    for (lang, _) in languages {
        // Languages with no ServerSpec (markdown) are resolved by heuristics
        // only — not a gap, so never reported as one.
        if !servers::has_server_spec(lang) {
            continue;
        }
        if overrides.contains_key(lang) {
            available.push(lang);
            if lang == "c" || lang == "cpp" {
                clangd_ready = true;
            }
            continue;
        }
        let specs = servers::find_servers(lang);
        match specs
            .iter()
            .find(|s| servers::is_binary_available(s.binary))
        {
            Some(_) => {
                available.push(lang);
                if lang == "c" || lang == "cpp" {
                    clangd_ready = true;
                }
            }
            None => {
                // List every candidate: Ruby ships two servers with different
                // minimum runtimes, so naming only the first can hand the user
                // the one hint they cannot satisfy.
                let candidates = specs
                    .iter()
                    .map(|s| format!("{}: {}", s.binary, s.install_hint))
                    .collect::<Vec<_>>()
                    .join(" | ");
                missing.push(format!("{lang} ({candidates})"));
            }
        }
    }

    // Only meaningful once clangd is actually reachable; otherwise the missing
    // server is the finding and a compile-db warning would double-report it.
    let uncovered_dirs: Vec<&std::path::PathBuf> = if clangd_ready {
        c_family_dirs
            .iter()
            .filter(|d| !has_compile_database(d, project_root))
            .collect()
    } else {
        Vec::new()
    };

    if available.is_empty() && missing.is_empty() {
        return CheckResult {
            name: "lsp".into(),
            status: CheckStatus::Ok,
            message: "no indexed language uses an LSP server".into(),
            details: Vec::new(),
        };
    }

    let mut notes: Vec<String> = Vec::new();
    if !missing.is_empty() {
        notes.push(format!("no server for {}", missing.join(", ")));
    }
    if !uncovered_dirs.is_empty() {
        // Name a real directory the user can act on, not the repo root — in a
        // polyglot repo a compile database at the root would be meaningless.
        let shown = uncovered_dirs
            .iter()
            .take(3)
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let more = uncovered_dirs.len().saturating_sub(3);
        let suffix = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        notes.push(format!(
            "clangd has no compile database covering {shown}{suffix} — add a \
             compile_commands.json or compile_flags.txt there or in a parent; \
             without one most cross-file includes go unresolved"
        ));
    }

    if notes.is_empty() {
        CheckResult {
            name: "lsp".into(),
            status: CheckStatus::Ok,
            message: format!("servers found for {}", available.join(", ")),
            details: Vec::new(),
        }
    } else {
        // Keep the positive half visible: a bug report needs to know which of
        // the twelve indexed languages *did* resolve, not only the one gap.
        if !available.is_empty() {
            notes.push(format!("servers found for {}", available.join(", ")));
        }
        CheckResult {
            name: "lsp".into(),
            status: CheckStatus::Warn,
            message: String::new(),
            details: notes,
        }
    }
}

/// What the `lsp` row needs from the index: which languages are present, and
/// which directories hold the C/C++ sources clangd would need a compile
/// database for.
#[cfg(feature = "lsp")]
struct IndexedForLsp {
    languages: Vec<(String, u32)>,
    /// Deduplicated, sorted directories containing at least one C/C++ file.
    c_family_dirs: Vec<std::path::PathBuf>,
}

/// Read the index once for everything the `lsp` row needs, or `None` when the
/// database is absent or unreadable — the database row already reports that,
/// so the LSP row stays silent rather than repeating it.
#[cfg(feature = "lsp")]
fn indexed_for_lsp(
    db_path: &Path,
    embedding_dim: usize,
    project_root: &Path,
) -> Option<IndexedForLsp> {
    if !db_path.exists() {
        return None;
    }
    let db = Database::open(db_path, embedding_dim).ok()?;
    let languages = db.stats().ok()?.languages;
    let has_c_family = languages.iter().any(|(l, _)| l == "c" || l == "cpp");
    // Only pay for the file listing when a C-family language is actually
    // indexed; every other project skips the query entirely.
    let c_family_dirs = if has_c_family {
        let mut dirs: Vec<std::path::PathBuf> = db
            .all_files()
            .unwrap_or_default()
            .iter()
            .filter(|p| {
                matches!(
                    Path::new(p).extension().and_then(|e| e.to_str()),
                    Some("c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx")
                )
            })
            .filter_map(|p| Path::new(p).parent().map(|d| project_root.join(d)))
            .collect();
        dirs.sort();
        dirs.dedup();
        dirs
    } else {
        Vec::new()
    };
    Some(IndexedForLsp {
        languages,
        c_family_dirs,
    })
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
        // An empty message means the row's content lives entirely in `details`
        // (paths, lsp), so the label must not trail a space before the break.
        let sep = if check.message.is_empty() { "" } else { " " };
        writeln!(
            out,
            "  [{}] {}:{sep}{}",
            check.status.icon(),
            check.name,
            check.message,
        )
        .unwrap();
        for detail in &check.details {
            writeln!(out, "      {detail}").unwrap();
        }
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
#[allow(clippy::too_many_arguments)]
pub fn cmd_doctor(
    config: &CartogConfig,
    config_path: Option<&Path>,
    config_rejected: bool,
    db_path: &Path,
    project_root: &Path,
    json: bool,
    embedding_dim: usize,
    provider_config: &rag::EmbeddingProviderConfig,
) -> Result<()> {
    // A present-but-rejected config grants consent — the file existing is the
    // opt-in — so a missing config is the only `Absent` case here.
    let file_consent = if config_path.is_some() {
        crate::config::IndexConsent::Granted
    } else {
        crate::config::IndexConsent::Absent
    };
    // The same gate `main` applies, so the "database not found" hint matches what
    // `cartog index` will actually do — e.g. AUTO_INIT alone makes index succeed,
    // so don't tell the user to init. Shared rather than mirrored: the hand-copied
    // version here had drifted, omitting the explicit-`--db` term.
    //
    // `db_override` is None: doctor receives the already-resolved `db_path` and
    // has no `--db` flag of its own.
    let creation =
        crate::config::IndexCreation::resolve(db_path, file_consent, config_rejected, None);
    let db_path_unknown = creation.is_db_path_unknown();
    let consent = creation.is_allowed();
    let mut checks = vec![
        check_git_repo(),
        check_config(config_path, config_rejected),
        check_paths(config_path, db_path, project_root),
        check_database(db_path, embedding_dim, consent, db_path_unknown),
    ];
    #[cfg(feature = "lsp")]
    if let Some(indexed) = indexed_for_lsp(db_path, embedding_dim, project_root) {
        checks.push(check_lsp(
            &indexed.languages,
            &crate::config::to_lsp_overrides(config),
            &indexed.c_family_dirs,
            project_root,
        ));
    }
    checks.push(check_embedding_provider(provider_config));
    checks.push(check_reranker(provider_config));
    checks.push(check_remote(config, config_rejected));
    checks.push(check_project_description(config, project_root));
    // Last: it is the only check that can block for seconds on a blackholed
    // network, and doctor is what you run *because* something is already
    // broken. Every local row is computed before the probe starts.
    checks.push(check_version(
        env!("CARGO_PKG_VERSION"),
        update_check_disabled(),
        || fetch_latest_version_quick(&github_latest_url()),
    ));

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
    /// A config declaring only `[project] description`.
    fn config_describing(description: Option<&str>) -> CartogConfig {
        use crate::config::ProjectConfig;
        CartogConfig {
            project: Some(ProjectConfig {
                name: None,
                description: description.map(str::to_string),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_description_check_passes_on_a_declared_description() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = config_describing(Some("Invoice generation and payment reconciliation."));

        let check = check_project_description(&config, dir.path());

        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.message.contains("(config)"), "{}", check.message);
    }

    #[test]
    fn the_description_check_passes_on_a_readme_paragraph_alone() {
        // The fallback is what makes this check pass for most repos without
        // anyone editing a config.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "# Billing\n\nInvoice generation.\n",
        )
        .unwrap();

        let check = check_project_description(&CartogConfig::default(), dir.path());

        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.message.contains("(readme)"), "{}", check.message);
    }

    #[test]
    fn the_description_check_advises_rather_than_fails_when_neither_source_exists() {
        // Advisory by design: a project with no description still indexes, so
        // an error here would make `doctor` exit 1 on a healthy repo.
        let dir = tempfile::TempDir::new().unwrap();

        let check = check_project_description(&CartogConfig::default(), dir.path());

        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("[project] description"));
        assert!(check.message.contains("README"));
    }

    #[test]
    fn the_declared_description_wins_over_the_readme_in_the_doctor_row() {
        // The row must report what `cartog index` would store, not a second
        // opinion on precedence.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("README.md"), "From the readme.\n").unwrap();
        let config = config_describing(Some("From the config."));

        let check = check_project_description(&config, dir.path());

        assert!(
            check.message.contains("From the config."),
            "{}",
            check.message
        );
        assert!(check.message.contains("(config)"));
    }

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
    fn test_check_database_missing_with_config_suggests_index() {
        let result = check_database(Path::new("/nonexistent/path.db"), 384, true, false);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("not found"));
        assert!(result.message.contains("cartog index"));
        assert!(!result.message.contains("cartog init"));
    }

    #[test]
    fn test_check_database_missing_without_config_suggests_init() {
        // Consent gate: `cartog index` would refuse, so doctor must point at init.
        let result = check_database(Path::new("/nonexistent/path.db"), 384, false, false);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("not found"));
        assert!(
            result.message.contains("cartog init"),
            "got: {}",
            result.message
        );
    }

    #[test]
    fn test_check_database_missing_with_rejected_config_does_not_suggest_index() {
        // A rejected config whose `[database] path` is unknown makes `cartog
        // index` refuse, so advising it would send the user to a command that
        // fails. Point at the real fix instead.
        let result = check_database(Path::new("/nonexistent/path.db"), 384, false, true);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(
            !result.message.contains("run 'cartog index'"),
            "must not advise a command that refuses: {}",
            result.message
        );
        assert!(
            result.message.contains("--db") && result.message.contains("rejected"),
            "must name the real fix: {}",
            result.message
        );
    }

    #[test]
    fn test_check_database_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let _db = Database::open(&db_path, 384).unwrap();
        let result = check_database(&db_path, 384, true, false);
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
                    details: Vec::new(),
                },
                CheckResult {
                    name: "config".into(),
                    status: CheckStatus::Warn,
                    message: "no config".into(),
                    details: Vec::new(),
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
                details: Vec::new(),
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
                details: Vec::new(),
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
                details: Vec::new(),
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Warn,
                message: "meh".into(),
                details: Vec::new(),
            },
            CheckResult {
                name: "c".into(),
                status: CheckStatus::Error,
                message: "bad".into(),
                details: Vec::new(),
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
                details: Vec::new(),
            },
            CheckResult {
                name: "db".into(),
                status: CheckStatus::Ok,
                message: "42 files".into(),
                details: Vec::new(),
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
                details: Vec::new(),
            },
            CheckResult {
                name: "config".into(),
                status: CheckStatus::Warn,
                message: "missing".into(),
                details: Vec::new(),
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
                details: Vec::new(),
            },
            CheckResult {
                name: "embed".into(),
                status: CheckStatus::Warn,
                message: "not cached".into(),
                details: Vec::new(),
            },
            CheckResult {
                name: "db".into(),
                status: CheckStatus::Error,
                message: "broken".into(),
                details: Vec::new(),
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

        let result = check_database(&db_path, 384, true, false);
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

    // ── check_version ─────────────────────────────────────────────────────

    #[test]
    fn version_older_than_latest_warns_with_upgrade_hint() {
        let result = check_version("0.32.0", false, || Ok("0.33.0".to_string()));
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(
            result.message.contains("0.32.0 -> 0.33.0"),
            "{}",
            result.message
        );
        assert!(result.message.contains("cartog self update"));
    }

    #[test]
    fn version_equal_to_latest_is_ok() {
        let result = check_version("0.33.0", false, || Ok("0.33.0".to_string()));
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("up to date"));
    }

    #[test]
    fn version_ahead_of_latest_is_ok() {
        // A dev build past the last release must not be reported as outdated.
        let result = check_version("0.34.0", false, || Ok("0.33.0".to_string()));
        assert_eq!(result.status, CheckStatus::Ok);
    }

    #[test]
    fn version_check_disabled_reports_current_without_probing() {
        let result = check_version("0.33.0", true, || {
            panic!("must not probe when the check is disabled")
        });
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(
            result.message.contains("update check disabled"),
            "{}",
            result.message
        );
    }

    #[test]
    fn version_probe_failure_is_ok_not_error() {
        // Offline is not a broken environment, and doctor exits 1 on any error.
        let result = check_version("0.33.0", false, || anyhow::bail!("connection refused"));
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(
            result.message.contains("latest unknown"),
            "{}",
            result.message
        );
        assert!(result.message.contains("connection refused"));
    }

    // ── check_paths ───────────────────────────────────────────────────────

    #[test]
    fn paths_lists_every_location_as_details() {
        let result = check_paths(
            Some(Path::new("/proj/.cartog.toml")),
            Path::new("/proj/.cartog/db.sqlite"),
            Path::new("/proj"),
        );
        assert_eq!(result.status, CheckStatus::Ok);
        // Content lives in `details` so `--json` consumers never re-split a blob.
        assert!(result.message.is_empty());
        let joined = result.details.join("\n");
        assert!(joined.contains("/proj/.cartog.toml"), "{joined}");
        assert!(joined.contains("/proj/.cartog/db.sqlite"), "{joined}");
        assert!(joined.contains("project root:"), "{joined}");
        assert!(joined.contains("install:"), "{joined}");
    }

    #[test]
    fn paths_reports_defaults_when_no_config_file() {
        let result = check_paths(
            None,
            Path::new("/proj/.cartog/db.sqlite"),
            Path::new("/proj"),
        );
        assert!(
            result
                .details
                .iter()
                .any(|d| d.contains("none (using defaults)")),
            "{:?}",
            result.details
        );
    }

    // ── check_lsp ─────────────────────────────────────────────────────────

    #[cfg(feature = "lsp")]
    mod lsp {
        use super::*;
        use std::collections::HashMap;

        fn langs(pairs: &[(&str, u32)]) -> Vec<(String, u32)> {
            pairs.iter().map(|(l, n)| ((*l).to_string(), *n)).collect()
        }

        #[test]
        fn language_with_no_server_spec_is_never_reported_missing() {
            // Markdown resolves by heuristics only — absence of a server is not a gap.
            let result = check_lsp(
                &langs(&[("markdown", 10)]),
                &HashMap::new(),
                &[],
                Path::new("/proj"),
            );
            assert_eq!(result.status, CheckStatus::Ok);
            assert!(result
                .message
                .contains("no indexed language uses an LSP server"));
        }

        /// Run `f` with an empty `PATH` so no server binary can resolve —
        /// makes the "missing server" branch deterministic instead of
        /// depending on what happens to be installed on the host.
        fn with_empty_path<T>(f: impl FnOnce() -> T) -> T {
            let saved = std::env::var_os("PATH");
            std::env::set_var("PATH", "");
            let out = f();
            match saved {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
            out
        }

        #[test]
        #[serial]
        fn missing_server_warns_and_names_the_install_hint() {
            let result = with_empty_path(|| {
                check_lsp(
                    &langs(&[("csharp", 3)]),
                    &HashMap::new(),
                    &[],
                    Path::new("/proj"),
                )
            });
            assert_eq!(result.status, CheckStatus::Warn);
            let joined = result.details.join(" ");
            assert!(joined.contains("csharp"), "{joined}");
            assert!(joined.contains("dotnet tool install"), "{joined}");
        }

        #[test]
        fn configured_override_counts_as_available_without_a_path_probe() {
            // The override's binary lives in a container, so PATH says nothing.
            let mut overrides = HashMap::new();
            overrides.insert("csharp".to_string(), vec!["docker".to_string()]);
            let result = check_lsp(
                &langs(&[("csharp", 3)]),
                &overrides,
                &[],
                Path::new("/proj"),
            );
            assert_eq!(result.status, CheckStatus::Ok);
            assert!(result.message.contains("csharp"), "{}", result.message);
        }

        #[test]
        fn cpp_without_a_compile_database_warns() {
            let dir = tempfile::TempDir::new().unwrap();
            let mut overrides = HashMap::new();
            overrides.insert("cpp".to_string(), vec!["clangd".to_string()]);
            let result = check_lsp(
                &langs(&[("cpp", 12)]),
                &overrides,
                &[dir.path().join("src")],
                dir.path(),
            );
            assert_eq!(result.status, CheckStatus::Warn);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.contains("compile database")),
                "{:?}",
                result.details
            );
        }

        #[test]
        fn cpp_with_compile_flags_txt_has_no_compile_database_warning() {
            let dir = tempfile::TempDir::new().unwrap();
            std::fs::write(dir.path().join("compile_flags.txt"), "-std=c++17\n").unwrap();
            let mut overrides = HashMap::new();
            overrides.insert("cpp".to_string(), vec!["clangd".to_string()]);
            let result = check_lsp(
                &langs(&[("cpp", 12)]),
                &overrides,
                &[dir.path().join("src")],
                dir.path(),
            );
            assert_eq!(result.status, CheckStatus::Ok);
        }

        #[test]
        #[serial]
        fn compile_database_is_not_reported_when_clangd_itself_is_missing() {
            // Otherwise a machine without clangd double-reports the same gap:
            // the missing server is the finding, not the missing compile db.
            let dir = tempfile::TempDir::new().unwrap();
            let result = with_empty_path(|| {
                check_lsp(
                    &langs(&[("cpp", 12)]),
                    &HashMap::new(),
                    &[dir.path().join("src")],
                    dir.path(),
                )
            });
            assert_eq!(result.status, CheckStatus::Warn);
            assert!(
                result.details.iter().any(|d| d.contains("clangd")),
                "expected the missing-server finding: {:?}",
                result.details
            );
            // The install hint mentions a compile database too, so match the
            // distinct finding, not the substring.
            assert!(
                !result
                    .details
                    .iter()
                    .any(|d| d.starts_with("clangd has no")),
                "{:?}",
                result.details
            );
        }

        #[test]
        fn compile_database_in_the_source_directory_satisfies_the_check() {
            // clangd searches the file's own directory first.
            let dir = tempfile::TempDir::new().unwrap();
            let src = dir.path().join("native");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.join("compile_commands.json"), "[]").unwrap();
            let mut overrides = HashMap::new();
            overrides.insert("cpp".to_string(), vec!["clangd".to_string()]);
            let result = check_lsp(&langs(&[("cpp", 12)]), &overrides, &[src], dir.path());
            assert_eq!(result.status, CheckStatus::Ok);
        }

        #[test]
        fn compile_database_warning_names_the_source_directory_not_the_repo_root() {
            // A polyglot repo (Rust workspace + a C fixture) must not be told to
            // put a compile database at a root where it would be meaningless.
            let dir = tempfile::TempDir::new().unwrap();
            let src = dir.path().join("benchmarks/fixtures/webapp_cpp");
            std::fs::create_dir_all(&src).unwrap();
            let mut overrides = HashMap::new();
            overrides.insert("cpp".to_string(), vec!["clangd".to_string()]);
            let result = check_lsp(&langs(&[("cpp", 12)]), &overrides, &[src], dir.path());
            assert_eq!(result.status, CheckStatus::Warn);
            let joined = result.details.join(" ");
            assert!(joined.contains("webapp_cpp"), "{joined}");
        }

        #[test]
        #[serial]
        fn warn_row_still_reports_the_servers_that_were_found() {
            // The positive half is what a bug report needs; a single gap must
            // not hide the eleven languages that did resolve.
            let mut overrides = HashMap::new();
            overrides.insert("rust".to_string(), vec!["rust-analyzer".to_string()]);
            let result = with_empty_path(|| {
                check_lsp(
                    &langs(&[("rust", 20), ("csharp", 3)]),
                    &overrides,
                    &[],
                    Path::new("/proj"),
                )
            });
            assert_eq!(result.status, CheckStatus::Warn);
            let joined = result.details.join(" ");
            assert!(joined.contains("servers found for rust"), "{joined}");
            assert!(joined.contains("csharp"), "{joined}");
        }

        #[test]
        #[serial]
        fn missing_multi_spec_language_lists_every_candidate_server() {
            // Ruby ships two servers with different minimum runtimes; naming
            // only the first can hand the user the one hint they cannot meet.
            let result = with_empty_path(|| {
                check_lsp(
                    &langs(&[("ruby", 5)]),
                    &HashMap::new(),
                    &[],
                    Path::new("/proj"),
                )
            });
            assert_eq!(result.status, CheckStatus::Warn);
            let joined = result.details.join(" ");
            assert!(joined.contains("ruby-lsp"), "{joined}");
            assert!(joined.contains("solargraph"), "{joined}");
        }
    }

    #[test]
    #[cfg(feature = "lsp")]
    fn indexed_for_lsp_is_none_when_database_is_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(indexed_for_lsp(&dir.path().join("missing.sqlite"), 384, dir.path()).is_none());
    }
}
