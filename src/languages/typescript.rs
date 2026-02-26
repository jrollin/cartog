use anyhow::Result;
use tree_sitter::{Language, Parser};

use super::{js_shared, ExtractionResult, Extractor};

pub struct TypeScriptExtractor {
    parser: Parser,
}

impl TypeScriptExtractor {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT))
            .expect("TypeScript grammar should always load");
        Self { parser }
    }
}

impl Default for TypeScriptExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for TypeScriptExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        js_shared::extract(&mut self.parser, source, file_path)
    }
}

pub struct TsxExtractor {
    parser: Parser,
}

impl TsxExtractor {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_typescript::LANGUAGE_TSX))
            .expect("TSX grammar should always load");
        Self { parser }
    }
}

impl Default for TsxExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for TsxExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        js_shared::extract(&mut self.parser, source, file_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EdgeKind, SymbolKind, Visibility};

    fn extract_ts(source: &str) -> ExtractionResult {
        let mut ext = TypeScriptExtractor::new();
        ext.extract(source, "test.ts").unwrap()
    }

    #[test]
    fn test_function_declaration() {
        let result = extract_ts(
            r#"
function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert_eq!(result.symbols[0].kind, SymbolKind::Function);
        assert_eq!(
            result.symbols[0].signature.as_deref(),
            Some("(name: string): string")
        );
    }

    #[test]
    fn test_arrow_function() {
        let result = extract_ts(
            r#"
const add = (a: number, b: number): number => a + b;
"#,
        );
        let func = result.symbols.iter().find(|s| s.name == "add");
        assert!(func.is_some());
        assert_eq!(func.unwrap().kind, SymbolKind::Function);
    }

    #[test]
    fn test_class_with_methods() {
        let result = extract_ts(
            r#"
class UserService {
    private db: Database;

    constructor(db: Database) {
        this.db = db;
    }

    async getUser(id: number): Promise<User> {
        return this.db.find(id);
    }

    protected _validate(user: User): boolean {
        return user.isActive;
    }
}
"#,
        );

        let class = result.symbols.iter().find(|s| s.name == "UserService");
        assert!(class.is_some());
        assert_eq!(class.unwrap().kind, SymbolKind::Class);

        let get_user = result.symbols.iter().find(|s| s.name == "getUser");
        assert!(get_user.is_some());
        assert_eq!(get_user.unwrap().kind, SymbolKind::Method);
        assert!(get_user.unwrap().is_async);

        let validate = result.symbols.iter().find(|s| s.name == "_validate");
        assert!(validate.is_some());
        assert_eq!(validate.unwrap().visibility, Visibility::Protected);

        let db_field = result.symbols.iter().find(|s| s.name == "db");
        assert!(db_field.is_some());
        assert_eq!(db_field.unwrap().visibility, Visibility::Private);
    }

    #[test]
    fn test_inheritance_and_implements() {
        let result = extract_ts(
            r#"
class AdminService extends UserService implements Loggable {
    impersonate(userId: number): void {}
}
"#,
        );

        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert_eq!(inherits.len(), 2);

        let targets: Vec<&str> = inherits.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"UserService"));
        assert!(targets.contains(&"Loggable"));
    }

    #[test]
    fn test_interface_and_extends() {
        let result = extract_ts(
            r#"
interface Serializable extends Readable {
    serialize(): string;
}
"#,
        );

        let iface = result.symbols.iter().find(|s| s.name == "Serializable");
        assert!(iface.is_some());

        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].target_name, "Readable");
    }

    #[test]
    fn test_imports() {
        let result = extract_ts(
            r#"
import { Router, Request, Response } from 'express';
import * as path from 'path';
import fs from 'fs';
"#,
        );

        let imports: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 3);

        let import_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();
        assert!(import_edges.len() >= 3); // Router, Request, Response, path, fs
    }

    #[test]
    fn test_function_calls() {
        let result = extract_ts(
            r#"
function process() {
    const data = fetchData();
    const result = transform(data);
    save(result);
}
"#,
        );

        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert_eq!(calls.len(), 3);

        let targets: Vec<&str> = calls.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"fetchData"));
        assert!(targets.contains(&"transform"));
        assert!(targets.contains(&"save"));
    }

    #[test]
    fn test_throws() {
        let result = extract_ts(
            r#"
function validate(x: number): void {
    if (x < 0) {
        throw new RangeError("negative");
    }
    if (x > 100) {
        throw new TypeError("too large");
    }
}
"#,
        );

        let raises: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Raises)
            .collect();
        assert_eq!(raises.len(), 2);

        let targets: Vec<&str> = raises.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"RangeError"));
        assert!(targets.contains(&"TypeError"));
    }

    #[test]
    fn test_enum() {
        let result = extract_ts(
            r#"
enum Status {
    Active,
    Inactive,
    Pending,
}
"#,
        );

        let e = result.symbols.iter().find(|s| s.name == "Status");
        assert!(e.is_some());
        assert_eq!(e.unwrap().kind, SymbolKind::Class);
    }

    #[test]
    fn test_type_alias() {
        let result = extract_ts(
            r#"
type UserId = string;
"#,
        );

        let t = result.symbols.iter().find(|s| s.name == "UserId");
        assert!(t.is_some());
        assert_eq!(t.unwrap().kind, SymbolKind::Variable);
    }

    #[test]
    fn test_type_annotation_refs() {
        let result = extract_ts(
            r#"
function process(user: User, count: number): Response {
    return new Response();
}
"#,
        );

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();

        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        // User and Response are uppercase → captured as references
        assert!(targets.contains(&"User"));
        assert!(targets.contains(&"Response"));
        // number is lowercase → not captured
        assert!(!targets.contains(&"number"));
    }

    #[test]
    fn test_new_expression_as_call() {
        let result = extract_ts(
            r#"
function create() {
    const user = new UserService();
    const map = new Map();
}
"#,
        );

        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();

        let targets: Vec<&str> = calls.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"UserService"));
        assert!(targets.contains(&"Map"));
    }

    #[test]
    fn test_generic_type_annotation_refs() {
        let result = extract_ts(
            r#"
async function getUser(id: number): Promise<User> {
    return {} as User;
}
"#,
        );

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();

        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"Promise"));
        assert!(targets.contains(&"User"));
    }
}
