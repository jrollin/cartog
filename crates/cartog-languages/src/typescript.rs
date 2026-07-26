use anyhow::Result;
use tree_sitter::{Language, Parser};

use super::{js_shared, ExtractionResult, Extractor};

pub struct TypeScriptExtractor {
    parser: Parser,
    queries: js_shared::JsQueries,
}

impl TypeScriptExtractor {
    pub fn new() -> Self {
        let lang = Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT);
        let mut parser = Parser::new();
        parser
            .set_language(&lang)
            .expect("TypeScript grammar should always load");
        let queries = js_shared::JsQueries::new(&lang);
        Self { parser, queries }
    }
}

impl Default for TypeScriptExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for TypeScriptExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        js_shared::extract(&mut self.parser, &self.queries, source, file_path)
    }
}

pub struct TsxExtractor {
    parser: Parser,
    queries: js_shared::JsQueries,
}

impl TsxExtractor {
    pub fn new() -> Self {
        let lang = Language::new(tree_sitter_typescript::LANGUAGE_TSX);
        let mut parser = Parser::new();
        parser
            .set_language(&lang)
            .expect("TSX grammar should always load");
        let queries = js_shared::JsQueries::new(&lang);
        Self { parser, queries }
    }
}

impl Default for TsxExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for TsxExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        js_shared::extract(&mut self.parser, &self.queries, source, file_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartog_core::{EdgeKind, SymbolKind, Visibility};

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
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].target_name, "UserService");

        let implements: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(implements.len(), 1);
        assert_eq!(implements[0].target_name, "Loggable");
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
        assert_eq!(iface.unwrap().kind, SymbolKind::Interface);

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
        assert_eq!(e.unwrap().kind, SymbolKind::Enum);
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
        assert_eq!(t.unwrap().kind, SymbolKind::TypeAlias);
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

    #[test]
    fn test_abstract_class() {
        let result = extract_ts(
            r#"
export abstract class BaseCache implements CacheBackend {
    protected name: string;

    constructor(name: string) {
        this.name = name;
    }

    abstract get(key: string): unknown | null;

    stats(): { hits: number } {
        return { hits: 0 };
    }
}
"#,
        );

        let class = result.symbols.iter().find(|s| s.name == "BaseCache");
        assert!(class.is_some(), "abstract class should be extracted");
        assert_eq!(class.unwrap().kind, SymbolKind::Class);

        let stats_method = result.symbols.iter().find(|s| s.name == "stats");
        assert!(
            stats_method.is_some(),
            "method inside abstract class should be extracted"
        );

        let implements: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(implements.len(), 1);
        assert_eq!(implements[0].target_name, "CacheBackend");
    }

    #[test]
    fn test_abstract_class_nested_in_export() {
        let result = extract_ts(
            r#"
export abstract class Service {
    abstract run(): void;
}

export class ConcreteService extends Service {
    run(): void {}
}
"#,
        );

        let abs = result.symbols.iter().find(|s| s.name == "Service");
        assert!(abs.is_some());

        let concrete = result.symbols.iter().find(|s| s.name == "ConcreteService");
        assert!(concrete.is_some());

        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].target_name, "Service");
    }

    #[test]
    fn test_renamed_import() {
        let result = extract_ts(
            r#"
import { validate as validateUser } from '../validators/user';
"#,
        );

        let import_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();

        let targets: Vec<&str> = import_edges
            .iter()
            .map(|e| e.target_name.as_str())
            .collect();
        // Import edge uses the original exported name for cross-file resolution.
        // Known limitation: calls using the alias (validateUser) won't resolve
        // to the original symbol without alias tracking in the edge model.
        assert!(
            targets.contains(&"validate"),
            "should have original name for resolution: got {:?}",
            targets
        );
    }

    fn extract_tsx(source: &str) -> ExtractionResult {
        let mut ext = TsxExtractor::new();
        ext.extract(source, "App.tsx").unwrap()
    }

    /// Collect `Calls`-edge target names emitted from `from`'s scope.
    fn calls_from<'a>(result: &'a ExtractionResult, from: &str) -> Vec<&'a str> {
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == from)
            .unwrap_or_else(|| panic!("symbol {from} not found"));
        result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.source_id == sym.id)
            .map(|e| e.target_name.as_str())
            .collect()
    }

    #[test]
    fn jsx_component_usage_emits_calls_edge() {
        let result = extract_tsx("function App() { return <Counter initial={0} />; }\n");
        assert!(
            calls_from(&result, "App").contains(&"Counter"),
            "App should have a Calls edge to Counter"
        );
    }

    #[test]
    fn jsx_lowercase_intrinsic_is_skipped() {
        let result = extract_tsx("function App() { return <div className=\"x\" />; }\n");
        assert!(
            !calls_from(&result, "App").contains(&"div"),
            "intrinsic HTML element <div> must not emit a component edge"
        );
    }

    #[test]
    fn jsx_member_expression_component_uses_dotted_name() {
        let result = extract_tsx("function App() { return <Foo.Bar />; }\n");
        assert!(
            calls_from(&result, "App").contains(&"Foo.Bar"),
            "member-expression component should keep its dotted name"
        );
    }

    #[test]
    fn jsx_lowercase_dotted_host_tag_is_skipped() {
        // Regression: `name.contains('.')` alone treated lowercase namespaced
        // tags like <svg.path/> as components, emitting unresolvable edges.
        let result = extract_tsx("function App() { return <svg.path d=\"M0 0\" />; }\n");
        assert!(
            !calls_from(&result, "App").contains(&"svg.path"),
            "lowercase namespaced host tag must not emit a component edge"
        );
    }

    #[test]
    fn jsx_self_closing_and_opening_both_captured() {
        let self_closing = extract_tsx("function A() { return <Counter />; }\n");
        let opening = extract_tsx("function A() { return <Counter>hi</Counter>; }\n");
        assert!(calls_from(&self_closing, "A").contains(&"Counter"));
        assert!(calls_from(&opening, "A").contains(&"Counter"));
    }

    // ── Test-DSL blocks (Jest / Vitest / Mocha) ──
    // Before these, a top-level `describe(...)` was an expression statement with no
    // enclosing symbol, so a whole spec file indexed to zero symbols and zero edges.

    fn sym<'a>(r: &'a ExtractionResult, name: &str) -> &'a cartog_core::Symbol {
        r.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name:?}"))
    }

    #[test]
    fn describe_block_becomes_a_test_symbol() {
        let r = extract_ts("describe('AuthService', () => {});\n");
        let d = sym(&r, "AuthService");
        assert_eq!(d.kind, SymbolKind::Function);
        assert!(d.is_test);
        assert_eq!(d.signature.as_deref(), Some("describe('AuthService')"));
    }

    #[test]
    fn nested_it_is_a_child_test_and_never_a_method() {
        let r = extract_ts("describe('Outer', () => { it('inner case', () => {}); });\n");
        let outer = sym(&r, "Outer");
        let inner = sym(&r, "inner case");
        // A nested `it` is a test case, not a method of the describe.
        assert_eq!(inner.kind, SymbolKind::Function);
        assert!(inner.is_test);
        assert_eq!(inner.parent_id.as_deref(), Some(outer.id.as_str()));
    }

    #[test]
    fn calls_inside_a_test_body_are_attributed_to_that_test() {
        let r = extract_ts("describe('S', () => { it('checks', () => { validate('x'); }); });\n");
        assert!(
            calls_from(&r, "checks").contains(&"validate"),
            "a call inside the `it` body must be sourced from the test, got {:?}",
            calls_from(&r, "checks")
        );
    }

    #[test]
    fn modifier_and_each_forms_are_recognized() {
        for src in [
            "it.only('a', () => {});\n",
            "it.skip('a', () => {});\n",
            "describe.each([1])('a', () => {});\n",
        ] {
            let r = extract_ts(src);
            assert!(sym(&r, "a").is_test, "not recognized: {src}");
        }
    }

    #[test]
    fn every_supported_dsl_callee_is_recognized() {
        for callee in ["describe", "context", "it", "test", "suite", "bench"] {
            let r = extract_ts(&format!("{callee}('label', () => {{}});\n"));
            assert!(sym(&r, "label").is_test, "{callee} not recognized");
        }
    }

    #[test]
    fn async_test_body_sets_is_async() {
        let r = extract_ts("it('waits', async () => {});\n");
        assert!(sym(&r, "waits").is_async);
    }

    #[test]
    fn template_literal_label_is_used() {
        let r = extract_ts("it(`renders fast`, () => {});\n");
        assert!(sym(&r, "renders fast").is_test);
    }

    #[test]
    fn non_test_call_emits_no_symbol_and_stays_an_edge_scan() {
        let r = extract_ts("configure('AuthService', () => {});\n");
        assert!(
            !r.symbols.iter().any(|s| s.name == "AuthService"),
            "a non-DSL callee must not become a symbol"
        );
    }

    #[test]
    fn dsl_call_without_a_string_label_emits_no_symbol() {
        let r = extract_ts("describe(someVar, () => {});\n");
        assert!(r.symbols.iter().all(|s| !s.is_test));
    }

    #[test]
    fn ordinary_declarations_are_not_flagged_as_tests() {
        let r = extract_ts("export function validate(x: string) { return !!x; }\n");
        let f = sym(&r, "validate");
        assert_eq!(f.kind, SymbolKind::Function);
        assert!(!f.is_test, "a plain function must not be flagged is_test");
    }
}
