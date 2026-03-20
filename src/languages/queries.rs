//! Shared tree-sitter query helpers for language extractors.
//!
//! Provides utilities to run declarative S-expression queries against tree-sitter
//! syntax trees, replacing verbose manual cursor-walk loops with concise patterns.

use tree_sitter::{Language, Node, Query};

/// A compiled query cached for reuse across multiple files.
///
/// Compile once per extractor (in `new()`), reuse on every `extract()` call.
pub struct CachedQuery {
    pub query: Query,
}

impl CachedQuery {
    /// Compile a tree-sitter S-expression query for the given language.
    ///
    /// # Panics
    /// Panics if the query pattern is invalid — this indicates a bug in the
    /// hard-coded query string, not a runtime error.
    pub fn new(language: &Language, pattern: &str) -> Self {
        let query = Query::new(language, pattern)
            .unwrap_or_else(|e| panic!("invalid tree-sitter query: {e}\npattern: {pattern}"));
        Self { query }
    }

    /// Try to compile a query, returning `None` if the pattern is invalid
    /// (e.g. the node type doesn't exist in this grammar).
    pub fn try_new(language: &Language, pattern: &str) -> Option<Self> {
        Query::new(language, pattern)
            .ok()
            .map(|query| Self { query })
    }

    /// Get the index of a named capture, or panic if it doesn't exist.
    pub fn capture_index(&self, name: &str) -> u32 {
        self.query
            .capture_index_for_name(name)
            .unwrap_or_else(|| panic!("capture @{name} not found in query"))
    }
}

/// Check whether a captured node is inside a nested scope relative to a root node.
///
/// Walks up the tree from `node` to `root`. If any ancestor between them matches
/// one of the `scope_kinds`, returns `true` — meaning the node belongs to a nested
/// scope and should be skipped when extracting edges for the root scope.
pub fn is_inside_nested_scope(node: Node, root: Node, scope_kinds: &[&str]) -> bool {
    let mut current = node.parent();
    while let Some(p) = current {
        if p.id() == root.id() {
            return false;
        }
        if scope_kinds.contains(&p.kind()) {
            return true;
        }
        current = p.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Language;

    #[test]
    fn test_cached_query_compiles() {
        let lang = Language::new(tree_sitter_python::LANGUAGE);
        let q = CachedQuery::new(&lang, "(call function: (identifier) @callee)");
        assert_eq!(q.capture_index("callee"), 0);
    }
}
