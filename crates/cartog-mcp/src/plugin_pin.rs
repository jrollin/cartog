//! The Claude Code plugin's pinned cartog version, as seen by a running server.
//!
//! The binary resolves the pin before starting the server (manifest lookup,
//! version compare, install-source-specific update command) and hands the
//! result in through `ServerOptions::plugin_pin`; this crate never reads the
//! environment for it. `None` outside the plugin (manual MCP wiring, tests).

/// What the binary resolved about the plugin pin before starting the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPin {
    /// Bare `MAJOR.MINOR.PATCH` from the plugin manifest.
    pub version: String,
    /// True when the running binary is older than `version`.
    pub behind: bool,
    /// The command that brings the binary to `version` for this install
    /// source: `/cartog-install`, or `cargo install cartog --force` for a
    /// cargo-managed binary that `cartog self update` refuses to swap.
    pub update_command: String,
}

/// Pull a bare `MAJOR.MINOR.PATCH` `version` out of a plugin manifest.
///
/// Anything else (missing field, `v` prefix, prerelease suffix, unparseable
/// JSON) is `None`, so a malformed pin falls back to "latest stable" rather
/// than arming garbage.
#[must_use]
pub fn parse_plugin_pin(manifest: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(manifest).ok()?;
    let version = parsed.get("version")?.as_str()?;
    let parts: Vec<&str> = version.split('.').collect();
    let bare = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    bare.then(|| version.to_string())
}
