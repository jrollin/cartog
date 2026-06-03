//! Edge resolution provenance: which tier or source resolved an [`Edge`](crate::Edge).
//!
//! Recorded at resolution time (not extraction) and surfaced in `--json`/MCP output so
//! agents can weigh an LSP-precise edge against a weak heuristic guess. Persisted as the
//! `edges.resolution_source` TEXT column; legacy rows resolved before this existed carry
//! `None`.

use serde::Serialize;

/// Which resolution tier or source produced an edge's `target_id`.
///
/// The first six variants are the heuristic name-resolution tiers (in priority order);
/// the last three are LSP outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeProvenance {
    /// Tier 1 — symbol with matching name in the same file.
    SameFile,
    /// Tier 2 — followed a resolved import to the target's file.
    ImportPath,
    /// Tier 3 — symbol in a file in the same directory.
    SameDir,
    /// Tier 4 — preferred a match sharing the source's parent scope.
    ParentScope,
    /// Tier 5 — exactly one symbol with that name project-wide.
    UniqueGlobal,
    /// Tier 6 — two matches, disambiguated by kind (class over constructor).
    KindDisambig,
    /// LSP located the definition inside the indexed root.
    Lsp,
    /// LSP located the definition outside the indexed root (stdlib, deps, node_modules).
    LspExternal,
    /// LSP definitively returned no definition (typo, dynamic dispatch, macro).
    LspUnresolvable,
}

impl EdgeProvenance {
    /// Stable lowercase identifier used for the `resolution_source` column and JSON output.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SameFile => "same_file",
            Self::ImportPath => "import_path",
            Self::SameDir => "same_dir",
            Self::ParentScope => "parent_scope",
            Self::UniqueGlobal => "unique_global",
            Self::KindDisambig => "kind_disambig",
            Self::Lsp => "lsp",
            Self::LspExternal => "lsp_external",
            Self::LspUnresolvable => "lsp_unresolvable",
        }
    }
}

impl std::str::FromStr for EdgeProvenance {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "same_file" => Ok(Self::SameFile),
            "import_path" => Ok(Self::ImportPath),
            "same_dir" => Ok(Self::SameDir),
            "parent_scope" => Ok(Self::ParentScope),
            "unique_global" => Ok(Self::UniqueGlobal),
            "kind_disambig" => Ok(Self::KindDisambig),
            "lsp" => Ok(Self::Lsp),
            "lsp_external" => Ok(Self::LspExternal),
            "lsp_unresolvable" => Ok(Self::LspUnresolvable),
            _ => Err(anyhow::anyhow!("unknown edge provenance: '{s}'")),
        }
    }
}

impl std::fmt::Display for EdgeProvenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[EdgeProvenance] = &[
        EdgeProvenance::SameFile,
        EdgeProvenance::ImportPath,
        EdgeProvenance::SameDir,
        EdgeProvenance::ParentScope,
        EdgeProvenance::UniqueGlobal,
        EdgeProvenance::KindDisambig,
        EdgeProvenance::Lsp,
        EdgeProvenance::LspExternal,
        EdgeProvenance::LspUnresolvable,
    ];

    #[test]
    fn round_trips_through_str() {
        for &p in ALL {
            assert_eq!(p.as_str().parse::<EdgeProvenance>().unwrap(), p);
        }
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert!("bogus".parse::<EdgeProvenance>().is_err());
    }

    #[test]
    fn str_repr_is_snake_case() {
        assert_eq!(EdgeProvenance::SameFile.as_str(), "same_file");
        assert_eq!(EdgeProvenance::KindDisambig.as_str(), "kind_disambig");
        assert_eq!(EdgeProvenance::LspExternal.as_str(), "lsp_external");
    }
}
