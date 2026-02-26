pub mod go;
pub mod javascript;
mod js_shared;
pub mod python;
pub mod ruby;
pub mod rust_lang;
pub mod typescript;

use crate::types::{Edge, Symbol};
use anyhow::Result;
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

/// Map file extension to language name.
pub fn detect_language(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "py" | "pyi" => Some("python"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "rs" => Some("rust"),
        "go" => Some("go"),
        "rb" => Some("ruby"),
        _ => None,
    }
}

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
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        use std::path::Path;
        assert_eq!(detect_language(Path::new("src/main.py")), Some("python"));
        assert_eq!(detect_language(Path::new("lib.pyi")), Some("python"));
        assert_eq!(detect_language(Path::new("app.ts")), Some("typescript"));
        assert_eq!(detect_language(Path::new("App.tsx")), Some("tsx"));
        assert_eq!(detect_language(Path::new("index.js")), Some("javascript"));
        assert_eq!(detect_language(Path::new("util.mjs")), Some("javascript"));
        assert_eq!(detect_language(Path::new("main.rs")), Some("rust"));
        assert_eq!(detect_language(Path::new("server.go")), Some("go"));
        assert_eq!(detect_language(Path::new("app.rb")), Some("ruby"));
        assert_eq!(detect_language(Path::new("README.md")), None);
        assert_eq!(detect_language(Path::new("Makefile")), None);
        assert_eq!(detect_language(Path::new("Main.java")), None); // java not supported yet
    }

    #[test]
    fn test_get_extractor() {
        assert!(get_extractor("python").is_some());
        assert!(get_extractor("typescript").is_some());
        assert!(get_extractor("tsx").is_some());
        assert!(get_extractor("javascript").is_some());
        assert!(get_extractor("rust").is_some());
        assert!(get_extractor("go").is_some());
        assert!(get_extractor("ruby").is_some());
        assert!(get_extractor("java").is_none());
        assert!(get_extractor("unknown").is_none());
    }
}
