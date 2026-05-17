//! Tree-sitter language extractors for the cartog code graph.
//!
//! Parses source code using tree-sitter grammars and extracts symbols and edges.
//! Each language implements the [`Extractor`] trait with compiled S-expression
//! queries for declarative AST pattern matching.
//!
//! Supported languages: Python, TypeScript, TSX, JavaScript, Rust, Go, Ruby, Java, PHP.

pub mod go;
pub mod java;
pub mod javascript;
mod js_shared;
pub mod markdown;
pub mod php;
pub mod python;
pub(crate) mod queries;
pub mod ruby;
pub mod rust_lang;
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
        assert!(get_extractor("markdown").is_some());
        assert!(get_extractor("unknown").is_none());
    }
}
