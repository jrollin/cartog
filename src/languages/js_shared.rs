//! Shared extraction logic for JavaScript and TypeScript.
//!
//! Both languages share the same CST node structure for the constructs
//! we care about. TypeScript adds type annotations, but the named node
//! kinds for functions, classes, imports, and calls are identical.

use anyhow::Result;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, QueryCursor};

use crate::types::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::queries::{is_inside_nested_scope, CachedQuery};
use super::{node_text, ExtractionResult};

/// Pre-compiled tree-sitter queries for JS/TS extraction.
pub struct JsQueries {
    /// `foo()`, `obj.method()`
    call_query: CachedQuery,
    call_callee_idx: u32,
    /// `new Foo()`
    new_query: CachedQuery,
    new_ctor_idx: u32,
    /// `throw new Error()`, `throw expr`
    throw_query: CachedQuery,
    throw_exc_idx: u32,
    /// Type identifiers in annotations (TypeScript only; JS has no `type_identifier` node)
    type_ref_query: Option<CachedQuery>,
    type_ref_idx: u32,
}

impl JsQueries {
    pub fn new(language: &Language) -> Self {
        let call_query = CachedQuery::new(
            language,
            "(call_expression function: [(identifier) (member_expression)] @callee)",
        );
        let call_callee_idx = call_query.capture_index("callee");

        let new_query = CachedQuery::new(
            language,
            "(new_expression constructor: [(identifier) (member_expression)] @ctor)",
        );
        let new_ctor_idx = new_query.capture_index("ctor");

        let throw_query = CachedQuery::new(
            language,
            r#"(throw_statement
              [(new_expression constructor: [(identifier) (member_expression)] @exception)
               (call_expression function: [(identifier) (member_expression)] @exception)
               (identifier) @exception
               (member_expression) @exception])"#,
        );
        let throw_exc_idx = throw_query.capture_index("exception");

        // `type_identifier` exists only in TypeScript grammars, not plain JavaScript.
        let (type_ref_query, type_ref_idx) =
            match CachedQuery::try_new(language, "(type_identifier) @type_ref") {
                Some(q) => {
                    let idx = q.capture_index("type_ref");
                    (Some(q), idx)
                }
                None => (None, 0),
            };

        Self {
            call_query,
            call_callee_idx,
            new_query,
            new_ctor_idx,
            throw_query,
            throw_exc_idx,
            type_ref_query,
            type_ref_idx,
        }
    }
}

/// Parse source and extract symbols + edges. Works for JS, TS, and TSX.
pub fn extract(
    parser: &mut Parser,
    queries: &JsQueries,
    source: &str,
    file_path: &str,
) -> Result<ExtractionResult> {
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse {file_path}"))?;

    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    extract_node(
        tree.root_node(),
        source,
        file_path,
        None,
        queries,
        &mut symbols,
        &mut edges,
    );

    Ok(ExtractionResult { symbols, edges })
}

fn extract_node(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    queries: &JsQueries,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    match node.kind() {
        // Functions
        "function_declaration" => {
            extract_function(node, source, file_path, parent_id, queries, symbols, edges);
        }
        // Arrow functions and function expressions assigned to variables
        "lexical_declaration" | "variable_declaration" => {
            extract_variable_declaration(
                node, source, file_path, parent_id, queries, symbols, edges,
            );
        }
        // Classes
        "class_declaration" => {
            extract_class(node, source, file_path, parent_id, queries, symbols, edges);
        }
        // Imports
        "import_statement" => {
            extract_import(node, source, file_path, parent_id, symbols, edges);
        }
        // Exports that wrap declarations
        "export_statement" => {
            for child in node.named_children(&mut node.walk()) {
                extract_node(child, source, file_path, parent_id, queries, symbols, edges);
            }
        }
        // Expression statements — scan for calls
        "expression_statement" => {
            walk_for_calls_and_throws_q(node, source, file_path, parent_id, queries, edges);
        }
        // TypeScript-specific
        "interface_declaration" => {
            extract_interface(node, source, file_path, parent_id, symbols, edges);
        }
        "type_alias_declaration" => {
            extract_type_alias(node, source, file_path, parent_id, symbols);
        }
        "enum_declaration" => {
            extract_enum(node, source, file_path, parent_id, symbols);
        }
        _ => {
            for child in node.named_children(&mut node.walk()) {
                extract_node(child, source, file_path, parent_id, queries, symbols, edges);
            }
        }
    }
}

// ── Functions ──

fn extract_function(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    queries: &JsQueries,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let is_method = parent_id.is_some();
    let kind = if is_method {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };

    let is_async = has_async_keyword(node, source);
    let signature = extract_signature(node, source);
    let docstring = extract_jsdoc(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    symbols.push(
        Symbol::new(
            &name,
            kind,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_signature(signature)
        .with_async(is_async)
        .with_docstring(docstring),
    );

    // Extract type annotation references from parameters and return type
    extract_fn_type_refs_q(node, source, file_path, &sym_id, queries, edges);

    // Walk body for calls/throws
    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls_and_throws_q(body, source, file_path, Some(&sym_id), queries, edges);
        walk_body_for_nested(body, source, file_path, &sym_id, queries, symbols, edges);
    }
}

// ── Variable declarations (const foo = () => {}, const bar = function() {}) ──

fn extract_variable_declaration(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    queries: &JsQueries,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() != "variable_declarator" {
            continue;
        }

        let name_node = match child.child_by_field_name("name") {
            Some(n) if n.kind() == "identifier" => n,
            _ => continue,
        };
        let name = node_text(name_node, source).to_string();
        let start_line = node.start_position().row as u32 + 1;
        let end_line = node.end_position().row as u32 + 1;

        let value = child.child_by_field_name("value");

        let is_function = value.as_ref().is_some_and(|v| is_function_like(v.kind()));

        if is_function {
            let Some(val) = value else {
                continue;
            };
            let is_async = has_async_keyword(val, source);
            let signature = extract_signature(val, source);
            let docstring = extract_jsdoc(node, source);

            let sym_id = symbol_id(file_path, &name, start_line);
            symbols.push(
                Symbol::new(
                    &name,
                    SymbolKind::Function,
                    file_path,
                    start_line,
                    end_line,
                    node.start_byte() as u32,
                    node.end_byte() as u32,
                )
                .with_parent(parent_id)
                .with_signature(signature)
                .with_async(is_async)
                .with_docstring(docstring),
            );

            extract_fn_type_refs_q(val, source, file_path, &sym_id, queries, edges);

            if let Some(body) = val.child_by_field_name("body") {
                walk_for_calls_and_throws_q(body, source, file_path, Some(&sym_id), queries, edges);
                walk_body_for_nested(body, source, file_path, &sym_id, queries, symbols, edges);
            }
        } else {
            // Plain variable
            let docstring = extract_jsdoc(node, source);
            symbols.push(
                Symbol::new(
                    &name,
                    SymbolKind::Variable,
                    file_path,
                    start_line,
                    end_line,
                    node.start_byte() as u32,
                    node.end_byte() as u32,
                )
                .with_parent(parent_id)
                .with_docstring(docstring),
            );
        }
    }
}

// ── Classes ──

fn extract_class(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    queries: &JsQueries,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let docstring = extract_jsdoc(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    symbols.push(
        Symbol::new(
            &name,
            SymbolKind::Class,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_docstring(docstring),
    );

    // Inheritance: class_heritage contains extends_clause (TS) or direct identifier (JS)
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "class_heritage" {
                for clause in child.named_children(&mut child.walk()) {
                    match clause.kind() {
                        "extends_clause" => {
                            if let Some(val) = clause.child_by_field_name("value") {
                                let base_name = extract_type_name(val, source);
                                if !base_name.is_empty() {
                                    edges.push(Edge::new(
                                        sym_id.clone(),
                                        base_name,
                                        EdgeKind::Inherits,
                                        file_path,
                                        val.start_position().row as u32 + 1,
                                    ));
                                }
                            }
                        }
                        "implements_clause" => {
                            for tc in clause.named_children(&mut clause.walk()) {
                                let iface_name = extract_type_name(tc, source);
                                if !iface_name.is_empty() {
                                    edges.push(Edge::new(
                                        sym_id.clone(),
                                        iface_name,
                                        EdgeKind::Implements,
                                        file_path,
                                        tc.start_position().row as u32 + 1,
                                    ));
                                }
                            }
                        }
                        // JS: class_heritage contains the superclass identifier directly
                        "identifier" | "member_expression" => {
                            let base_name = extract_type_name(clause, source);
                            if !base_name.is_empty() {
                                edges.push(Edge::new(
                                    sym_id.clone(),
                                    base_name,
                                    EdgeKind::Inherits,
                                    file_path,
                                    clause.start_position().row as u32 + 1,
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Walk class body
    if let Some(body) = node.child_by_field_name("body") {
        for child in body.named_children(&mut body.walk()) {
            match child.kind() {
                "method_definition" => {
                    extract_method(child, source, file_path, &sym_id, queries, symbols, edges);
                }
                "public_field_definition" | "field_definition" | "property_definition" => {
                    extract_field(child, source, file_path, &sym_id, symbols);
                }
                _ => {}
            }
        }
    }
}

fn extract_method(
    node: Node,
    source: &str,
    file_path: &str,
    class_id: &str,
    queries: &JsQueries,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let is_async = has_async_keyword(node, source);
    let signature = extract_signature(node, source);
    let docstring = extract_jsdoc(node, source);
    let visibility = js_visibility_from_node(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    symbols.push(
        Symbol::new(
            &name,
            SymbolKind::Method,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(Some(class_id))
        .with_signature(signature)
        .with_visibility(visibility)
        .with_async(is_async)
        .with_docstring(docstring),
    );

    extract_fn_type_refs_q(node, source, file_path, &sym_id, queries, edges);

    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls_and_throws_q(body, source, file_path, Some(&sym_id), queries, edges);
    }
}

fn extract_field(
    node: Node,
    source: &str,
    file_path: &str,
    class_id: &str,
    symbols: &mut Vec<Symbol>,
) {
    // field_definition uses "property" field, public_field_definition uses "name"
    let name = match node
        .child_by_field_name("property")
        .or_else(|| node.child_by_field_name("name"))
    {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = js_visibility_from_node(node, source);

    symbols.push(
        Symbol::new(
            &name,
            SymbolKind::Variable,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(Some(class_id))
        .with_visibility(visibility),
    );
}

// ── Imports ──

fn extract_import(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    let import_text = node_text(node, source).to_string();

    let module_name = extract_import_source(node, source);
    if module_name.is_empty() {
        return;
    }

    let sym_id = symbol_id(file_path, &module_name, line);
    symbols.push(
        Symbol::new(
            &module_name,
            SymbolKind::Import,
            file_path,
            line,
            line,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_signature(Some(import_text)),
    );

    // Collect imported names
    let names = collect_imported_names(node, source);
    for imported in names {
        edges.push(Edge::new(
            sym_id.clone(),
            imported,
            EdgeKind::Imports,
            file_path,
            line,
        ));
    }
}

fn extract_import_source(node: Node, source: &str) -> String {
    // import ... from 'module'  —  the source is a string child
    node.child_by_field_name("source")
        .map(|s| {
            node_text(s, source)
                .trim_matches('\'')
                .trim_matches('"')
                .trim_matches('`')
                .to_string()
        })
        .unwrap_or_default()
}

fn collect_imported_names(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();

    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "import_clause" {
            for inner in child.named_children(&mut child.walk()) {
                match inner.kind() {
                    "identifier" => names.push(node_text(inner, source).to_string()),
                    "named_imports" => {
                        for spec in inner.named_children(&mut inner.walk()) {
                            if spec.kind() == "import_specifier" {
                                if let Some(n) = spec.child_by_field_name("name") {
                                    names.push(node_text(n, source).to_string());
                                }
                            }
                        }
                    }
                    "namespace_import" => {
                        if let Some(n) = inner.named_child(0) {
                            names.push(node_text(n, source).to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    names
}

// ── TypeScript-specific ──

fn extract_interface(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let docstring = extract_jsdoc(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    symbols.push(
        Symbol::new(
            &name,
            SymbolKind::Interface,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_docstring(docstring),
    );

    // interface Foo extends Bar, Baz
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "extends_type_clause" {
                for tc in child.named_children(&mut child.walk()) {
                    let base_name = extract_type_name(tc, source);
                    if !base_name.is_empty() {
                        edges.push(Edge::new(
                            sym_id.clone(),
                            base_name,
                            EdgeKind::Inherits,
                            file_path,
                            tc.start_position().row as u32 + 1,
                        ));
                    }
                }
            }
        }
    }
}

fn extract_type_alias(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let docstring = extract_jsdoc(node, source);

    symbols.push(
        Symbol::new(
            &name,
            SymbolKind::TypeAlias,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_docstring(docstring),
    );
}

fn extract_enum(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let docstring = extract_jsdoc(node, source);

    symbols.push(
        Symbol::new(
            &name,
            SymbolKind::Enum,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_docstring(docstring),
    );
}

// ── Call / Throw walking (query-based) ──

const JS_SCOPE_KINDS: &[&str] = &[
    "function_declaration",
    "arrow_function",
    "function_expression",
    "class_declaration",
    "method_definition",
];

fn walk_for_calls_and_throws_q(
    node: Node,
    source: &str,
    file_path: &str,
    context_id: Option<&str>,
    queries: &JsQueries,
    edges: &mut Vec<Edge>,
) {
    let Some(ctx) = context_id else { return };

    // Collect call edges
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&queries.call_query.query, node, source.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures {
            if capture.index == queries.call_callee_idx
                && !is_inside_nested_scope(capture.node, node, JS_SCOPE_KINDS)
            {
                let name = node_text(capture.node, source);
                if !name.is_empty() {
                    edges.push(Edge::new(
                        ctx,
                        name,
                        EdgeKind::Calls,
                        file_path,
                        capture.node.start_position().row as u32 + 1,
                    ));
                }
            }
        }
    }

    // Collect new expression edges (also treated as calls)
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&queries.new_query.query, node, source.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures {
            if capture.index == queries.new_ctor_idx
                && !is_inside_nested_scope(capture.node, node, JS_SCOPE_KINDS)
            {
                let name = node_text(capture.node, source);
                if !name.is_empty() {
                    edges.push(Edge::new(
                        ctx,
                        name,
                        EdgeKind::Calls,
                        file_path,
                        capture.node.start_position().row as u32 + 1,
                    ));
                }
            }
        }
    }

    // Collect throw edges
    let mut cursor = QueryCursor::new();
    let mut seen_throws = std::collections::HashSet::new();
    let mut matches = cursor.matches(&queries.throw_query.query, node, source.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures {
            if capture.index == queries.throw_exc_idx
                && !is_inside_nested_scope(capture.node, node, JS_SCOPE_KINDS)
            {
                let line = capture.node.start_position().row as u32 + 1;
                let name = node_text(capture.node, source);
                if !name.is_empty() && seen_throws.insert((name.to_string(), line)) {
                    edges.push(Edge::new(ctx, name, EdgeKind::Raises, file_path, line));
                }
            }
        }
    }
}

fn walk_body_for_nested(
    body: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    queries: &JsQueries,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    for child in body.named_children(&mut body.walk()) {
        match child.kind() {
            "function_declaration"
            | "class_declaration"
            | "lexical_declaration"
            | "variable_declaration" => {
                extract_node(
                    child,
                    source,
                    file_path,
                    Some(parent_id),
                    queries,
                    symbols,
                    edges,
                );
            }
            _ => {}
        }
    }
}

// ── Type reference extraction (query-based) ──

/// Extract type annotation references from function parameters and return type.
fn extract_fn_type_refs_q(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    queries: &JsQueries,
    edges: &mut Vec<Edge>,
) {
    // Walk parameters looking for type_identifier nodes
    if let Some(params) = node.child_by_field_name("parameters") {
        collect_type_refs_q(params, source, file_path, sym_id, queries, edges);
    }
    // Return type annotation
    if let Some(ret) = node.child_by_field_name("return_type") {
        collect_type_refs_q(ret, source, file_path, sym_id, queries, edges);
    }
}

/// Collect type_identifier references from a subtree using query API.
fn collect_type_refs_q(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    queries: &JsQueries,
    edges: &mut Vec<Edge>,
) {
    let type_ref_query = match &queries.type_ref_query {
        Some(q) => q,
        None => return,
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&type_ref_query.query, node, source.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures {
            if capture.index == queries.type_ref_idx {
                let name = node_text(capture.node, source);
                // Skip built-in types (lowercase: string, number, boolean, void, etc.)
                if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_uppercase()) {
                    edges.push(Edge::new(
                        sym_id,
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

// ── Helpers ──

fn is_function_like(kind: &str) -> bool {
    matches!(kind, "arrow_function" | "function_expression" | "function")
}

fn has_async_keyword(node: Node, source: &str) -> bool {
    // Check for "async" keyword as a child, or in the source prefix
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "async" {
                return true;
            }
        }
    }
    // Fallback: check source text
    let start = node.start_byte();
    let prefix_start = start.saturating_sub(6);
    let prefix = source.get(prefix_start..start).unwrap_or("");
    prefix.contains("async")
}

fn extract_signature(node: Node, source: &str) -> Option<String> {
    let params = node.child_by_field_name("parameters")?;
    let params_text = node_text(params, source);

    let return_type = node.child_by_field_name("return_type").map(|r| {
        // In TS, return_type is a type_annotation that already includes ":"
        let text = node_text(r, source);
        if text.starts_with(':') {
            text.to_string()
        } else {
            format!(": {text}")
        }
    });

    Some(format!("{params_text}{}", return_type.unwrap_or_default()))
}

/// Extract JSDoc comment preceding a node.
fn extract_jsdoc(node: Node, source: &str) -> Option<String> {
    // Look for a comment sibling preceding this node
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "comment" {
            let text = node_text(p, source);
            if text.starts_with("/**") {
                return parse_jsdoc(text);
            }
            return None;
        }
        // Skip over whitespace-like anonymous nodes
        if p.is_named() {
            return None;
        }
        prev = p.prev_sibling();
    }
    None
}

fn parse_jsdoc(text: &str) -> Option<String> {
    let inner = text.strip_prefix("/**")?.strip_suffix("*/")?;
    let cleaned: Vec<&str> = inner
        .lines()
        .map(|l| l.trim().trim_start_matches('*').trim())
        .filter(|l| !l.is_empty() && !l.starts_with('@'))
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.join(" "))
    }
}

fn js_visibility_from_node(node: Node, source: &str) -> Visibility {
    // Check for TS accessibility modifiers
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "accessibility_modifier" {
                let text = node_text(child, source);
                return match text {
                    "private" => Visibility::Private,
                    "protected" => Visibility::Protected,
                    _ => Visibility::Public,
                };
            }
        }
    }

    // Convention: #field or _field
    // Try both "name" and "property" fields (JS uses "property" for field_definition)
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("property"));
    if let Some(n) = name_node {
        let name = node_text(n, source);
        if name.starts_with('#') || n.kind() == "private_property_identifier" {
            return Visibility::Private;
        }
        if name.starts_with('_') {
            return Visibility::Protected;
        }
    }

    Visibility::Public
}

fn extract_type_name(node: Node, source: &str) -> String {
    // For generic types like Foo<Bar>, just get "Foo"
    if let Some(name) = node.child_by_field_name("name") {
        return node_text(name, source).to_string();
    }
    // For plain identifiers
    if node.kind() == "type_identifier" || node.kind() == "identifier" {
        return node_text(node, source).to_string();
    }
    // Fallback: try first named child
    node.named_child(0)
        .map(|c| node_text(c, source).to_string())
        .unwrap_or_default()
}
