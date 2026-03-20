use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use crate::types::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{node_text, ExtractionResult, Extractor};

pub struct JavaExtractor {
    parser: Parser,
}

impl JavaExtractor {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_java::LANGUAGE))
            .expect("Java grammar should always load");
        Self { parser }
    }
}

impl Default for JavaExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for JavaExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        let tree = self
            .parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {file_path}"))?;

        let mut symbols = Vec::new();
        let mut edges = Vec::new();

        extract_node(
            tree.root_node(),
            source,
            file_path,
            None,
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
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    match node.kind() {
        "class_declaration" | "enum_declaration" | "annotation_type_declaration" => {
            extract_class_like(node, source, file_path, parent_id, symbols, edges);
        }
        "interface_declaration" => {
            extract_interface(node, source, file_path, parent_id, symbols, edges);
        }
        "import_declaration" => {
            extract_import(node, source, file_path, parent_id, symbols, edges);
        }
        _ => {
            for child in node.named_children(&mut node.walk()) {
                extract_node(child, source, file_path, parent_id, symbols, edges);
            }
        }
    }
}

// ── Class-like declarations (class, enum, @interface) ──

fn extract_class_like(
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
    let visibility = java_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    let mut sym = Symbol::new(
        name.clone(),
        if node.kind() == "enum_declaration" {
            SymbolKind::Enum
        } else {
            SymbolKind::Class
        },
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
    )
    .with_parent(parent_id)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Annotation references on the class itself
    extract_annotation_refs(node, source, file_path, &sym_id, edges);

    // extends (superclass)
    if let Some(sc) = node.child_by_field_name("superclass") {
        extract_superclass_edges(sc, source, file_path, &sym_id, start_line, edges);
    }

    // implements (super_interfaces)
    if let Some(si) = node.child_by_field_name("interfaces") {
        extract_super_interfaces_edges(
            si,
            source,
            file_path,
            &sym_id,
            start_line,
            EdgeKind::Implements,
            edges,
        );
    }

    // Walk body for methods, constructors, fields, nested named classes
    let body_field = match node.kind() {
        "enum_declaration" => "body",
        _ => "body",
    };
    if let Some(body) = node.child_by_field_name(body_field) {
        extract_class_body(body, source, file_path, &sym_id, symbols, edges);
    }
}

// ── Interface declarations ──

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
    let visibility = java_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    let mut sym = Symbol::new(
        name.clone(),
        SymbolKind::Interface,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
    )
    .with_parent(parent_id)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Annotation references on the interface
    extract_annotation_refs(node, source, file_path, &sym_id, edges);

    // extends (extends_interfaces)
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "extends_interfaces" {
            extract_super_interfaces_edges(
                child,
                source,
                file_path,
                &sym_id,
                start_line,
                EdgeKind::Inherits,
                edges,
            );
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_class_body(body, source, file_path, &sym_id, symbols, edges);
    }
}

// ── Class body traversal ──

fn extract_class_body(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "method_declaration" => {
                extract_method(child, source, file_path, parent_id, symbols, edges);
            }
            "constructor_declaration" => {
                extract_constructor(child, source, file_path, parent_id, symbols, edges);
            }
            "field_declaration" | "constant_declaration" => {
                extract_field(child, source, file_path, parent_id, symbols, edges);
            }
            // Named nested classes — option C: named only, skip anonymous
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "annotation_type_declaration" => {
                if child.kind() == "interface_declaration" {
                    extract_interface(child, source, file_path, Some(parent_id), symbols, edges);
                } else {
                    extract_class_like(child, source, file_path, Some(parent_id), symbols, edges);
                }
            }
            "enum_body_declarations" => {
                // enum body contains methods/fields after the semicolon
                extract_class_body(child, source, file_path, parent_id, symbols, edges);
            }
            _ => {}
        }
    }
}

// ── Methods ──

fn extract_method(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = java_visibility(node, source);
    let signature = extract_method_signature(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    let mut sym = Symbol::new(
        name,
        SymbolKind::Method,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
    )
    .with_parent(Some(parent_id))
    .with_signature(signature)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Annotation references on the method
    extract_annotation_refs(node, source, file_path, &sym_id, edges);

    // throws clauses → Raises edges
    extract_throws_edges(node, source, file_path, &sym_id, edges);

    // Type refs from parameters and return type
    extract_method_type_refs(node, source, file_path, &sym_id, edges);

    // Walk body for calls
    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, &sym_id, edges);
    }
}

// ── Constructors ──

fn extract_constructor(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = java_visibility(node, source);
    let signature = extract_constructor_signature(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    let mut sym = Symbol::new(
        name,
        SymbolKind::Method,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
    )
    .with_parent(Some(parent_id))
    .with_signature(signature)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    extract_annotation_refs(node, source, file_path, &sym_id, edges);
    extract_throws_edges(node, source, file_path, &sym_id, edges);

    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, &sym_id, edges);
    }
}

// ── Fields ──

fn extract_field(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let type_node = node.child_by_field_name("type");
    let type_text = type_node
        .map(|t| node_text(t, source).to_string())
        .unwrap_or_default();

    let visibility = java_visibility(node, source);
    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    // A field_declaration may have multiple declarators: `int a, b;`
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "variable_declarator" {
            let name = match child.child_by_field_name("name") {
                Some(n) => node_text(n, source).to_string(),
                None => continue,
            };

            let decl_line = child.start_position().row as u32 + 1;
            let signature = if type_text.is_empty() {
                None
            } else {
                Some(format!("{type_text} {name}"))
            };

            let sym_id = symbol_id(file_path, &name, decl_line);
            let mut sym = Symbol::new(
                name,
                SymbolKind::Variable,
                file_path,
                start_line,
                end_line,
                node.start_byte() as u32,
                node.end_byte() as u32,
            )
            .with_parent(Some(parent_id))
            .with_signature(signature);
            if visibility != Visibility::Public {
                sym = sym.with_visibility(visibility);
            }
            symbols.push(sym);

            // Type reference for the field's type (named types only)
            if let Some(tn) = type_node {
                collect_type_refs(tn, source, file_path, &sym_id, start_line, edges);
            }
        }
    }
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
    // Reconstruct the import path from children (identifier / scoped_identifier / asterisk)
    let import_text = node_text(node, source)
        .trim_start_matches("import")
        .trim_start_matches("static")
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_string();

    if import_text.is_empty() {
        return;
    }

    let sym_id = symbol_id(file_path, &import_text, line);
    symbols.push(
        Symbol::new(
            import_text.clone(),
            SymbolKind::Import,
            file_path,
            line,
            line,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_signature(Some(
            node_text(node, source)
                .trim_end_matches(';')
                .trim()
                .to_string(),
        )),
    );

    // The imported name is the last segment of the dotted path
    let target = import_text.rsplit('.').next().unwrap_or(&import_text);
    if target != "*" {
        edges.push(Edge::new(
            sym_id,
            target,
            EdgeKind::Imports,
            file_path,
            line,
        ));
    }
}

// ── Call walking ──

fn walk_for_calls(
    node: Node,
    source: &str,
    file_path: &str,
    context_id: &str,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    let mut did_visit_children = false;

    loop {
        let current = cursor.node();

        if !did_visit_children {
            match current.kind() {
                "method_invocation" => {
                    let callee = build_callee_name(current, source);
                    if !callee.is_empty() {
                        edges.push(Edge::new(
                            context_id,
                            callee,
                            EdgeKind::Calls,
                            file_path,
                            current.start_position().row as u32 + 1,
                        ));
                    }
                }
                "object_creation_expression" => {
                    if let Some(type_node) = current.child_by_field_name("type") {
                        let type_name = extract_simple_type_name(type_node, source);
                        if !type_name.is_empty() {
                            edges.push(Edge::new(
                                context_id,
                                type_name,
                                EdgeKind::References,
                                file_path,
                                current.start_position().row as u32 + 1,
                            ));
                        }
                    }
                    // Don't descend into anonymous class bodies — option C
                    for child in current.named_children(&mut current.walk()) {
                        if child.kind() == "class_body" {
                            did_visit_children = true;
                            break;
                        }
                    }
                    if did_visit_children {
                        continue;
                    }
                }
                "throw_statement" => {
                    extract_throw_ref(current, source, file_path, context_id, edges);
                }
                // Don't descend into lambda/anonymous class bodies nested inside this scope
                "lambda_expression" => {
                    did_visit_children = true;
                    continue;
                }
                _ => {}
            }
        }

        if !did_visit_children && cursor.goto_first_child() {
            did_visit_children = false;
            continue;
        }
        did_visit_children = false;
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Build `object.method` or just `method` callee name from a method_invocation node.
fn build_callee_name(node: Node, source: &str) -> String {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source),
        None => return String::new(),
    };
    if let Some(obj) = node.child_by_field_name("object") {
        let obj_text = node_text(obj, source);
        if !obj_text.is_empty() {
            return format!("{obj_text}.{name}");
        }
    }
    name.to_string()
}

/// Extract throw target name from a throw_statement child expression.
fn extract_throw_ref(
    node: Node,
    source: &str,
    file_path: &str,
    context_id: &str,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    for child in node.named_children(&mut node.walk()) {
        // throw new SomeException(...) or throw someVar
        let type_name = match child.kind() {
            "object_creation_expression" => child
                .child_by_field_name("type")
                .map(|t| extract_simple_type_name(t, source))
                .unwrap_or_default(),
            "identifier" => node_text(child, source).to_string(),
            _ => String::new(),
        };
        if !type_name.is_empty() {
            edges.push(Edge::new(
                context_id,
                type_name,
                EdgeKind::Raises,
                file_path,
                line,
            ));
            return;
        }
    }
}

// ── Inheritance edge helpers ──

fn extract_superclass_edges(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    for child in node.named_children(&mut node.walk()) {
        let name = extract_simple_type_name(child, source);
        if !name.is_empty() {
            edges.push(Edge::new(sym_id, name, EdgeKind::Inherits, file_path, line));
        }
    }
}

fn extract_super_interfaces_edges(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edge_kind: EdgeKind,
    edges: &mut Vec<Edge>,
) {
    // super_interfaces → type_list → type_identifier / scoped_type_identifier / generic_type
    // extends_interfaces (on interface) has same structure
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "type_list" {
            for type_node in child.named_children(&mut child.walk()) {
                let name = extract_simple_type_name(type_node, source);
                if !name.is_empty() {
                    edges.push(Edge::new(sym_id, name, edge_kind, file_path, line));
                }
            }
        } else {
            // direct type_identifier (e.g. extends_interfaces on interface)
            let name = extract_simple_type_name(child, source);
            if !name.is_empty() {
                edges.push(Edge::new(sym_id, name, edge_kind, file_path, line));
            }
        }
    }
}

// ── Throws edges (declared exceptions) ──

fn extract_throws_edges(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "throws" {
            let line = child.start_position().row as u32 + 1;
            for exc in child.named_children(&mut child.walk()) {
                let name = extract_simple_type_name(exc, source);
                if !name.is_empty() {
                    edges.push(Edge::new(sym_id, name, EdgeKind::Raises, file_path, line));
                }
            }
        }
    }
}

// ── Annotation reference edges ──

fn extract_annotation_refs(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    // Annotations appear as children of the node (not in a field).
    // They precede modifiers and the declaration keyword.
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "modifiers" => {
                // modifiers node contains annotation / marker_annotation children
                for mod_child in child.named_children(&mut child.walk()) {
                    if let Some(name) = annotation_name(mod_child, source) {
                        edges.push(Edge::new(
                            sym_id,
                            name,
                            EdgeKind::References,
                            file_path,
                            mod_child.start_position().row as u32 + 1,
                        ));
                    }
                }
            }
            "annotation" | "marker_annotation" => {
                if let Some(name) = annotation_name(child, source) {
                    edges.push(Edge::new(
                        sym_id,
                        name,
                        EdgeKind::References,
                        file_path,
                        child.start_position().row as u32 + 1,
                    ));
                }
            }
            _ => {}
        }
    }
}

fn annotation_name(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "annotation" | "marker_annotation" => node
            .child_by_field_name("name")
            .map(|n| node_text(n, source).to_string())
            .filter(|s| !s.is_empty()),
        _ => None,
    }
}

// ── Type reference collection ──

fn extract_method_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    // Return type
    if let Some(ret) = node.child_by_field_name("type") {
        collect_type_refs(
            ret,
            source,
            file_path,
            sym_id,
            ret.start_position().row as u32 + 1,
            edges,
        );
    }
    // Parameters
    if let Some(params) = node.child_by_field_name("parameters") {
        for param in params.named_children(&mut params.walk()) {
            if param.kind() == "formal_parameter" || param.kind() == "spread_parameter" {
                if let Some(type_node) = param.child_by_field_name("type") {
                    collect_type_refs(
                        type_node,
                        source,
                        file_path,
                        sym_id,
                        type_node.start_position().row as u32 + 1,
                        edges,
                    );
                }
            }
        }
    }
}

/// Recursively collect named type references, filtering out primitive/void types.
fn collect_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    match node.kind() {
        "type_identifier" => {
            let name = node_text(node, source);
            if !name.is_empty() && !is_primitive(name) {
                edges.push(Edge::new(
                    sym_id,
                    name,
                    EdgeKind::References,
                    file_path,
                    line,
                ));
            }
        }
        "void_type" | "integral_type" | "floating_point_type" | "boolean_type" => {}
        _ => {
            for child in node.named_children(&mut node.walk()) {
                collect_type_refs(child, source, file_path, sym_id, line, edges);
            }
        }
    }
}

fn is_primitive(name: &str) -> bool {
    matches!(
        name,
        "byte"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "boolean"
            | "char"
            | "String"
            | "Object"
            | "void"
    )
}

/// Extract the simple (unqualified) type name from a type node.
fn extract_simple_type_name(node: Node, source: &str) -> String {
    match node.kind() {
        "type_identifier" => node_text(node, source).to_string(),
        "scoped_type_identifier" => {
            // com.example.Foo → "Foo"
            let text = node_text(node, source);
            text.rsplit('.').next().unwrap_or(text).to_string()
        }
        "generic_type" => {
            // List<String> → extract just the outer type name
            node.named_child(0)
                .map(|c| extract_simple_type_name(c, source))
                .unwrap_or_default()
        }
        "array_type" => {
            // int[] or Foo[] → recurse into the element type
            node.named_child(0)
                .map(|c| extract_simple_type_name(c, source))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

// ── Signature builders ──

fn extract_method_signature(node: Node, source: &str) -> Option<String> {
    let ret = node
        .child_by_field_name("type")
        .map(|t| node_text(t, source));
    let params = node
        .child_by_field_name("parameters")
        .map(|p| node_text(p, source))
        .unwrap_or("()");
    let throws = extract_throws_text(node, source);

    Some(format!(
        "{}{}{}",
        ret.map(|r| format!("{r} ")).unwrap_or_default(),
        params,
        throws.map(|t| format!(" throws {t}")).unwrap_or_default(),
    ))
}

fn extract_constructor_signature(node: Node, source: &str) -> Option<String> {
    let params = node
        .child_by_field_name("parameters")
        .map(|p| node_text(p, source))
        .unwrap_or("()");
    let throws = extract_throws_text(node, source);

    Some(format!(
        "{}{}",
        params,
        throws.map(|t| format!(" throws {t}")).unwrap_or_default(),
    ))
}

fn extract_throws_text(node: Node, source: &str) -> Option<String> {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "throws" {
            let types: Vec<&str> = child
                .named_children(&mut child.walk())
                .map(|c| node_text(c, source))
                .filter(|s| !s.is_empty())
                .collect();
            if !types.is_empty() {
                return Some(types.join(", "));
            }
        }
    }
    None
}

// ── Visibility ──

fn java_visibility(node: Node, source: &str) -> Visibility {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "modifiers" {
            let text = node_text(child, source);
            if text.contains("private") {
                return Visibility::Private;
            }
            if text.contains("protected") {
                return Visibility::Protected;
            }
            if text.contains("public") {
                return Visibility::Public;
            }
            // Modifiers present but no access modifier → package-private → Public
            return Visibility::Public;
        }
    }
    // No modifiers at all → package-private → Public
    Visibility::Public
}

// ── Doc comment extraction ──

fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    // Look for a block_comment (/** ... */) or line_comment chain immediately before the node.
    let prev = node.prev_sibling()?;

    if prev.kind() == "block_comment" {
        let text = node_text(prev, source);
        // Strip leading /** and trailing */ and per-line leading * prefixes
        let inner = text
            .trim_start_matches("/**")
            .trim_start_matches("/*")
            .trim_end_matches("*/")
            .trim();
        let lines: Vec<&str> = inner
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .filter(|l| !l.is_empty())
            .collect();
        if !lines.is_empty() {
            return Some(lines.join(" "));
        }
        return None;
    }

    if prev.kind() == "line_comment" {
        let mut lines = Vec::new();
        let mut p: Option<Node> = Some(prev);
        while let Some(n) = p {
            if n.kind() == "line_comment" {
                let text = node_text(n, source);
                if let Some(stripped) = text.strip_prefix("//") {
                    lines.push(stripped.trim().to_string());
                } else {
                    break;
                }
                p = n.prev_sibling();
            } else {
                break;
            }
        }
        if !lines.is_empty() {
            lines.reverse();
            return Some(lines.join(" "));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> ExtractionResult {
        let mut ext = JavaExtractor::new();
        ext.extract(source, "Test.java").unwrap()
    }

    #[test]
    fn test_empty_file() {
        let result = extract("");
        assert!(result.symbols.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_syntax_error_partial_parse() {
        let result = extract("class Broken {");
        let _ = result.symbols.len();
    }

    #[test]
    fn test_class() {
        let result = extract(
            r#"
public class UserService {
}
"#,
        );
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "UserService")
            .unwrap();
        assert_eq!(sym.kind, SymbolKind::Class);
        assert_eq!(sym.visibility, Visibility::Public);
    }

    #[test]
    fn test_interface() {
        let result = extract(
            r#"
public interface Repository {
    void save(Object entity);
}
"#,
        );
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "Repository")
            .unwrap();
        assert_eq!(sym.kind, SymbolKind::Interface);
        assert_eq!(sym.visibility, Visibility::Public);
    }

    #[test]
    fn test_enum() {
        let result = extract(
            r#"
public enum Status {
    ACTIVE, INACTIVE;
}
"#,
        );
        let sym = result.symbols.iter().find(|s| s.name == "Status").unwrap();
        assert_eq!(sym.kind, SymbolKind::Enum);
        assert_eq!(sym.visibility, Visibility::Public);
    }

    #[test]
    fn test_method() {
        let result = extract(
            r#"
public class Foo {
    public String greet(String name) {
        return "Hello " + name;
    }
}
"#,
        );
        let m = result.symbols.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.visibility, Visibility::Public);
        assert!(m.parent_id.is_some());
        let sig = m.signature.as_deref().unwrap();
        assert!(sig.contains("String"));
    }

    #[test]
    fn test_constructor() {
        let result = extract(
            r#"
public class Foo {
    public Foo(int x) {}
}
"#,
        );
        let ctor = result
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Method);
        assert!(ctor.is_some());
    }

    #[test]
    fn test_field() {
        let result = extract(
            r#"
public class Foo {
    private int count;
    protected String name;
}
"#,
        );
        let count = result.symbols.iter().find(|s| s.name == "count").unwrap();
        assert_eq!(count.kind, SymbolKind::Variable);
        assert_eq!(count.visibility, Visibility::Private);

        let name_field = result.symbols.iter().find(|s| s.name == "name").unwrap();
        assert_eq!(name_field.visibility, Visibility::Protected);
    }

    #[test]
    fn test_imports() {
        let result = extract(
            r#"
import java.util.List;
import java.util.Map;
import static java.lang.Math.PI;

public class Foo {}
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
        let targets: Vec<&str> = import_edges
            .iter()
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(targets.contains(&"List"));
        assert!(targets.contains(&"Map"));
        assert!(targets.contains(&"PI"));
    }

    #[test]
    fn test_wildcard_import_no_edge() {
        let result = extract(
            r#"
import java.util.*;
public class Foo {}
"#,
        );
        let import_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();
        // Wildcard imports produce no edge (no specific symbol to link)
        assert!(import_edges.is_empty());
    }

    #[test]
    fn test_visibility() {
        let result = extract(
            r#"
public class Foo {
    public void pubMethod() {}
    private void privMethod() {}
    protected void protMethod() {}
    void packageMethod() {}
}
"#,
        );
        let pub_m = result
            .symbols
            .iter()
            .find(|s| s.name == "pubMethod")
            .unwrap();
        assert_eq!(pub_m.visibility, Visibility::Public);

        let priv_m = result
            .symbols
            .iter()
            .find(|s| s.name == "privMethod")
            .unwrap();
        assert_eq!(priv_m.visibility, Visibility::Private);

        let prot_m = result
            .symbols
            .iter()
            .find(|s| s.name == "protMethod")
            .unwrap();
        assert_eq!(prot_m.visibility, Visibility::Protected);

        let pkg_m = result
            .symbols
            .iter()
            .find(|s| s.name == "packageMethod")
            .unwrap();
        assert_eq!(pkg_m.visibility, Visibility::Public);
    }

    #[test]
    fn test_extends_implements() {
        let result = extract(
            r#"
public class UserService extends BaseService implements Repository, Auditable {
}
"#,
        );
        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        let inherits_targets: Vec<&str> = inherits.iter().map(|e| e.target_name.as_str()).collect();
        assert!(inherits_targets.contains(&"BaseService"));

        let implements: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        let implements_targets: Vec<&str> =
            implements.iter().map(|e| e.target_name.as_str()).collect();
        assert!(implements_targets.contains(&"Repository"));
        assert!(implements_targets.contains(&"Auditable"));
    }

    #[test]
    fn test_interface_extends() {
        let result = extract(
            r#"
public interface UserRepo extends Repository, CrudRepository {
}
"#,
        );
        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        let targets: Vec<&str> = inherits.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"Repository"));
        assert!(targets.contains(&"CrudRepository"));
    }

    #[test]
    fn test_method_calls() {
        let result = extract(
            r#"
public class Foo {
    public void process() {
        fetchData();
        transform(data);
        repo.save(entity);
    }
}
"#,
        );
        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        let targets: Vec<&str> = calls.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"fetchData"));
        assert!(targets.contains(&"transform"));
        assert!(targets.contains(&"repo.save"));
    }

    #[test]
    fn test_new_expression_refs() {
        let result = extract(
            r#"
public class Foo {
    public void build() {
        User user = new User("Alice");
        Config cfg = new Config();
    }
}
"#,
        );
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"User"));
        assert!(targets.contains(&"Config"));
    }

    #[test]
    fn test_throws_declared() {
        let result = extract(
            r#"
public class Foo {
    public void read() throws IOException, ParseException {
    }
}
"#,
        );
        let raises: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Raises)
            .collect();
        let targets: Vec<&str> = raises.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"IOException"));
        assert!(targets.contains(&"ParseException"));
    }

    #[test]
    fn test_throw_statement() {
        let result = extract(
            r#"
public class Foo {
    public void fail() {
        throw new IllegalArgumentException("bad");
    }
}
"#,
        );
        let raises: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Raises)
            .collect();
        let targets: Vec<&str> = raises.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"IllegalArgumentException"));
    }

    #[test]
    fn test_annotation_refs() {
        let result = extract(
            r#"
public class Foo {
    @Override
    @Deprecated
    public void legacyMethod() {}
}
"#,
        );
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"Override"));
        assert!(targets.contains(&"Deprecated"));
    }

    #[test]
    fn test_docstring_javadoc() {
        let result = extract(
            r#"
/**
 * Processes the given request.
 */
public class Processor {
}
"#,
        );
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "Processor")
            .unwrap();
        assert_eq!(
            sym.docstring.as_deref(),
            Some("Processes the given request.")
        );
    }

    #[test]
    fn test_nested_class() {
        let result = extract(
            r#"
public class Outer {
    public static class Builder {
        private String name;
        public Builder name(String n) { this.name = n; return this; }
    }
}
"#,
        );
        let outer = result.symbols.iter().find(|s| s.name == "Outer").unwrap();
        let builder = result.symbols.iter().find(|s| s.name == "Builder").unwrap();
        assert!(builder.parent_id.is_some());
        assert!(builder.parent_id.as_ref().unwrap().contains("Outer"));

        let name_field = result
            .symbols
            .iter()
            .find(|s| s.name == "name" && s.kind == SymbolKind::Variable)
            .unwrap();
        assert!(name_field.parent_id.as_ref().unwrap().contains("Builder"));

        let _ = outer;
    }

    #[test]
    fn test_type_refs_in_signature() {
        let result = extract(
            r#"
public class Foo {
    public Response handle(Request req, Context ctx) {
        return null;
    }
}
"#,
        );
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"Response"));
        assert!(targets.contains(&"Request"));
        assert!(targets.contains(&"Context"));
    }
}
