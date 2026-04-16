//! Build script: emit compile-time env vars used by `cartog --version --verbose`.
//!
//! - `CARTOG_BUILD_SHA`: short git SHA, or "unknown" outside a git checkout.
//! - `CARTOG_BUILD_FEATURES`: comma-separated list of enabled Cargo features,
//!   or "none" when the crate is built with no extras.

use std::env;
use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CARTOG_BUILD_SHA={sha}");

    // Cargo sets CARGO_FEATURE_<UPPERCASE_NAME>=1 for each enabled feature.
    // Collect them into a sorted, comma-joined string.
    let mut features: Vec<String> = env::vars()
        .filter_map(|(k, _)| {
            k.strip_prefix("CARGO_FEATURE_").map(|rest| {
                // Cargo uppercases and replaces `-` with `_`; restore the
                // canonical form used in Cargo.toml.
                rest.to_ascii_lowercase().replace('_', "-")
            })
        })
        .collect();
    features.sort();
    let features_str = if features.is_empty() {
        "none".to_string()
    } else {
        features.join(", ")
    };
    println!("cargo:rustc-env=CARTOG_BUILD_FEATURES={features_str}");

    // Re-run when git HEAD moves. Resolve the path via git itself so we
    // handle git worktrees (where .git is a file, not a dir) and
    // unusual layouts correctly. When git isn't available (e.g.
    // `cargo install` from crates.io with no .git/), fall back to a
    // rerun-if-env-changed so cargo can still cache deterministically.
    let head_path = Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match head_path {
        Some(p) => println!("cargo:rerun-if-changed={p}"),
        // Keep the build script deterministic when we're not in a git
        // checkout: let the user force a rebuild via CARTOG_BUILD_SHA.
        None => println!("cargo:rerun-if-env-changed=CARTOG_BUILD_SHA"),
    }
}
