use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use cartog_core::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{last_segment, node_text, ExtractionResult, Extractor};

/// Tree-sitter–based extractor for Dart source files.
pub struct DartExtractor {
    parser: Parser,
}

impl DartExtractor {
    /// Build a `DartExtractor` with a `Parser` configured for the bundled
    /// `tree_sitter_dart::LANGUAGE` grammar. Panics only if the grammar fails
    /// to load (bundled at compile time, so an infallible invariant in practice).
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_dart::LANGUAGE))
            .expect("Dart grammar should always load");
        Self { parser }
    }
}

impl Default for DartExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for DartExtractor {
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
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    match node.kind() {
        "class_declaration" | "mixin_declaration" | "extension_declaration" => {
            extract_class_like(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "enum_declaration" => {
            extract_enum(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "function_declaration" => {
            extract_function(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "type_alias" => {
            extract_type_alias(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "import_or_export" | "part_directive" => {
            extract_directive(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "top_level_variable_declaration" => {
            extract_top_level_var(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        _ => {
            for child in node.named_children(&mut node.walk()) {
                extract_node(
                    child,
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
}

// ── Class / mixin / extension ──

fn extract_class_like(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match find_name_identifier(node, source) {
        Some(n) => n,
        None => return,
    };

    let kind = match node.kind() {
        "mixin_declaration" => SymbolKind::Trait,
        "extension_declaration" => SymbolKind::Class,
        _ => SymbolKind::Class,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = dart_visibility(&name);
    let docstring = extract_doc_comment(node, source);
    let signature = build_signature_until_body(node, source);

    let sym_id = symbol_id(file_path, kind.as_str(), &name, parent_qname);
    let qname = qualified(parent_qname, &name);

    let mut sym = Symbol::new(
        name.clone(),
        kind,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent_qname,
    )
    .with_parent(parent_id)
    .with_docstring(docstring)
    .with_signature(signature);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Inheritance / mixin / interface edges (classes only).
    if node.kind() == "class_declaration" {
        extract_class_relations(node, source, file_path, &sym_id, start_line, edges);
    }

    // Walk the body for members. class_body wraps each member in a `class_member`
    // node; extension_body exposes members directly.
    if let Some(body) = find_body(node) {
        for child in body.named_children(&mut body.walk()) {
            if child.kind() == "class_member" {
                for inner in child.named_children(&mut child.walk()) {
                    extract_member(
                        inner,
                        source,
                        file_path,
                        Some(&sym_id),
                        Some(&qname),
                        symbols,
                        edges,
                    );
                }
            } else {
                extract_member(
                    child,
                    source,
                    file_path,
                    Some(&sym_id),
                    Some(&qname),
                    symbols,
                    edges,
                );
            }
        }
    }
}

fn extract_class_relations(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "superclass" => {
                // `extends X` (+ optional `with Y, Z`)
                for sub in child.named_children(&mut child.walk()) {
                    match sub.kind() {
                        "type" => {
                            if let Some(name) = extract_type_name(sub, source) {
                                edges.push(Edge::new(
                                    sym_id,
                                    name,
                                    EdgeKind::Inherits,
                                    file_path,
                                    line,
                                ));
                            }
                        }
                        "mixins" => {
                            for t in sub.named_children(&mut sub.walk()) {
                                if t.kind() == "type" {
                                    if let Some(name) = extract_type_name(t, source) {
                                        edges.push(Edge::new(
                                            sym_id,
                                            name,
                                            EdgeKind::Inherits,
                                            file_path,
                                            line,
                                        ));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "interfaces" => {
                for t in child.named_children(&mut child.walk()) {
                    if t.kind() == "type" {
                        if let Some(name) = extract_type_name(t, source) {
                            edges.push(Edge::new(
                                sym_id,
                                name,
                                EdgeKind::Implements,
                                file_path,
                                line,
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ── Class members ──

fn extract_member(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    match node.kind() {
        "method_declaration" => {
            extract_method_decl(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "declaration" => {
            // `declaration` wraps either a field (with initialized_identifier_list /
            // static_final_declaration_list), a constructor_signature, a getter/setter
            // signature, or an abstract method signature.
            extract_declaration(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        _ => {}
    }
}

fn extract_method_decl(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // Prefer the signature subtree (method_signature / function_signature)
    // so type-ref collection sees parameter and return types. Fall back to
    // the whole declaration only if no signature child exists.
    let sig = node
        .named_children(&mut node.walk())
        .find(|c| matches!(c.kind(), "method_signature" | "function_signature"))
        .unwrap_or(node);

    let (name, kind, is_async) = match method_name_kind(node, source) {
        Some(t) => t,
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = dart_visibility(&name);
    let docstring = extract_doc_comment(node, source);
    let signature = build_signature_until_body(node, source);

    let sym_id = symbol_id(file_path, kind.as_str(), &name, parent_qname);

    let mut sym = Symbol::new(
        name.clone(),
        kind,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent_qname,
    )
    .with_parent(parent_id)
    .with_docstring(docstring)
    .with_signature(signature)
    .with_async(is_async);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    collect_signature_type_refs(sig, source, file_path, &sym_id, edges);

    if let Some(body) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "function_body")
    {
        walk_for_calls(body, source, file_path, &sym_id, edges);
        walk_for_nested_decls(body, source, file_path, Some(&sym_id), None, symbols, edges);
    }
}

/// Determine the name, symbol kind, and async-ness of a method_declaration / declaration.
fn method_name_kind(node: Node, source: &str) -> Option<(String, SymbolKind, bool)> {
    // Look at the first signature child.
    let sig = node
        .named_children(&mut node.walk())
        .find(|c| matches!(c.kind(), "method_signature" | "function_signature"))?;

    let inner = sig.named_children(&mut sig.walk()).next().unwrap_or(sig);

    let (name, kind) = match inner.kind() {
        "function_signature" => {
            let id = inner
                .named_children(&mut inner.walk())
                .find(|c| c.kind() == "identifier")?;
            (node_text(id, source).to_string(), SymbolKind::Method)
        }
        "getter_signature" | "setter_signature" => {
            let id = inner
                .named_children(&mut inner.walk())
                .find(|c| c.kind() == "identifier")?;
            (node_text(id, source).to_string(), SymbolKind::Method)
        }
        "constructor_signature"
        | "factory_constructor_signature"
        | "constant_constructor_signature" => {
            // Default ctor: name = ClassName (matches Java convention, id becomes
            // `Class.Class`). Named ctor `Class.named`: name = `named`, parent
            // already qualifies, id becomes `Class.named`.
            let ids: Vec<_> = inner
                .named_children(&mut inner.walk())
                .filter(|c| c.kind() == "identifier")
                .collect();
            let name = if ids.len() >= 2 {
                node_text(ids[1], source).to_string()
            } else if let Some(id) = ids.first() {
                node_text(*id, source).to_string()
            } else {
                return None;
            };
            (name, SymbolKind::Method)
        }
        _ => {
            // method_signature with direct identifier (e.g. abstract methods)
            let id = sig
                .named_children(&mut sig.walk())
                .find(|c| c.kind() == "identifier")?;
            (node_text(id, source).to_string(), SymbolKind::Method)
        }
    };

    let body_text = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "function_body")
        .map(|b| node_text(b, source))
        .unwrap_or("");
    let is_async = body_text.trim_start().starts_with("async");

    Some((name, kind, is_async))
}

fn extract_declaration(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // Case 1: contains a constructor or function/getter/setter signature → method-shaped.
    let has_signature = node.named_children(&mut node.walk()).any(|c| {
        matches!(
            c.kind(),
            "function_signature"
                | "getter_signature"
                | "setter_signature"
                | "constructor_signature"
                | "factory_constructor_signature"
                | "constant_constructor_signature"
        )
    });
    if has_signature {
        // Build a fake method-shaped name.
        let name = decl_signature_name(node, source);
        if let Some(name) = name {
            let start_line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;
            let visibility = dart_visibility(&name);
            let docstring = extract_doc_comment(node, source);
            let signature = Some(node_text(node, source).trim().to_string());

            let sym_id = symbol_id(file_path, "method", &name, parent_qname);
            let mut sym = Symbol::new(
                name,
                SymbolKind::Method,
                file_path,
                start_line,
                end_line,
                node.start_byte() as u32,
                node.end_byte() as u32,
                parent_qname,
            )
            .with_parent(parent_id)
            .with_docstring(docstring)
            .with_signature(signature);
            if visibility != Visibility::Public {
                sym = sym.with_visibility(visibility);
            }
            symbols.push(sym);

            // Collect type refs from the signature subtree only, not the body —
            // body type_identifiers (local var annotations, generic args in
            // expressions) are not signature references.
            for child in node.named_children(&mut node.walk()) {
                if matches!(
                    child.kind(),
                    "function_signature"
                        | "getter_signature"
                        | "setter_signature"
                        | "constructor_signature"
                        | "factory_constructor_signature"
                        | "constant_constructor_signature"
                ) {
                    collect_signature_type_refs(child, source, file_path, &sym_id, edges);
                }
            }

            // Walk the body for Calls edges and nested local declarations.
            if let Some(body) = node
                .named_children(&mut node.walk())
                .find(|c| c.kind() == "function_body")
            {
                walk_for_calls(body, source, file_path, &sym_id, edges);
                walk_for_nested_decls(body, source, file_path, Some(&sym_id), None, symbols, edges);
            }
        }
        return;
    }

    // Case 2: field declaration → emit Variable per identifier, plus Calls
    // for the initializer expression of each spec.
    let line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "initialized_identifier_list" | "static_final_declaration_list" => {
                for spec in child.named_children(&mut child.walk()) {
                    let id = spec
                        .named_children(&mut spec.walk())
                        .find(|c| c.kind() == "identifier");
                    if let Some(id) = id {
                        let name = node_text(id, source).to_string();
                        push_variable(
                            id,
                            source,
                            file_path,
                            parent_id,
                            parent_qname,
                            line,
                            end_line,
                            symbols,
                        );
                        let ctx = symbol_id(file_path, "variable", &name, parent_qname);
                        walk_for_calls(spec, source, file_path, &ctx, edges);
                    }
                }
            }
            _ => {}
        }
    }
}

fn decl_signature_name(node: Node, source: &str) -> Option<String> {
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "function_signature" | "getter_signature" | "setter_signature" => {
                let id = child
                    .named_children(&mut child.walk())
                    .find(|c| c.kind() == "identifier")?;
                return Some(node_text(id, source).to_string());
            }
            "constructor_signature"
            | "factory_constructor_signature"
            | "constant_constructor_signature" => {
                let ids: Vec<_> = child
                    .named_children(&mut child.walk())
                    .filter(|c| c.kind() == "identifier")
                    .collect();
                if ids.len() >= 2 {
                    return Some(node_text(ids[1], source).to_string());
                }
                if let Some(id) = ids.first() {
                    return Some(node_text(*id, source).to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn push_variable(
    id_node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    line: u32,
    end_line: u32,
    symbols: &mut Vec<Symbol>,
) {
    let name = node_text(id_node, source).to_string();
    if name.is_empty() {
        return;
    }
    let visibility = dart_visibility(&name);
    let mut sym = Symbol::new(
        name,
        SymbolKind::Variable,
        file_path,
        line,
        end_line,
        id_node.start_byte() as u32,
        id_node.end_byte() as u32,
        parent_qname,
    )
    .with_parent(parent_id);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);
}

// ── Top-level function ──

fn extract_function(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let sig = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "function_signature");
    let sig = match sig {
        Some(s) => s,
        None => return,
    };
    let name_node = sig
        .named_children(&mut sig.walk())
        .find(|c| c.kind() == "identifier");
    let name = match name_node {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = dart_visibility(&name);
    let docstring = extract_doc_comment(node, source);
    let signature = Some(node_text(sig, source).trim().to_string());
    let body = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "function_body");
    let is_async = body
        .map(|b| node_text(b, source).trim_start().starts_with("async"))
        .unwrap_or(false);

    let sym_id = symbol_id(file_path, "function", &name, parent_qname);
    let mut sym = Symbol::new(
        name,
        SymbolKind::Function,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent_qname,
    )
    .with_parent(parent_id)
    .with_docstring(docstring)
    .with_signature(signature)
    .with_async(is_async);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    collect_signature_type_refs(sig, source, file_path, &sym_id, edges);
    if let Some(b) = body {
        walk_for_calls(b, source, file_path, &sym_id, edges);
        walk_for_nested_decls(b, source, file_path, Some(&sym_id), None, symbols, edges);
    }
}

// ── Enum ──

fn extract_enum(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match find_name_identifier(node, source) {
        Some(n) => n,
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = dart_visibility(&name);
    let docstring = extract_doc_comment(node, source);
    let signature = build_signature_until_body(node, source);

    let sym_id = symbol_id(file_path, "enum", &name, parent_qname);
    let qname = qualified(parent_qname, &name);

    let mut sym = Symbol::new(
        name.clone(),
        SymbolKind::Enum,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent_qname,
    )
    .with_parent(parent_id)
    .with_docstring(docstring)
    .with_signature(signature);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    if let Some(body) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "enum_body")
    {
        for child in body.named_children(&mut body.walk()) {
            match child.kind() {
                "enum_constant" => {
                    let id = child
                        .named_children(&mut child.walk())
                        .find(|c| c.kind() == "identifier");
                    if let Some(id) = id {
                        push_variable(
                            id,
                            source,
                            file_path,
                            Some(&sym_id),
                            Some(&qname),
                            child.start_position().row as u32 + 1,
                            child.end_position().row as u32 + 1,
                            symbols,
                        );
                    }
                }
                "class_member" => {
                    for inner in child.named_children(&mut child.walk()) {
                        extract_member(
                            inner,
                            source,
                            file_path,
                            Some(&sym_id),
                            Some(&qname),
                            symbols,
                            edges,
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

// ── Typedef ──

fn extract_type_alias(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name_node = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "type_identifier");
    let name = match name_node {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = dart_visibility(&name);
    let signature = Some(node_text(node, source).trim().to_string());

    let sym_id = symbol_id(file_path, "type_alias", &name, parent_qname);
    let mut sym = Symbol::new(
        name,
        SymbolKind::TypeAlias,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent_qname,
    )
    .with_parent(parent_id)
    .with_signature(signature);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Emit References for every type_identifier on the RHS of the alias
    // (e.g. `typedef Handler = void Function(Request)` → References Request).
    // The LHS type_identifier is the alias name we already named; skip it.
    for child in node.named_children(&mut node.walk()) {
        if child.kind() != "type_identifier" {
            collect_signature_type_refs(child, source, file_path, &sym_id, edges);
        }
    }
}

// ── Imports / exports / part ──

fn extract_directive(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    let uri = match find_uri(node, source) {
        Some(u) => u,
        None => return,
    };
    let signature = Some(node_text(node, source).trim().to_string());
    let pkg_name = uri_short_name(&uri);

    let sym_id = symbol_id(file_path, "import", &uri, parent_qname);
    symbols.push(
        Symbol::new(
            uri.clone(),
            SymbolKind::Import,
            file_path,
            line,
            line,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_signature(signature),
    );

    edges.push(Edge::new(
        sym_id,
        pkg_name,
        EdgeKind::Imports,
        file_path,
        line,
    ));
}

fn find_uri(node: Node, source: &str) -> Option<String> {
    fn walk(n: Node, source: &str) -> Option<String> {
        if n.kind() == "uri" {
            return Some(strip_quotes(node_text(n, source)));
        }
        for child in n.named_children(&mut n.walk()) {
            if let Some(s) = walk(child, source) {
                return Some(s);
            }
        }
        None
    }
    walk(node, source)
}

/// Strip surrounding Dart string quotes and an optional raw-string `r`/`R` prefix.
/// Handles single, double, and triple-quoted strings.
fn strip_quotes(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix('r')
        .or_else(|| s.strip_prefix('R'))
        .unwrap_or(s);
    for q in ["'''", "\"\"\""] {
        if let Some(rest) = s.strip_prefix(q) {
            return rest.strip_suffix(q).unwrap_or(rest).to_string();
        }
    }
    for q in ['\'', '"'] {
        if let Some(rest) = s.strip_prefix(q) {
            return rest.strip_suffix(q).unwrap_or(rest).to_string();
        }
    }
    s.to_string()
}

/// Pick a short import target name (last path segment, without extension).
fn uri_short_name(uri: &str) -> String {
    let last = last_segment(uri, "/");
    let stem = last.strip_suffix(".dart").unwrap_or(last);
    if let Some(rest) = uri.strip_prefix("package:") {
        // For `package:foo/bar.dart`, prefer the package name `foo`.
        if let Some(pkg) = rest.split('/').next() {
            return pkg.to_string();
        }
    }
    if let Some(rest) = uri.strip_prefix("dart:") {
        return rest.to_string();
    }
    stem.to_string()
}

// ── Top-level variable ──

fn extract_top_level_var(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "initialized_identifier_list" | "static_final_declaration_list" => {
                for spec in child.named_children(&mut child.walk()) {
                    let id = spec
                        .named_children(&mut spec.walk())
                        .find(|c| c.kind() == "identifier");
                    if let Some(id) = id {
                        let name = node_text(id, source).to_string();
                        push_variable(
                            id,
                            source,
                            file_path,
                            parent_id,
                            parent_qname,
                            line,
                            end_line,
                            symbols,
                        );
                        // Walk only this spec's initializer for Calls, so multi-
                        // declarator decls (`var a = f1(), b = f2();`) attribute
                        // each call to its own variable.
                        let ctx = symbol_id(file_path, "variable", &name, parent_qname);
                        walk_for_calls(spec, source, file_path, &ctx, edges);
                    }
                }
            }
            _ => {}
        }
    }
}

// ── Calls ──

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
                "call_expression" => {
                    if let Some(func) = current.named_children(&mut current.walk()).next() {
                        let callee_name = node_text(func, source).to_string();
                        if !callee_name.is_empty() {
                            edges.push(Edge::new(
                                context_id,
                                callee_name,
                                EdgeKind::Calls,
                                file_path,
                                current.start_position().row as u32 + 1,
                            ));
                        }
                    }
                }
                // Don't descend into nested function bodies — calls there belong
                // to the nested symbol (emitted separately by walk_for_nested_decls).
                "local_function_declaration" | "function_declaration"
                    if current.id() != node.id() =>
                {
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
            if cursor.node().id() == node.id() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Recursively find nested `local_function_declaration` / `function_declaration`
/// nodes inside a function body and emit them as Method symbols. Calls within
/// each nested function are attributed to the nested symbol, not the enclosing one.
fn walk_for_nested_decls(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "local_function_declaration" | "function_declaration" => {
                let sig = child
                    .named_children(&mut child.walk())
                    .find(|c| c.kind() == "function_signature");
                let sig = match sig {
                    Some(s) => s,
                    None => continue,
                };
                let name_node = sig
                    .named_children(&mut sig.walk())
                    .find(|c| c.kind() == "identifier");
                let name = match name_node {
                    Some(n) => node_text(n, source).to_string(),
                    None => continue,
                };

                let start_line = child.start_position().row as u32 + 1;
                let end_line = child.end_position().row as u32 + 1;
                let visibility = dart_visibility(&name);
                let signature = Some(node_text(sig, source).trim().to_string());
                let body = child
                    .named_children(&mut child.walk())
                    .find(|c| c.kind() == "function_body");
                let is_async = body
                    .map(|b| node_text(b, source).trim_start().starts_with("async"))
                    .unwrap_or(false);

                let sym_id = symbol_id(file_path, "method", &name, parent_qname);
                let mut sym = Symbol::new(
                    name,
                    SymbolKind::Method,
                    file_path,
                    start_line,
                    end_line,
                    child.start_byte() as u32,
                    child.end_byte() as u32,
                    parent_qname,
                )
                .with_parent(parent_id)
                .with_signature(signature)
                .with_async(is_async);
                if visibility != Visibility::Public {
                    sym = sym.with_visibility(visibility);
                }
                symbols.push(sym);

                collect_signature_type_refs(sig, source, file_path, &sym_id, edges);
                if let Some(b) = body {
                    walk_for_calls(b, source, file_path, &sym_id, edges);
                    walk_for_nested_decls(
                        b,
                        source,
                        file_path,
                        Some(&sym_id),
                        None,
                        symbols,
                        edges,
                    );
                }
            }
            _ => {
                walk_for_nested_decls(
                    child,
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
}

// ── Type references ──

fn collect_signature_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    fn walk(n: Node, source: &str, file_path: &str, sym_id: &str, edges: &mut Vec<Edge>) {
        // Skip generic-parameter bindings: `class Foo<T>` declares `T`, not refs it.
        if matches!(n.kind(), "type_parameters" | "type_parameter") {
            return;
        }
        if n.kind() == "type_identifier" {
            let name = node_text(n, source);
            if !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_uppercase() || c == '_')
                && !is_builtin_dart_type(name)
            {
                edges.push(Edge::new(
                    sym_id,
                    name,
                    EdgeKind::References,
                    file_path,
                    n.start_position().row as u32 + 1,
                ));
            }
        }
        for child in n.named_children(&mut n.walk()) {
            walk(child, source, file_path, sym_id, edges);
        }
    }
    walk(node, source, file_path, sym_id, edges);
}

fn is_builtin_dart_type(name: &str) -> bool {
    matches!(
        name,
        // Primitives + top types
        "int"
            | "double"
            | "num"
            | "bool"
            | "void"
            | "dynamic"
            | "String"
            | "Object"
            | "Null"
            | "Never"
            | "Symbol"
            | "Type"
            | "Function"
            | "Record"
            | "Enum"
            // Async / collections
            | "Future"
            | "FutureOr"
            | "Stream"
            | "Iterable"
            | "Iterator"
            | "List"
            | "Map"
            | "Set"
            // Error / debug types
            | "Exception"
            | "Error"
            | "StackTrace"
            // Common SDK types used in signatures
            | "Comparable"
            | "Comparator"
            | "Pattern"
            | "Match"
            | "RegExp"
            | "Duration"
            | "DateTime"
            | "Uri"
    )
}

// ── Helpers ──

fn dart_visibility(name: &str) -> Visibility {
    if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

fn qualified(parent_qname: Option<&str>, name: &str) -> String {
    match parent_qname {
        Some(p) => format!("{p}.{name}"),
        None => name.to_string(),
    }
}

fn find_name_identifier(node: Node, source: &str) -> Option<String> {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "identifier" {
            return Some(node_text(child, source).to_string());
        }
    }
    None
}

fn find_body(node: Node) -> Option<Node> {
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "class_body" | "extension_body" | "enum_body" => return Some(child),
            _ => {}
        }
    }
    None
}

fn extract_type_name(node: Node, source: &str) -> Option<String> {
    if node.kind() == "type" {
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "type_identifier" {
                let text = node_text(child, source).to_string();
                if text.is_empty() {
                    return None;
                }
                return Some(text);
            }
        }
    }
    let text = node_text(node, source).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Slice of the node up to (but not including) its body, used as a signature.
fn build_signature_until_body(node: Node, source: &str) -> Option<String> {
    let body = find_body(node).or_else(|| {
        node.named_children(&mut node.walk())
            .find(|c| c.kind() == "function_body" || c.kind() == "enum_body")
    });
    let end = body
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let slice = source.get(node.start_byte()..end).unwrap_or("").trim();
    if slice.is_empty() {
        None
    } else {
        Some(slice.to_string())
    }
}

/// Collect preceding `///` doc comment lines.
fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "comment" {
            let text = node_text(p, source);
            if let Some(stripped) = text.strip_prefix("///") {
                lines.push(stripped.trim().to_string());
            } else {
                break;
            }
        } else {
            break;
        }
        prev = p.prev_sibling();
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
        let mut ext = DartExtractor::new();
        ext.extract(source, "test.dart").unwrap()
    }

    #[test]
    fn test_top_level_function() {
        let result = extract(
            r#"
/// Greets a user.
String greet(String name) => 'hi $name';
"#,
        );
        let f = result.symbols.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(f.kind, SymbolKind::Function);
        assert_eq!(f.visibility, Visibility::Public);
        assert_eq!(f.docstring.as_deref(), Some("Greets a user."));
        assert!(f.signature.as_deref().unwrap().contains("String greet"));
    }

    #[test]
    fn test_private_function() {
        let result = extract("void _helper() {}");
        let f = result.symbols.iter().find(|s| s.name == "_helper").unwrap();
        assert_eq!(f.visibility, Visibility::Private);
    }

    #[test]
    fn test_async_function() {
        let result = extract(
            r#"
Future<int> fetch() async {
  return 1;
}
"#,
        );
        let f = result.symbols.iter().find(|s| s.name == "fetch").unwrap();
        assert!(f.is_async);
    }

    #[test]
    fn test_class_and_method() {
        let result = extract(
            r#"
/// A dog.
class Dog {
  final String name;
  Dog(this.name);
  void speak() { print('bark'); }
}
"#,
        );
        let c = result.symbols.iter().find(|s| s.name == "Dog").unwrap();
        assert_eq!(c.kind, SymbolKind::Class);
        assert_eq!(c.docstring.as_deref(), Some("A dog."));

        let speak = result.symbols.iter().find(|s| s.name == "speak").unwrap();
        assert_eq!(speak.kind, SymbolKind::Method);
        assert!(speak.parent_id.as_ref().unwrap().contains("Dog"));

        let field = result.symbols.iter().find(|s| s.name == "name").unwrap();
        assert_eq!(field.kind, SymbolKind::Variable);
    }

    #[test]
    fn test_constructor_named_and_factory() {
        let result = extract(
            r#"
class Dog {
  Dog();
  Dog.named();
  factory Dog.empty() => Dog();
}
"#,
        );
        let names: Vec<&str> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"Dog"), "default ctor: names={names:?}");
        assert!(names.contains(&"named"), "named ctor: names={names:?}");
        assert!(names.contains(&"empty"), "factory ctor: names={names:?}");

        // Stable IDs: ids are `<file>:method:<class>.<suffix>` (default ctor uses
        // the class name as the suffix, matching Java's `Class.Class`).
        let ids: Vec<&str> = result.symbols.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"test.dart:method:Dog.Dog"));
        assert!(ids.contains(&"test.dart:method:Dog.named"));
        assert!(ids.contains(&"test.dart:method:Dog.empty"));
    }

    #[test]
    fn test_extends_implements_with() {
        let result = extract(
            r#"
class Dog extends Animal with Logger implements Comparable<Dog> {}
"#,
        );
        let inherits: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(inherits.contains(&"Animal"));
        assert!(inherits.contains(&"Logger"));

        let implements: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(implements.contains(&"Comparable"));
    }

    #[test]
    fn test_mixin_and_extension() {
        let result = extract(
            r#"
mixin Logger {
  void log(String s) {}
}

extension StringExt on String {
  String shout() => toUpperCase();
}
"#,
        );
        let m = result.symbols.iter().find(|s| s.name == "Logger").unwrap();
        assert_eq!(m.kind, SymbolKind::Trait);
        let e = result
            .symbols
            .iter()
            .find(|s| s.name == "StringExt")
            .unwrap();
        assert_eq!(e.kind, SymbolKind::Class);

        let shout = result.symbols.iter().find(|s| s.name == "shout").unwrap();
        assert_eq!(shout.kind, SymbolKind::Method);
    }

    #[test]
    fn test_enum() {
        let result = extract(
            r#"
enum Color { red, green, blue }
"#,
        );
        let e = result.symbols.iter().find(|s| s.name == "Color").unwrap();
        assert_eq!(e.kind, SymbolKind::Enum);

        let consts: Vec<&str> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Variable)
            .map(|s| s.name.as_str())
            .collect();
        assert!(consts.contains(&"red"));
        assert!(consts.contains(&"green"));
        assert!(consts.contains(&"blue"));
    }

    #[test]
    fn test_typedef() {
        let result = extract("typedef IntList = List<int>;");
        let t = result.symbols.iter().find(|s| s.name == "IntList").unwrap();
        assert_eq!(t.kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn test_typedef_emits_type_refs() {
        let result = extract("typedef Handler = void Function(Request req, User u);");
        let alias = result.symbols.iter().find(|s| s.name == "Handler").unwrap();
        let ref_targets: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References && e.source_id == alias.id)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(ref_targets.contains(&"Request"), "{ref_targets:?}");
        assert!(ref_targets.contains(&"User"), "{ref_targets:?}");
    }

    #[test]
    fn test_field_initializer_calls() {
        let result = extract(
            r#"
class Api {
  final client = buildClient();
}
"#,
        );
        let field = result.symbols.iter().find(|s| s.name == "client").unwrap();
        let call = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "buildClient")
            .expect("buildClient call");
        assert_eq!(call.source_id, field.id);
    }

    #[test]
    fn test_imports_and_uri_name() {
        let result = extract(
            r#"
import 'package:flutter/material.dart';
import 'dart:async';
import 'utils.dart';
"#,
        );
        let imports: Vec<&Symbol> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 3);

        let targets: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(targets.contains(&"flutter"));
        assert!(targets.contains(&"async"));
        assert!(targets.contains(&"utils"));
    }

    #[test]
    fn test_top_level_var() {
        let result = extract(
            r#"
const int maxRetries = 3;
final logger = createLogger();
var counter = 0;
"#,
        );
        let names: Vec<&str> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Variable)
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"maxRetries"));
        assert!(names.contains(&"logger"));
        assert!(names.contains(&"counter"));

        let calls: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(calls.contains(&"createLogger"));
    }

    #[test]
    fn test_function_calls() {
        let result = extract(
            r#"
void process() {
  final data = fetchData();
  save(transform(data));
}
"#,
        );
        let calls: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(calls.contains(&"fetchData"));
        assert!(calls.contains(&"transform"));
        assert!(calls.contains(&"save"));
    }

    #[test]
    fn test_type_refs() {
        let result = extract(
            r#"
Response handle(User u, int n) { return Response(); }
"#,
        );
        let refs: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(refs.contains(&"User"));
        assert!(refs.contains(&"Response"));
        assert!(!refs.contains(&"int"));
    }

    #[test]
    fn test_visibility() {
        let result = extract(
            r#"
class _Internal {}
class Public {}
void _hidden() {}
void shown() {}
"#,
        );
        let internal = result
            .symbols
            .iter()
            .find(|s| s.name == "_Internal")
            .unwrap();
        assert_eq!(internal.visibility, Visibility::Private);
        let public = result.symbols.iter().find(|s| s.name == "Public").unwrap();
        assert_eq!(public.visibility, Visibility::Public);
        let hidden = result.symbols.iter().find(|s| s.name == "_hidden").unwrap();
        assert_eq!(hidden.visibility, Visibility::Private);
        let shown = result.symbols.iter().find(|s| s.name == "shown").unwrap();
        assert_eq!(shown.visibility, Visibility::Public);
    }

    #[test]
    fn test_getter_setter() {
        let result = extract(
            r#"
class Box {
  int _v = 0;
  int get value => _v;
  set value(int x) { _v = x; }
}
"#,
        );
        let getter = result
            .symbols
            .iter()
            .find(|s| s.name == "value" && s.kind == SymbolKind::Method);
        assert!(getter.is_some(), "expected `value` method (getter/setter)");
    }

    #[test]
    fn test_nested_local_function() {
        let result = extract(
            r#"
void outer() {
  void inner() {
    log('hi');
  }
  inner();
}
"#,
        );
        // Both outer and inner are emitted.
        let outer = result.symbols.iter().find(|s| s.name == "outer");
        assert!(outer.is_some());
        let inner = result.symbols.iter().find(|s| s.name == "inner");
        assert!(inner.is_some(), "nested local function should be a symbol");

        // The call `log(...)` is attributed to `inner`, not `outer`.
        let inner_id = &inner.unwrap().id;
        let log_call = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "log");
        assert!(log_call.is_some());
        assert_eq!(&log_call.unwrap().source_id, inner_id);
    }

    #[test]
    fn test_multi_declarator_var_call_attribution() {
        let result = extract("var a = first(), b = second();");
        let a = result.symbols.iter().find(|s| s.name == "a").unwrap();
        let b = result.symbols.iter().find(|s| s.name == "b").unwrap();

        let first_call = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "first")
            .expect("first() call edge");
        let second_call = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "second")
            .expect("second() call edge");

        assert_eq!(&first_call.source_id, &a.id, "first() should belong to a");
        assert_eq!(&second_call.source_id, &b.id, "second() should belong to b");
    }

    #[test]
    fn test_generic_type_parameter_declaration_not_emitted_as_ref() {
        // `T` in `<T>` is a binding; later uses (e.g. in `Future<T?>`) are
        // real refs and should still be emitted. We assert that a class with
        // generics but NO use of T in any member emits zero refs to T.
        let result = extract("abstract class Bag<T> { void noop(); }");
        let refs_to_t: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References && e.target_name == "T")
            .collect();
        assert!(
            refs_to_t.is_empty(),
            "generic param declaration T should not be a References target: {refs_to_t:?}"
        );
    }

    #[test]
    fn test_builtin_types_not_emitted() {
        let result = extract(
            r#"
Duration delay(DateTime t, Uri u, Exception? e) {
  return Duration.zero;
}
"#,
        );
        let ref_targets: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(!ref_targets.contains(&"Duration"));
        assert!(!ref_targets.contains(&"DateTime"));
        assert!(!ref_targets.contains(&"Uri"));
        assert!(!ref_targets.contains(&"Exception"));
    }

    #[test]
    fn test_raw_string_import() {
        let result = extract(r#"import r'package:foo/bar.dart';"#);
        let imp = result
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .expect("import symbol");
        assert_eq!(imp.name, "package:foo/bar.dart");

        let target = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert_eq!(target.target_name, "foo");
    }

    #[test]
    fn test_empty_file() {
        let result = extract("");
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn test_syntax_error_partial_parse() {
        let result = extract("class Broken {");
        let _ = result.symbols.len();
    }
}
