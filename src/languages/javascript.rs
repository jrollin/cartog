use anyhow::Result;
use tree_sitter::Language;

use super::{js_shared, ExtractionResult, Extractor};

pub struct JavaScriptExtractor {
    language: Language,
}

impl JavaScriptExtractor {
    pub fn new() -> Self {
        Self {
            language: Language::new(tree_sitter_javascript::LANGUAGE),
        }
    }
}

impl Default for JavaScriptExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for JavaScriptExtractor {
    fn extract(&self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        js_shared::extract(&self.language, source, file_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EdgeKind, SymbolKind, Visibility};

    fn extract_js(source: &str) -> ExtractionResult {
        let ext = JavaScriptExtractor::new();
        ext.extract(source, "test.js").unwrap()
    }

    #[test]
    fn test_function_declaration() {
        let result = extract_js(
            r#"
function greet(name) {
    return `Hello, ${name}!`;
}
"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert_eq!(result.symbols[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_arrow_function() {
        let result = extract_js(
            r#"
const add = (a, b) => a + b;
"#,
        );
        let func = result.symbols.iter().find(|s| s.name == "add");
        assert!(func.is_some());
        assert_eq!(func.unwrap().kind, SymbolKind::Function);
    }

    #[test]
    fn test_class_with_inheritance() {
        let result = extract_js(
            r#"
class Animal {
    constructor(name) {
        this.name = name;
    }

    speak() {
        return `${this.name} makes a sound`;
    }
}

class Dog extends Animal {
    speak() {
        return `${this.name} barks`;
    }
}
"#,
        );

        let classes: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(classes.len(), 2);

        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].target_name, "Animal");
    }

    #[test]
    fn test_imports_require() {
        let result = extract_js(
            r#"
import { readFile, writeFile } from 'fs';
import path from 'path';
"#,
        );

        let imports: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn test_function_calls() {
        let result = extract_js(
            r#"
function process() {
    const data = fetchData();
    transform(data);
}
"#,
        );

        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn test_private_field_convention() {
        let result = extract_js(
            r#"
class Cache {
    #store;

    constructor() {
        this.#store = new Map();
    }

    _internalMethod() {
        return this.#store;
    }
}
"#,
        );

        let store = result.symbols.iter().find(|s| s.name == "#store");
        assert!(store.is_some());
        assert_eq!(store.unwrap().visibility, Visibility::Private);

        let internal = result.symbols.iter().find(|s| s.name == "_internalMethod");
        assert!(internal.is_some());
        assert_eq!(internal.unwrap().visibility, Visibility::Protected);
    }
}
