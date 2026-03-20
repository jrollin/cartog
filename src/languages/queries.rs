//! Shared tree-sitter query helpers for language extractors.
//!
//! Provides utilities to run declarative S-expression queries against tree-sitter
//! syntax trees, replacing verbose manual cursor-walk loops with concise patterns.

use tree_sitter::{Language, Node, Query, QueryCursor};

use crate::types::{Edge, EdgeKind};

use super::node_text;

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

    /// Get the index of a named capture, or panic if it doesn't exist.
    pub fn capture_index(&self, name: &str) -> u32 {
        self.query
            .capture_index_for_name(name)
            .unwrap_or_else(|| panic!("capture @{name} not found in query"))
    }
}

/// Walk a subtree collecting call edges using a pre-compiled query.
///
/// This replaces the manual `walk_for_calls` cursor loops used across all extractors.
/// The query should capture `@callee` for the function/method name node.
pub fn collect_call_edges(
    query: &CachedQuery,
    callee_idx: u32,
    node: Node,
    source: &str,
    file_path: &str,
    context_id: &str,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = QueryCursor::new();
    let matches = cursor.matches(&query.query, node, source.as_bytes());

    for m in matches {
        for capture in m.captures {
            if capture.index == callee_idx {
                let callee_name = node_text(capture.node, source);
                if !callee_name.is_empty() {
                    edges.push(Edge::new(
                        context_id,
                        callee_name,
                        EdgeKind::Calls,
                        file_path,
                        capture.node.start_position().row as u32 + 1,
                    ));
                }
            }
        }
    }
}

/// Walk a subtree collecting edges of any kind using a pre-compiled query.
///
/// Generic version that takes edge kind as a parameter. The query should
/// capture `@target` for the target name node.
pub fn collect_edges(
    query: &CachedQuery,
    target_idx: u32,
    kind: EdgeKind,
    node: Node,
    source: &str,
    file_path: &str,
    context_id: &str,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = QueryCursor::new();
    let matches = cursor.matches(&query.query, node, source.as_bytes());

    for m in matches {
        for capture in m.captures {
            if capture.index == target_idx {
                let name = node_text(capture.node, source);
                if !name.is_empty() {
                    edges.push(Edge::new(
                        context_id,
                        name,
                        kind,
                        file_path,
                        capture.node.start_position().row as u32 + 1,
                    ));
                }
            }
        }
    }
}

/// Walk a subtree collecting type reference edges (uppercase identifiers only).
///
/// Filters out lowercase names (builtins like `int`, `str`, `bool`).
pub fn collect_type_ref_edges(
    query: &CachedQuery,
    target_idx: u32,
    node: Node,
    source: &str,
    file_path: &str,
    context_id: &str,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = QueryCursor::new();
    let matches = cursor.matches(&query.query, node, source.as_bytes());

    for m in matches {
        for capture in m.captures {
            if capture.index == target_idx {
                let name = node_text(capture.node, source);
                if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_uppercase()) {
                    edges.push(Edge::new(
                        context_id,
                        name,
                        EdgeKind::References,
                        file_path,
                        capture.node.start_position().row as u32 + 1,
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cached_query_compiles() {
        let lang = Language::new(tree_sitter_python::LANGUAGE);
        let q = CachedQuery::new(&lang, "(call function: (identifier) @callee)");
        assert_eq!(q.capture_index("callee"), 0);
    }

    #[test]
    fn test_collect_call_edges_python() {
        let lang = Language::new(tree_sitter_python::LANGUAGE);
        let q = CachedQuery::new(&lang, "(call function: (identifier) @callee)");
        let callee_idx = q.capture_index("callee");

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let source = "def foo():\n    bar()\n    baz(42)\n";
        let tree = parser.parse(source, None).unwrap();

        // Find the function body to search within
        let root = tree.root_node();
        let mut edges = Vec::new();
        collect_call_edges(&q, callee_idx, root, source, "test.py", "test:foo:1", &mut edges);

        assert_eq!(edges.len(), 2);
        let targets: Vec<&str> = edges.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"bar"));
        assert!(targets.contains(&"baz"));
    }

    #[test]
    fn test_collect_type_ref_edges_filters_lowercase() {
        let lang = Language::new(tree_sitter_python::LANGUAGE);
        let q = CachedQuery::new(&lang, "(identifier) @target");
        let target_idx = q.capture_index("target");

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let source = "x: int = User()";
        let tree = parser.parse(source, None).unwrap();

        let mut edges = Vec::new();
        collect_type_ref_edges(
            &q,
            target_idx,
            tree.root_node(),
            source,
            "test.py",
            "test:x:1",
            &mut edges,
        );

        let targets: Vec<&str> = edges.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"User"));
        assert!(!targets.contains(&"int"));
        assert!(!targets.contains(&"x"));
    }
}
