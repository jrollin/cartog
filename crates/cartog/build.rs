//! Emit compile-time env vars surfaced by `cartog --version` and `cartog self version`.

use std::env;
use std::process::Command;

/// Run `git <args>` and return trimmed stdout, or `None` on failure / empty output.
fn git_output(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn main() {
    let sha =
        git_output(&["rev-parse", "--short=10", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CARTOG_BUILD_SHA={sha}");

    // Display-only `git describe` (e.g. `v0.29.1-2-g3e2822c`) so unreleased main
    // builds are visibly distinct from a release. NOT the comparison key —
    // CARGO_PKG_VERSION stays the clean semver `self update`/crates.io use.
    let describe = git_output(&["describe", "--tags", "--dirty", "--always"])
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CARTOG_BUILD_VERSION={describe}");

    let mut features: Vec<String> = env::vars()
        .filter_map(|(k, _)| {
            k.strip_prefix("CARGO_FEATURE_")
                .map(|rest| rest.to_ascii_lowercase().replace('_', "-"))
        })
        .collect();
    features.sort();
    let features_str = if features.is_empty() {
        "none".to_string()
    } else {
        features.join(", ")
    };
    println!("cargo:rustc-env=CARTOG_BUILD_FEATURES={features_str}");

    // Cargo-installed binaries are detected at runtime by inspecting the
    // binary path; only release-tarball vs dev is decidable at build time.
    let install_source = if env::var_os("CARTOG_RELEASE_BUILD").is_some() {
        "release-tarball"
    } else {
        "dev"
    };
    println!("cargo:rustc-env=CARTOG_INSTALL_SOURCE={install_source}");
    println!("cargo:rerun-if-env-changed=CARTOG_RELEASE_BUILD");

    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=CARTOG_TARGET_TRIPLE={target}");

    // Re-run the build script when the commit changes. Emitting ANY rerun-if
    // disables cargo's default "rebuild on any package file change", so the SHA
    // and describe strings would otherwise freeze at first build. .git/HEAD only
    // changes on branch switch — a plain `git commit` moves the branch ref, so
    // we must also watch the resolved ref file and packed-refs. `git describe`'s
    // `-dirty` flag tracks the working tree, which no .git path reflects; that
    // suffix is therefore best-effort and may lag until a watched ref changes.
    // Falls back to env-changed when not in a checkout (`cargo install`).
    let mut watched = false;
    for path in git_rerun_paths() {
        println!("cargo:rerun-if-changed={path}");
        watched = true;
    }
    if !watched {
        println!("cargo:rerun-if-env-changed=CARTOG_BUILD_SHA");
    }
}

/// Git paths whose change should re-run the build script: `HEAD`, the branch
/// ref `HEAD` points at, and `packed-refs`. Resolved via git so worktrees
/// (`.git` as a file) and custom git dirs work. Missing paths are skipped.
fn git_rerun_paths() -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(head) = git_output(&["rev-parse", "--git-path", "HEAD"]) {
        paths.push(head);
    }
    // The branch ref that a plain `git commit` advances (e.g. refs/heads/main).
    if let Some(symref) = git_output(&["symbolic-ref", "--quiet", "HEAD"]) {
        if let Some(ref_path) = git_output(&["rev-parse", "--git-path", &symref]) {
            paths.push(ref_path);
        }
    }
    if let Some(packed) = git_output(&["rev-parse", "--git-path", "packed-refs"]) {
        paths.push(packed);
    }
    paths
}
