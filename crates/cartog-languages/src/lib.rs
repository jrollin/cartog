//! Tree-sitter language extractors for the cartog code graph.
//!
//! Parses source code using tree-sitter grammars and extracts symbols and edges.
//! Each language implements the [`Extractor`] trait with compiled S-expression
//! queries for declarative AST pattern matching.
//!
//! Supported languages: Python, TypeScript, TSX, JavaScript, Rust, Go, Ruby, Java, PHP, Dart, Swift, Kotlin.
#![doc = ""]
#![doc = include_str!("../README.md")]

pub mod dart;
pub mod go;
pub mod java;
pub mod javascript;
mod js_shared;
pub mod kotlin;
pub mod markdown;
pub mod php;
pub mod python;
pub(crate) mod queries;
pub mod ruby;
pub mod rust_lang;
pub mod swift;
pub mod typescript;

use anyhow::Result;
use cartog_core::{Edge, Symbol};
use tree_sitter::Node;

/// Result of extracting symbols and edges from a source file.
#[derive(Debug, Clone, Default)]
pub struct ExtractionResult {
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
}

/// Trait implemented by each language extractor.
///
/// `extract` takes `&mut self` so implementations can reuse an internal
/// `tree_sitter::Parser` across calls instead of allocating a new one per file.
pub trait Extractor: Send {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult>;
}

/// Extract the text of a tree-sitter node from the source.
/// Returns an empty string if byte offsets fall outside the source or on a char boundary.
pub(crate) fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

/// Last segment of `s` after the final `sep`, or all of `s` if `sep` is absent.
/// Used to turn dotted/slashed import paths into a bare target name
/// (`a.b.C` → `C`, `pkg/mod` → `mod`, `crate::path::Item` → `Item`).
pub(crate) fn last_segment<'a>(s: &'a str, sep: &str) -> &'a str {
    s.rsplit(sep).next().unwrap_or(s)
}

/// Qualified name: `Parent.name` when nested, else `name`.
pub(crate) fn qualified(parent_qname: Option<&str>, name: &str) -> String {
    match parent_qname {
        Some(p) => format!("{p}.{name}"),
        None => name.to_string(),
    }
}

/// Deepest AST nesting a recursive extractor will descend before bailing. Recursive
/// walkers use one stack frame per level; pathological/generated source would
/// otherwise overflow the worker stack and abort the whole index run.
pub(crate) const MAX_TREE_DEPTH: usize = 600;

/// Iterative (non-recursive) check that the tree's max depth stays within `limit`.
/// Uses a `TreeCursor` so it can't itself overflow on the very input it guards.
pub(crate) fn tree_depth_exceeds(root: Node, limit: usize) -> bool {
    let mut cursor = root.walk();
    let mut depth = 0usize;
    loop {
        if depth > limit {
            return true;
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return false;
            }
            depth -= 1;
        }
    }
}

/// Enclosing scope while extracting: `id` becomes the child's `parent_id`,
/// `qname` its `parent_name`. Top level: `id` is `None`, `qname` the namespace.
#[derive(Clone, Copy, Default)]
pub(crate) struct ParentScope<'a> {
    pub id: Option<&'a str>,
    pub qname: Option<&'a str>,
}

impl<'a> ParentScope<'a> {
    /// Top-level scope, optionally within a namespace.
    pub fn top_level(namespace: Option<&'a str>) -> Self {
        Self {
            id: None,
            qname: namespace,
        }
    }

    /// Scope nested inside a symbol identified by `id` + `qname`.
    pub fn nested(id: &'a str, qname: &'a str) -> Self {
        Self {
            id: Some(id),
            qname: Some(qname),
        }
    }
}

pub use cartog_core::detect_language;

/// Get the extractor for a language name.
pub fn get_extractor(language: &str) -> Option<Box<dyn Extractor>> {
    match language {
        "python" => Some(Box::new(python::PythonExtractor::new())),
        "typescript" => Some(Box::new(typescript::TypeScriptExtractor::new())),
        "tsx" => Some(Box::new(typescript::TsxExtractor::new())),
        "javascript" => Some(Box::new(javascript::JavaScriptExtractor::new())),
        "rust" => Some(Box::new(rust_lang::RustExtractor::new())),
        "go" => Some(Box::new(go::GoExtractor::new())),
        "ruby" => Some(Box::new(ruby::RubyExtractor::new())),
        "java" => Some(Box::new(java::JavaExtractor::new())),
        "php" => Some(Box::new(php::PhpExtractor::new())),
        "dart" => Some(Box::new(dart::DartExtractor::new())),
        "swift" => Some(Box::new(swift::SwiftExtractor::new())),
        "kotlin" => Some(Box::new(kotlin::KotlinExtractor::new())),
        "markdown" => Some(Box::new(markdown::MarkdownExtractor::new())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_extractor() {
        assert!(get_extractor("python").is_some());
        assert!(get_extractor("typescript").is_some());
        assert!(get_extractor("tsx").is_some());
        assert!(get_extractor("javascript").is_some());
        assert!(get_extractor("rust").is_some());
        assert!(get_extractor("go").is_some());
        assert!(get_extractor("ruby").is_some());
        assert!(get_extractor("java").is_some());
        assert!(get_extractor("php").is_some());
        assert!(get_extractor("dart").is_some());
        assert!(get_extractor("swift").is_some());
        assert!(get_extractor("kotlin").is_some());
        assert!(get_extractor("markdown").is_some());
        assert!(get_extractor("unknown").is_none());
    }

    const ALL_LANGS: [&str; 13] = [
        "python",
        "typescript",
        "tsx",
        "javascript",
        "rust",
        "go",
        "ruby",
        "java",
        "php",
        "dart",
        "swift",
        "kotlin",
        "markdown",
    ];

    proptest::proptest! {
        /// No extractor panics on arbitrary source — indexing whole repos must
        /// degrade (return Ok/Err), never abort the run. Covers unicode, control
        /// chars, and unbalanced delimiters via the regex generator.
        #[test]
        fn extractors_never_panic_on_arbitrary_source(src in ".{0,400}") {
            for lang in ALL_LANGS {
                let mut ex = get_extractor(lang).unwrap();
                let _ = ex.extract(&src, &format!("fuzz.{lang}"));
            }
        }
    }
}
