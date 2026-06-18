//! Tests for version reporting and update-availability checks.

use std::path::Path;

use crate::commands::self_cmd::*;

#[test]
fn render_human_shows_describe_value_separately_from_version() {
    let info = VersionInfo {
        version: "0.29.1".into(),
        build_version: "v0.29.1-2-g3e2822c".into(),
        target: "x".into(),
        install_source: "dev".into(),
        last_update_check: None,
        pending_update: None,
    };
    let out = info.render_human();
    // The version line carries the bare semver, the describe line its own value
    // — assert on the values, not the column padding.
    assert!(out.lines().any(|l| l == "cartog 0.29.1"));
    assert!(out
        .lines()
        .any(|l| l.trim_start().starts_with("describe:") && l.contains("v0.29.1-2-g3e2822c")));
}

// ── resolve_install_source ────────────────────────────────────────

#[test]
fn resolve_release_tarball_short_circuits_runtime_detection() {
    // A release-built binary stays "release-tarball" no matter where
    // it sits on disk.
    assert_eq!(
        resolve_install_source(
            "release-tarball",
            Some(Path::new("/home/user/.cargo/bin/cartog")),
            None,
        ),
        "release-tarball",
    );
}

#[test]
fn resolve_dev_binary_in_cargo_home_classified_as_cargo() {
    let cargo_home = Path::new("/home/user/.cargo");
    let bin = Path::new("/home/user/.cargo/bin/cartog");
    assert_eq!(
        resolve_install_source("dev", Some(bin), Some(cargo_home)),
        "cargo",
    );
}

#[test]
fn resolve_dev_binary_outside_cargo_classified_as_dev() {
    assert_eq!(
        resolve_install_source(
            "dev",
            Some(Path::new("/usr/local/bin/cartog")),
            Some(Path::new("/home/user/.cargo")),
        ),
        "dev",
    );
}

#[test]
fn resolve_dev_falls_back_to_path_heuristic_when_cargo_home_unset() {
    // Catches `~/.cargo/bin` even without CARGO_HOME (common on macOS).
    assert_eq!(
        resolve_install_source(
            "dev",
            Some(Path::new("/Users/alice/.cargo/bin/cartog")),
            None,
        ),
        "cargo",
    );
}

#[test]
fn resolve_dev_no_binary_path_stays_dev() {
    assert_eq!(resolve_install_source("dev", None, None), "dev");
}

// ── parse_release_tag / is_stable_semver ──────────────────────────

#[test]
fn parse_release_tag_strips_v_prefix() {
    assert_eq!(
        parse_release_tag(r#"{"tag_name":"v0.14.0"}"#),
        Some("0.14.0".to_string()),
    );
    assert_eq!(
        parse_release_tag(r#"{"tag_name":"0.14.0"}"#),
        Some("0.14.0".to_string()),
        "leading v is optional",
    );
}

#[test]
fn parse_release_tag_rejects_prereleases() {
    for tag in [
        r#"{"tag_name":"v0.14.0-rc.1"}"#,
        r#"{"tag_name":"v0.14.0-alpha"}"#,
        r#"{"tag_name":"v0.14.0-nightly.42"}"#,
    ] {
        assert_eq!(parse_release_tag(tag), None, "must reject {tag}");
    }
}

#[test]
fn parse_release_tag_rejects_malformed() {
    assert_eq!(parse_release_tag("not json at all"), None);
    assert_eq!(parse_release_tag(r#"{}"#), None, "missing tag_name");
    assert_eq!(
        parse_release_tag(r#"{"tag_name":"v0.14"}"#),
        None,
        "two-part is not stable semver",
    );
    assert_eq!(
        parse_release_tag(r#"{"tag_name":"v0.14.0.0"}"#),
        None,
        "four-part is not stable semver",
    );
    assert_eq!(
        parse_release_tag(r#"{"tag_name":"vfoo.bar.baz"}"#),
        None,
        "non-numeric components rejected",
    );
}

#[test]
fn is_stable_semver_accepts_canonical_triples() {
    assert!(is_stable_semver("0.0.0"));
    assert!(is_stable_semver("1.2.3"));
    assert!(is_stable_semver("99.0.0"));
}

#[test]
fn is_stable_semver_rejects_anything_else() {
    assert!(!is_stable_semver(""));
    assert!(!is_stable_semver("1.2"));
    assert!(!is_stable_semver("1.2.3.4"));
    assert!(!is_stable_semver("1.2.x"));
    assert!(!is_stable_semver("1..3"));
}

// ── compare_stable_versions ───────────────────────────────────────

#[test]
fn compare_stable_versions_orders_lexicographically() {
    use std::cmp::Ordering::*;
    assert_eq!(compare_stable_versions("0.13.2", "0.14.0"), Less);
    assert_eq!(compare_stable_versions("0.14.0", "0.13.2"), Greater);
    assert_eq!(compare_stable_versions("0.14.0", "0.14.0"), Equal);
    assert_eq!(compare_stable_versions("1.0.0", "0.99.99"), Greater);
    assert_eq!(compare_stable_versions("0.13.10", "0.13.2"), Greater);
}

#[test]
fn compare_stable_versions_garbage_treated_as_zero() {
    // Documented degradation: non-numeric parts become 0 instead of panicking.
    use std::cmp::Ordering::*;
    assert_eq!(compare_stable_versions("0.0.0", "abc.def.ghi"), Equal);
    assert_eq!(compare_stable_versions("0.1.0", "abc.def.ghi"), Greater);
}

// ── CheckOutcome ──────────────────────────────────────────────────

#[test]
fn check_outcome_ok_marks_outdated_when_latest_is_newer() {
    let outcome = CheckOutcome::ok("0.13.2", "0.14.0");
    assert_eq!(outcome.outdated, Some(true));
    assert_eq!(outcome.latest.as_deref(), Some("0.14.0"));
    assert_eq!(outcome.current, "0.13.2");
    assert_eq!(outcome.error, None);
}

#[test]
fn check_outcome_ok_not_outdated_when_versions_match() {
    let outcome = CheckOutcome::ok("0.14.0", "0.14.0");
    assert_eq!(outcome.outdated, Some(false));
}

#[test]
fn check_outcome_ok_not_outdated_when_local_is_ahead() {
    // Pre-release dev builds (e.g. 0.15.0 mid-development) must not be
    // told to "downgrade" to a published 0.14.0.
    let outcome = CheckOutcome::ok("0.15.0", "0.14.0");
    assert_eq!(outcome.outdated, Some(false));
}

#[test]
fn check_outcome_failed_reports_error_with_null_latest() {
    let outcome = CheckOutcome::failed("0.13.2", "connection refused");
    assert_eq!(outcome.latest, None);
    assert_eq!(outcome.outdated, None);
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
}

#[test]
fn check_outcome_to_human_outdated() {
    let s = CheckOutcome::ok("0.13.2", "0.14.0").to_human();
    assert!(s.contains("update available"), "got: {s}");
    assert!(s.contains("0.13.2"));
    assert!(s.contains("0.14.0"));
}

#[test]
fn check_outcome_to_human_up_to_date() {
    let s = CheckOutcome::ok("0.14.0", "0.14.0").to_human();
    assert!(s.contains("up to date"), "got: {s}");
    assert!(s.contains("0.14.0"));
}

#[test]
fn check_outcome_to_human_failed() {
    let s = CheckOutcome::failed("0.13.2", "DNS lookup failed").to_human();
    assert!(s.contains("update check failed"), "got: {s}");
    assert!(s.contains("DNS lookup failed"));
}

#[test]
fn check_outcome_serialises_with_skip_none_keys() {
    // Failure shape must omit `latest` and `outdated`, never serialise as null.
    let outcome = CheckOutcome::failed("0.13.2", "boom");
    let json = serde_json::to_string(&outcome).unwrap();
    assert!(json.contains(r#""error":"boom""#));
    assert!(json.contains(r#""current":"0.13.2""#));
    assert!(
        !json.contains("latest"),
        "latest must be skipped, got: {json}"
    );
    assert!(
        !json.contains("outdated"),
        "outdated must be skipped, got: {json}"
    );
}

#[test]
fn check_outcome_serialises_success_with_all_fields() {
    let outcome = CheckOutcome::ok("0.13.2", "0.14.0");
    let json = serde_json::to_string(&outcome).unwrap();
    assert!(json.contains(r#""current":"0.13.2""#));
    assert!(json.contains(r#""latest":"0.14.0""#));
    assert!(json.contains(r#""outdated":true"#));
    assert!(!json.contains("error"), "error must be skipped on success");
}

// ── looks_like_cargo_install ─────────────────────────────────────

#[test]
fn cargo_install_detected_via_cargo_home_prefix() {
    assert!(looks_like_cargo_install(
        Path::new("/home/u/.cargo/bin/cartog"),
        Some(Path::new("/home/u/.cargo")),
    ));
}

#[test]
fn cargo_install_detected_via_dotcargo_path_segment() {
    assert!(looks_like_cargo_install(
        Path::new("/Users/alice/.cargo/bin/cartog"),
        None,
    ));
}

#[test]
fn cargo_install_not_detected_for_unrelated_paths() {
    assert!(!looks_like_cargo_install(
        Path::new("/usr/local/bin/cartog"),
        None,
    ));
    assert!(!looks_like_cargo_install(
        Path::new("/home/u/.cargo-tools/bin/cartog"),
        None,
    ));
}
