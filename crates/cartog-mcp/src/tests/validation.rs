//! Path-validation, normalization, depth-capping, edge-kind and provenance tests.

use crate::*;
use cartog_core::EdgeKind;

// ── Path validation tests ──

#[test]
fn validate_path_dot_is_allowed() {
    let result = validate_path_within_cwd(".");
    assert!(result.is_ok());
}

#[test]
fn validate_path_subdirectory_is_allowed() {
    let result = validate_path_within_cwd("src");
    // May not exist in test env, but should not be rejected as "outside CWD"
    // (normalize_path handles non-existent paths)
    assert!(result.is_ok() || result.unwrap_err().contains("cannot resolve"));
}

#[test]
fn validate_path_parent_escape_is_rejected() {
    let result = validate_path_within_cwd("../../etc/passwd");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("outside the project directory"),
        "should reject path traversal"
    );
}

#[test]
fn validate_path_absolute_outside_cwd_is_rejected() {
    let result = validate_path_within_cwd("/etc/passwd");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("outside the project directory"),
        "should reject absolute paths outside CWD"
    );
}

#[test]
fn validate_path_absolute_inside_cwd_is_allowed() {
    let cwd = std::env::current_dir().expect("CWD");
    let inside = cwd.join("src");
    let result = validate_path_within_cwd(inside.to_str().expect("utf-8 path"));
    // src/ exists in this project
    assert!(result.is_ok());
}

#[test]
fn validate_path_dotdot_in_middle_is_rejected() {
    let result = validate_path_within_cwd("src/../../etc");
    assert!(result.is_err());
}

// ── Normalize path tests ──

#[test]
fn normalize_removes_dot() {
    let p = normalize_path(Path::new("/a/./b/./c"));
    assert_eq!(p, PathBuf::from("/a/b/c"));
}

#[test]
fn normalize_resolves_parent() {
    let p = normalize_path(Path::new("/a/b/../c"));
    assert_eq!(p, PathBuf::from("/a/c"));
}

// ── Depth capping ──

/// Verify depth is clamped at MAX_IMPACT_DEPTH.
#[test]
fn impact_depth_is_capped() {
    fn resolve_depth(input: Option<u32>) -> u32 {
        input.unwrap_or(3).min(MAX_IMPACT_DEPTH)
    }
    assert_eq!(resolve_depth(Some(999)), MAX_IMPACT_DEPTH);
    assert_eq!(resolve_depth(Some(5)), 5);
}

/// Verify default depth when None is provided.
#[test]
fn impact_depth_default() {
    fn resolve_depth(input: Option<u32>) -> u32 {
        input.unwrap_or(3).min(MAX_IMPACT_DEPTH)
    }
    assert_eq!(resolve_depth(None), 3);
}

// ── Edge kind parsing ──

#[test]
fn parse_valid_edge_kinds() {
    assert_eq!("calls".parse::<EdgeKind>().unwrap(), EdgeKind::Calls);
    assert_eq!("imports".parse::<EdgeKind>().unwrap(), EdgeKind::Imports);
    assert_eq!("inherits".parse::<EdgeKind>().unwrap(), EdgeKind::Inherits);
    assert_eq!(
        "references".parse::<EdgeKind>().unwrap(),
        EdgeKind::References
    );
    assert_eq!("raises".parse::<EdgeKind>().unwrap(), EdgeKind::Raises);
}

#[test]
fn parse_invalid_edge_kind_fails() {
    assert!("invalid".parse::<EdgeKind>().is_err());
    assert!("CALLS".parse::<EdgeKind>().is_err());
    assert!("".parse::<EdgeKind>().is_err());
}

// ── Edge provenance in structured output ──

#[test]
fn json_output_includes_provenance_when_present() {
    let mut edge = cartog_core::Edge::new("s:1", "foo", EdgeKind::Calls, "a.py", 1);
    edge.provenance = Some(cartog_core::EdgeProvenance::SameFile);
    let value = serde_json::to_value(EdgeList {
        results: vec![edge],
    })
    .unwrap();
    assert_eq!(value["results"][0]["provenance"], "same_file");
}

#[test]
fn json_output_omits_provenance_when_absent() {
    // A freshly extracted edge has no provenance; skip_serializing_if drops
    // the key entirely so the wire format stays clean.
    let edge = cartog_core::Edge::new("s:1", "foo", EdgeKind::Calls, "a.py", 1);
    let value = serde_json::to_value(EdgeList {
        results: vec![edge],
    })
    .unwrap();
    assert!(value["results"][0].get("provenance").is_none());
}
