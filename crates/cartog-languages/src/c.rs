use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use cartog_core::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{last_segment, node_text, qualified, ExtractionResult, Extractor, ParentScope};

pub struct CExtractor {
    parser: Parser,
}

impl CExtractor {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_c::LANGUAGE))
            .expect("C grammar should always load");
        Self { parser }
    }
}

impl Default for CExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for CExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        let tree = crate::parse_bounded(&mut self.parser, source)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {file_path}"))?;

        let mut symbols = Vec::new();
        let mut edges = Vec::new();

        extract_node(
            tree.root_node(),
            source,
            file_path,
            ParentScope::default(),
            &mut symbols,
            &mut edges,
        );

        Ok(ExtractionResult { symbols, edges })
    }
}

fn extract_node(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    match node.kind() {
        "function_definition" => {
            extract_function(node, source, file_path, scope, symbols, edges);
        }
        "struct_specifier" | "union_specifier" => {
            extract_record(node, source, file_path, scope, symbols, edges);
        }
        "enum_specifier" => extract_enum(node, source, file_path, scope, symbols),
        "type_definition" => extract_typedef(node, source, file_path, scope, symbols, edges),
        "preproc_include" => extract_include(node, source, file_path, scope, symbols, edges),
        // A bodiless prototype (`declaration` + `function_declarator`) emits no
        // symbol (D2): one symbol per function keeps the resolver's UniqueGlobal
        // tier working, since its `disambiguate_two` returns None for two
        // same-kind candidates (header prototype + implementation).
        //
        // Do NOT descend either: a prototype's children are only its return and
        // parameter type specifiers, and `struct C *c` there is a type USE, not a
        // definition. Walking in would emit a phantom `class C` from every
        // forward-declared or pointed-to type in a header.
        "declaration" | "parameter_declaration" | "field_declaration" => {}
        _ => {
            for child in node.named_children(&mut node.walk()) {
                extract_node(child, source, file_path, scope, symbols, edges);
            }
        }
    }
}

// ── Functions ──

fn extract_function(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(declarator) = find_function_declarator(node) else {
        return;
    };
    let Some(name_node) = declarator.child_by_field_name("declarator") else {
        return;
    };
    let name = innermost_declarator_name(name_node, source);
    if name.is_empty() {
        return;
    }

    let start_line = node.start_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, SymbolKind::Function, &name, scope.qname);

    let mut sym = Symbol::new(
        name,
        SymbolKind::Function,
        file_path,
        start_line,
        node.end_position().row as u32 + 1,
        node.start_byte() as u32,
        node.end_byte() as u32,
        scope.qname,
    )
    .with_parent(scope.id)
    .with_signature(signature_of(node, declarator, source))
    .with_docstring(extract_doc_comment(node, source));
    // `static` in C means file-local linkage.
    if is_static(node, source) {
        sym = sym.with_visibility(Visibility::Private);
    }
    symbols.push(sym);

    collect_param_and_return_refs(node, declarator, source, file_path, &sym_id, edges);
    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, &sym_id, edges);
    }
}

// Identical to `find_function_declarator` in cpp.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// The `function_declarator` under a declarator, descending through pointer
/// wrappers (`char* f()`).
fn find_function_declarator(node: Node) -> Option<Node> {
    let mut cur = node.child_by_field_name("declarator")?;
    for _ in 0..crate::MAX_TREE_DEPTH {
        if cur.kind() == "function_declarator" {
            return Some(cur);
        }
        cur = cur.child_by_field_name("declarator")?;
    }
    None
}

// Identical to `signature_of` in cpp.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
fn signature_of(node: Node, declarator: Node, source: &str) -> Option<String> {
    let params = declarator
        .child_by_field_name("parameters")
        .map(|p| node_text(p, source))
        .unwrap_or("()");
    let ret = node
        .child_by_field_name("type")
        .map(|t| node_text(t, source));
    Some(match ret {
        Some(r) => format!("{r} {params}"),
        None => params.to_string(),
    })
}

fn is_static(node: Node, source: &str) -> bool {
    node.named_children(&mut node.walk())
        .any(|c| c.kind() == "storage_class_specifier" && node_text(c, source) == "static")
}

// ── Structs and unions (→ Class; C has no classes) ──

fn extract_record(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // A bare `struct Foo x;` type reference (no body) or an anonymous struct
    // carries nothing to key a symbol on.
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if node.child_by_field_name("body").is_none() {
        return;
    }
    let name = node_text(name_node, source).to_string();
    if name.is_empty() {
        return;
    }

    let start_line = node.start_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, SymbolKind::Class, &name, scope.qname);
    let record_qname = qualified(scope.qname, &name);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Class,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            scope.qname,
        )
        .with_parent(scope.id)
        .with_docstring(extract_doc_comment(node, source)),
    );

    if let Some(body) = node.child_by_field_name("body") {
        extract_fields(
            body,
            source,
            file_path,
            &sym_id,
            &record_qname,
            symbols,
            edges,
        );
    }
}

/// Struct/union members → `Variable` symbols. A function-pointer member (the C
/// idiom for a vtable) is still a data field, so it maps to Variable too.
fn extract_fields(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    parent_qname: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "field_declaration" => {
                // A nested struct/union arrives wrapped as this declaration's
                // `type`. Anonymous: its members belong to the parent record.
                // Named: it is its own record symbol.
                if let Some(nested) = nested_record(child) {
                    match nested.child_by_field_name("name") {
                        Some(_) => extract_record(
                            nested,
                            source,
                            file_path,
                            ParentScope::nested(parent_id, parent_qname),
                            symbols,
                            edges,
                        ),
                        None => {
                            if let Some(body) = nested.child_by_field_name("body") {
                                extract_fields(
                                    body,
                                    source,
                                    file_path,
                                    parent_id,
                                    parent_qname,
                                    symbols,
                                    edges,
                                );
                            }
                        }
                    }
                    continue;
                }
                let Some(decl) = child.child_by_field_name("declarator") else {
                    continue;
                };
                let name = innermost_declarator_name(decl, source);
                if name.is_empty() {
                    continue;
                }
                let line = child.start_position().row as u32 + 1;
                symbols.push(
                    Symbol::new(
                        name,
                        SymbolKind::Variable,
                        file_path,
                        line,
                        child.end_position().row as u32 + 1,
                        child.start_byte() as u32,
                        child.end_byte() as u32,
                        Some(parent_qname),
                    )
                    .with_parent(Some(parent_id)),
                );
                if let Some(ty) = child.child_by_field_name("type") {
                    emit_type_ref(ty, source, file_path, parent_id, line, edges);
                }
            }
            // A nested anonymous struct/union's members belong to the parent.
            "struct_specifier" | "union_specifier" => {
                if let Some(body) = child.child_by_field_name("body") {
                    extract_fields(
                        body,
                        source,
                        file_path,
                        parent_id,
                        parent_qname,
                        symbols,
                        edges,
                    );
                }
            }
            _ => {}
        }
    }
}

/// A struct/union defined inside a record body arrives wrapped in a
/// `field_declaration` whose `type` is the specifier. Returns it only when it
/// actually defines a record (has a body), so `struct Foo f;` stays a data field.
fn nested_record(node: Node) -> Option<Node> {
    let ty = node.child_by_field_name("type")?;
    let defines_record = matches!(ty.kind(), "struct_specifier" | "union_specifier")
        && ty.child_by_field_name("body").is_some();
    defines_record.then_some(ty)
}

// Identical to `innermost_declarator_name` in cpp.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// Bare name of a possibly-wrapped declarator (`*p`, `a[3]`, `(*fp)(int)`).
fn innermost_declarator_name(node: Node, source: &str) -> String {
    crate::parse::guard_recursion!(String::new());
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => {
            node_text(node, source).to_string()
        }
        _ => match node.child_by_field_name("declarator") {
            Some(inner) => innermost_declarator_name(inner, source),
            None => node
                .named_children(&mut node.walk())
                .map(|c| innermost_declarator_name(c, source))
                .find(|s| !s.is_empty())
                .unwrap_or_default(),
        },
    }
}

// ── Enums ──

fn extract_enum(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = node_text(name_node, source).to_string();
    if name.is_empty() {
        return;
    }

    let start_line = node.start_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, SymbolKind::Enum, &name, scope.qname);
    let enum_qname = qualified(scope.qname, &name);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Enum,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            scope.qname,
        )
        .with_parent(scope.id)
        .with_docstring(extract_doc_comment(node, source)),
    );

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    for member in body.named_children(&mut body.walk()) {
        if member.kind() != "enumerator" {
            continue;
        }
        let Some(mn) = member.child_by_field_name("name") else {
            continue;
        };
        let m_line = member.start_position().row as u32 + 1;
        symbols.push(
            Symbol::new(
                node_text(mn, source),
                SymbolKind::Variable,
                file_path,
                m_line,
                m_line,
                member.start_byte() as u32,
                member.end_byte() as u32,
                Some(&enum_qname),
            )
            .with_parent(Some(&sym_id)),
        );
    }
}

// ── Typedefs ──

/// `typedef int MyInt;` → a TypeAlias. `typedef struct { .. } Name;` also
/// defines the record, so recurse into the type to pick that up as well.
fn extract_typedef(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    if let Some(ty) = node.child_by_field_name("type") {
        if matches!(
            ty.kind(),
            "struct_specifier" | "union_specifier" | "enum_specifier"
        ) && ty.child_by_field_name("body").is_some()
        {
            extract_node(ty, source, file_path, scope, symbols, edges);
        }
    }

    let Some(decl) = node.child_by_field_name("declarator") else {
        return;
    };
    let name = innermost_declarator_name(decl, source);
    if name.is_empty() {
        return;
    }

    let line = node.start_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, SymbolKind::TypeAlias, &name, scope.qname);
    symbols.push(
        Symbol::new(
            name,
            SymbolKind::TypeAlias,
            file_path,
            line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            scope.qname,
        )
        .with_parent(scope.id)
        .with_docstring(extract_doc_comment(node, source)),
    );
    if let Some(ty) = node.child_by_field_name("type") {
        emit_type_ref(ty, source, file_path, &sym_id, line, edges);
    }
}

// ── Includes ──

// Identical to `extract_include` in cpp.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// `#include "local.h"` → an Import symbol + Imports edge. A `<system>` include
/// is stdlib noise, so it is skipped.
fn extract_include(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(path_node) = node.child_by_field_name("path") else {
        return;
    };
    if path_node.kind() == "system_lib_string" {
        return;
    }
    let raw = node_text(path_node, source);
    let path = raw.trim_matches(|c| c == '"' || c == '<' || c == '>');
    if path.is_empty() {
        return;
    }

    let line = node.start_position().row as u32 + 1;
    // Target the header's basename: the resolver matches edges by symbol name,
    // and an include path is relative to the includer, not the repo root.
    let target = last_segment(path, "/");
    // Derive the edge source from symbol_id, never a hand-built string: it
    // escapes `.` inside a segment, so a literal format! would not match the
    // symbol row and the edge's foreign key would fail.
    let sym_id = symbol_id(file_path, SymbolKind::Import, path, scope.qname);

    symbols.push(Symbol::new(
        path,
        SymbolKind::Import,
        file_path,
        line,
        line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        scope.qname,
    ));
    edges.push(Edge::new(
        sym_id,
        target,
        EdgeKind::Imports,
        file_path,
        line,
    ));
}

// ── Calls ──

// Identical to `walk_for_calls` in cpp.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
fn walk_for_calls(node: Node, source: &str, file_path: &str, sym_id: &str, edges: &mut Vec<Edge>) {
    crate::parse::guard_recursion!();
    if node.kind() == "call_expression" {
        if let Some(f) = node.child_by_field_name("function") {
            if let Some(target) = call_target_name(f, source) {
                edges.push(Edge::new(
                    sym_id,
                    target,
                    EdgeKind::Calls,
                    file_path,
                    node.start_position().row as u32 + 1,
                ));
            }
        }
    }
    for child in node.named_children(&mut node.walk()) {
        walk_for_calls(child, source, file_path, sym_id, edges);
    }
}

/// Target name for a call: bare `f()` → `f`; `s.fp()` / `p->fp()` → `s.fp`
/// (the C function-pointer / vtable idiom).
fn call_target_name(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node_text(node, source).to_string()),
        "field_expression" => {
            let field = node.child_by_field_name("field")?;
            let recv = node
                .child_by_field_name("argument")
                .map(|a| receiver_name(a, source))
                .unwrap_or_default();
            let field = node_text(field, source);
            Some(if recv.is_empty() {
                field.to_string()
            } else {
                format!("{recv}.{field}")
            })
        }
        // `(*fp)(x)` — the dereferenced function-pointer call idiom.
        "parenthesized_expression" | "pointer_expression" => node
            .named_child(0)
            .and_then(|c| call_target_name(c, source)),
        _ => None,
    }
}

fn receiver_name(node: Node, source: &str) -> String {
    match node.kind() {
        "identifier" | "field_identifier" => node_text(node, source).to_string(),
        _ => String::new(),
    }
}

// ── Type references ──

// Identical to `collect_param_and_return_refs` in cpp.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
fn collect_param_and_return_refs(
    node: Node,
    declarator: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    if let Some(ty) = node.child_by_field_name("type") {
        emit_type_ref(ty, source, file_path, sym_id, line, edges);
    }
    let Some(params) = declarator.child_by_field_name("parameters") else {
        return;
    };
    for p in params.named_children(&mut params.walk()) {
        if let Some(ty) = p.child_by_field_name("type") {
            emit_type_ref(ty, source, file_path, sym_id, line, edges);
        }
    }
}

// Identical to `emit_type_ref` in cpp.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
fn emit_type_ref(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    let name = simple_type_name(node, source);
    if !name.is_empty() && !is_builtin_type(&name) {
        edges.push(Edge::new(
            sym_id,
            name,
            EdgeKind::References,
            file_path,
            line,
        ));
    }
}

/// Simple type name, unwrapping `struct Foo` / `union U` / `enum E` tags and
/// pointer/const nesting.
fn simple_type_name(node: Node, source: &str) -> String {
    crate::parse::guard_recursion!(String::new());
    match node.kind() {
        "type_identifier" | "identifier" => node_text(node, source).to_string(),
        "primitive_type" | "sized_type_specifier" => String::new(),
        "struct_specifier" | "union_specifier" | "enum_specifier" => node
            .child_by_field_name("name")
            .map(|n| simple_type_name(n, source))
            .unwrap_or_default(),
        _ => node
            .named_children(&mut node.walk())
            .map(|c| simple_type_name(c, source))
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
    }
}

/// Keyword/stdlib type names that carry no graph value as an edge target.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "void"
            | "bool"
            | "_Bool"
            | "char"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "signed"
            | "unsigned"
            | "size_t"
            | "ssize_t"
            | "ptrdiff_t"
            | "intptr_t"
            | "uintptr_t"
            | "int8_t"
            | "int16_t"
            | "int32_t"
            | "int64_t"
            | "uint8_t"
            | "uint16_t"
            | "uint32_t"
            | "uint64_t"
            | "FILE"
    )
}

// ── Doc comments ──

/// Preceding `///` or `/** */` comment block, nearest-first then reversed.
fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut p = node.prev_sibling();
    while let Some(n) = p {
        if n.kind() != "comment" {
            break;
        }
        let text = node_text(n, source).trim();
        if let Some(stripped) = text.strip_prefix("///") {
            let clean = stripped.trim();
            if !clean.is_empty() {
                lines.push(clean.to_string());
            }
        } else if text.starts_with("/**") {
            for raw in text
                .trim_start_matches("/**")
                .trim_end_matches("*/")
                .lines()
            {
                let clean = raw.trim().trim_start_matches('*').trim();
                if !clean.is_empty() {
                    lines.push(clean.to_string());
                }
            }
        } else {
            // A plain `//` comment is not documentation; keep scanning past it
            // rather than truncating a `///` block above it.
            p = n.prev_sibling();
            continue;
        }
        p = n.prev_sibling();
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> ExtractionResult {
        let mut ext = CExtractor::new();
        ext.extract(source, "test.c").unwrap()
    }

    fn edge_targets(result: &ExtractionResult, kind: EdgeKind) -> Vec<String> {
        result
            .edges
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.target_name.clone())
            .collect()
    }

    #[test]
    fn test_empty_file() {
        let result = extract("");
        assert!(result.symbols.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_syntax_error_partial_parse() {
        let result = extract("int broken( {");
        let _ = result.symbols.len();
    }

    #[test]
    fn test_function_definition() {
        let result = extract("int add(int a, int b) { return a + b; }");
        let sym = result.symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(sym.kind, SymbolKind::Function);
        assert_eq!(sym.visibility, Visibility::Public);
    }

    #[test]
    fn test_static_function_is_private() {
        let result = extract("static int helper(void) { return 0; }");
        let sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(sym.visibility, Visibility::Private);
    }

    #[test]
    fn test_pointer_returning_function() {
        let result = extract("char* get_name(void) { return 0; }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "get_name")
            .unwrap();
        assert_eq!(sym.kind, SymbolKind::Function);
    }

    // D2: a bodiless prototype must NOT emit a symbol, or the resolver's
    // UniqueGlobal tier goes ambiguous between the header and the impl.
    #[test]
    fn test_prototype_emits_no_symbol() {
        let result = extract("int add(int a, int b);");
        assert!(!result.symbols.iter().any(|s| s.name == "add"));
    }

    #[test]
    fn test_definition_emits_exactly_one_symbol() {
        let result = extract("int add(int a, int b);\nint add(int a, int b) { return a + b; }");
        let defs: Vec<_> = result.symbols.iter().filter(|s| s.name == "add").collect();
        assert_eq!(defs.len(), 1, "exactly one symbol per function");
    }

    #[test]
    fn test_struct_maps_to_class_with_fields() {
        let result = extract("struct Point { int x; int y; };");
        let sym = result.symbols.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(sym.kind, SymbolKind::Class);
        let field = result.symbols.iter().find(|s| s.name == "x").unwrap();
        assert_eq!(field.kind, SymbolKind::Variable);
        assert!(field.id.ends_with("Point.x"));
    }

    #[test]
    fn test_union_maps_to_class() {
        let result = extract("union Value { int i; float f; };");
        let sym = result.symbols.iter().find(|s| s.name == "Value").unwrap();
        assert_eq!(sym.kind, SymbolKind::Class);
    }

    // Regression: a prototype's return/parameter type specifiers are type USES,
    // not definitions. Descending into `declaration` emitted a phantom
    // `class Ret`/`class Param` for every forward-declared type in a header.
    #[test]
    fn test_prototype_type_specifiers_emit_no_struct_symbols() {
        let result = extract("struct Ret make(struct Param *p);");
        assert!(!result.symbols.iter().any(|s| s.name == "Ret"));
        assert!(!result.symbols.iter().any(|s| s.name == "Param"));
    }

    #[test]
    fn test_pointer_field_of_undefined_struct_emits_no_symbol() {
        let result = extract("struct Holder { struct Other *o; };");
        assert!(result.symbols.iter().any(|s| s.name == "Holder"));
        assert!(!result.symbols.iter().any(|s| s.name == "Other"));
    }

    #[test]
    fn test_bodiless_struct_reference_emits_no_symbol() {
        let result = extract("struct Point p;");
        assert!(!result.symbols.iter().any(|s| s.name == "Point"));
    }

    #[test]
    fn test_enum_with_members() {
        let result = extract("enum TokenError { EXPIRED, INVALID };");
        let e = result
            .symbols
            .iter()
            .find(|s| s.name == "TokenError")
            .unwrap();
        assert_eq!(e.kind, SymbolKind::Enum);
        let m = result.symbols.iter().find(|s| s.name == "EXPIRED").unwrap();
        assert!(m.id.ends_with("TokenError.EXPIRED"));
    }

    #[test]
    fn test_typedef_is_type_alias() {
        let result = extract("typedef int MyInt;");
        let sym = result.symbols.iter().find(|s| s.name == "MyInt").unwrap();
        assert_eq!(sym.kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn test_typedef_struct_emits_record_and_alias() {
        let result = extract("typedef struct Point { int x; } PointT;");
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Point" && s.kind == SymbolKind::Class));
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "PointT" && s.kind == SymbolKind::TypeAlias));
    }

    #[test]
    fn test_function_pointer_typedef() {
        let result = extract("typedef int (*Handler)(int);");
        let sym = result.symbols.iter().find(|s| s.name == "Handler").unwrap();
        assert_eq!(sym.kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn test_bare_call_edge() {
        let result = extract("void f(void) { authenticate(); }");
        assert!(edge_targets(&result, EdgeKind::Calls).contains(&"authenticate".to_string()));
    }

    #[test]
    fn test_member_call_edge_dot() {
        let result = extract("void f(void) { repo.execute_query(); }");
        assert!(edge_targets(&result, EdgeKind::Calls).contains(&"repo.execute_query".to_string()));
    }

    // The C vtable idiom: a function-pointer member called through a pointer.
    #[test]
    fn test_vtable_call_edge_arrow() {
        let result = extract("void f(void) { svc->login(); }");
        assert!(edge_targets(&result, EdgeKind::Calls).contains(&"svc.login".to_string()));
    }

    #[test]
    fn test_function_pointer_member_is_variable() {
        let result = extract("struct Service { int (*login)(struct Service*, const char*); };");
        let sym = result.symbols.iter().find(|s| s.name == "login").unwrap();
        assert_eq!(sym.kind, SymbolKind::Variable);
        assert!(sym.id.ends_with("Service.login"));
    }

    #[test]
    fn test_local_include_emits_import_and_edge() {
        let result = extract("#include \"auth/service.h\"\n");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .unwrap();
        assert_eq!(sym.name, "auth/service.h");
        assert!(edge_targets(&result, EdgeKind::Imports).contains(&"service.h".to_string()));
    }

    // Regression: the Imports edge's source_id must be the Import symbol's real
    // `symbol_id` (which escapes `.` inside a segment), not a hand-built
    // "{file}:import:{path}" string — a mismatch makes the DB insert fail with
    // FOREIGN KEY constraint failed on any file with a local include.
    #[test]
    fn test_include_edge_source_matches_import_symbol_id() {
        let result = extract("#include \"auth/service.h\"\n");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .unwrap();
        let edge = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert_eq!(edge.source_id, sym.id);
    }

    #[test]
    fn test_system_include_is_skipped() {
        let result = extract("#include <stdio.h>\n");
        assert!(!result.symbols.iter().any(|s| s.kind == SymbolKind::Import));
        assert!(edge_targets(&result, EdgeKind::Imports).is_empty());
    }

    #[test]
    fn test_line_doc_comment() {
        let result = extract("/// Validates a token.\nint validate_token(void) { return 1; }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "validate_token")
            .unwrap();
        assert_eq!(sym.docstring.as_deref(), Some("Validates a token."));
    }

    #[test]
    fn test_block_doc_comment() {
        let result = extract("/** Runs a query. */\nint execute_query(void) { return 1; }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "execute_query")
            .unwrap();
        assert_eq!(sym.docstring.as_deref(), Some("Runs a query."));
    }

    #[test]
    fn test_struct_parameter_type_reference_edge() {
        let result = extract("void f(struct DatabaseConnection* conn) { }");
        assert!(
            edge_targets(&result, EdgeKind::References).contains(&"DatabaseConnection".to_string())
        );
    }

    #[test]
    fn test_typedef_parameter_type_reference_edge() {
        let result = extract("void f(TokenError err) { }");
        assert!(edge_targets(&result, EdgeKind::References).contains(&"TokenError".to_string()));
    }

    #[test]
    fn test_builtin_parameter_types_are_not_referenced() {
        let result = extract("void f(int a, double b, char* c) { }");
        assert!(edge_targets(&result, EdgeKind::References).is_empty());
    }

    #[test]
    fn test_signature_includes_return_and_params() {
        let result = extract("int add(int a, int b) { return a + b; }");
        let sym = result.symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(sym.signature.as_deref(), Some("int (int a, int b)"));
    }

    #[test]
    fn test_calls_attach_to_enclosing_function() {
        let result = extract("int login(void) { return validate_token(); }");
        let f = result.symbols.iter().find(|s| s.name == "login").unwrap();
        let call = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "validate_token")
            .unwrap();
        assert_eq!(call.source_id, f.id);
    }

    #[test]
    fn test_nested_anonymous_struct_fields_attach_to_parent() {
        let result = extract("struct Outer { struct { int inner_field; }; };");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "inner_field")
            .unwrap();
        assert!(sym.id.ends_with("Outer.inner_field"));
    }

    // A named nested struct is its own record; regression for nested types
    // being dropped because they arrive wrapped in a `field_declaration`.
    #[test]
    fn test_named_nested_struct_is_its_own_record() {
        let result = extract("struct Outer { struct Inner { int v; }; };");
        let inner = result
            .symbols
            .iter()
            .find(|s| s.name == "Inner" && s.kind == SymbolKind::Class)
            .unwrap();
        assert!(inner.id.ends_with("Outer.Inner"));
    }

    // A data member whose type is a declared struct must stay a Variable.
    #[test]
    fn test_member_of_struct_type_is_still_a_variable() {
        let result = extract("struct Outer { struct Inner field; };");
        let sym = result.symbols.iter().find(|s| s.name == "field").unwrap();
        assert_eq!(sym.kind, SymbolKind::Variable);
    }

    #[test]
    fn test_preprocessor_guarded_code_is_extracted() {
        let result = extract("#ifndef H\n#define H\nint guarded(void) { return 1; }\n#endif\n");
        assert!(result.symbols.iter().any(|s| s.name == "guarded"));
    }
}
