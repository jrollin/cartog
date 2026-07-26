use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use cartog_core::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{last_segment, node_text, ExtractionResult, Extractor};

pub struct RustExtractor {
    parser: Parser,
}

impl RustExtractor {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_rust::LANGUAGE))
            .expect("Rust grammar should always load");
        Self { parser }
    }
}

impl Default for RustExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for RustExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        let tree = crate::parse_bounded(&mut self.parser, source)
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
    crate::parse::guard_recursion!();
    match node.kind() {
        "function_item" => {
            extract_function(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                false,
                symbols,
                edges,
            );
        }
        "struct_item" => {
            extract_struct(node, source, file_path, parent_id, parent_qname, symbols);
        }
        "enum_item" => {
            extract_enum(node, source, file_path, parent_id, parent_qname, symbols);
        }
        "trait_item" => {
            extract_trait(node, source, file_path, parent_id, parent_qname, symbols);
        }
        "impl_item" => {
            extract_impl(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "use_declaration" => {
            extract_use(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "mod_item" => {
            extract_mod(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "const_item" | "static_item" => {
            extract_const(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "type_item" => {
            extract_type_alias(node, source, file_path, parent_id, parent_qname, symbols);
        }
        "macro_definition" => {
            extract_macro_definition(node, source, file_path, parent_id, parent_qname, symbols);
        }
        "attribute_item" | "inner_attribute_item" => {
            // Skip attributes, but process the next sibling
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

// ── Functions ──

#[allow(clippy::too_many_arguments)]
fn extract_function(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    is_method: bool,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let kind = if is_method {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);
    let is_async = has_child_kind(node, "async");
    let signature = extract_fn_signature(node, source);
    let docstring = extract_doc_comment(node, source);
    let is_test = has_test_attribute(node, source) || is_inside_cfg_test_mod(node, source);

    let sym_id = symbol_id(file_path, kind, &name, parent_qname);
    symbols.push(
        Symbol::new(
            name,
            kind,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_signature(signature)
        .with_visibility(visibility)
        .with_async(is_async)
        .with_test(is_test)
        .with_docstring(docstring),
    );

    // Extract type references from parameter and return types
    extract_fn_type_refs(node, source, file_path, &sym_id, edges);

    // Walk body for calls
    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, &sym_id, edges);
    }
}

// ── Structs ──

fn extract_struct(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, SymbolKind::Class, &name, parent_qname);
    symbols.push(
        Symbol::new(
            name.clone(),
            SymbolKind::Class,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_visibility(visibility)
        .with_docstring(docstring),
    );

    let struct_qname = match parent_qname {
        Some(pq) => format!("{pq}.{name}"),
        None => name,
    };

    // Tuple-struct fields are positional (`field_declaration_list` absent) — no useful name to emit.
    if let Some(body) = node.child_by_field_name("body") {
        if body.kind() == "field_declaration_list" {
            for field in body.named_children(&mut body.walk()) {
                if field.kind() == "field_declaration" {
                    extract_struct_field(field, source, file_path, &sym_id, &struct_qname, symbols);
                }
            }
        }
    }
}

fn extract_struct_field(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    parent_qname: &str,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);
    let signature = node
        .child_by_field_name("type")
        .map(|t| node_text(t, source).to_string());

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Variable,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            Some(parent_qname),
        )
        .with_parent(Some(parent_id))
        .with_signature(signature)
        .with_visibility(visibility),
    );
}

// ── Enums ──

fn extract_enum(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, SymbolKind::Enum, &name, parent_qname);
    symbols.push(
        Symbol::new(
            name.clone(),
            SymbolKind::Enum,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_visibility(visibility)
        .with_docstring(docstring),
    );

    let enum_qname = match parent_qname {
        Some(pq) => format!("{pq}.{name}"),
        None => name,
    };

    if let Some(body) = node.child_by_field_name("body") {
        for variant in body.named_children(&mut body.walk()) {
            if variant.kind() == "enum_variant" {
                extract_enum_variant(
                    variant,
                    source,
                    file_path,
                    &sym_id,
                    &enum_qname,
                    visibility,
                    symbols,
                );
            }
        }
    }
}

fn extract_enum_variant(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    parent_qname: &str,
    enum_visibility: Visibility,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let docstring = extract_doc_comment(node, source);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::EnumMember,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            Some(parent_qname),
        )
        .with_parent(Some(parent_id))
        // A variant has no visibility of its own — it inherits the enum's.
        .with_visibility(enum_visibility)
        .with_docstring(docstring),
    );
}

// ── Traits ──

fn extract_trait(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Trait,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_visibility(visibility)
        .with_docstring(docstring),
    );
}

// ── Impl blocks ──

fn extract_impl(
    node: Node,
    source: &str,
    file_path: &str,
    _parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // Get the type being implemented
    let impl_type = match node.child_by_field_name("type") {
        Some(n) => extract_type_name(n, source),
        None => return,
    };

    if impl_type.is_empty() {
        return;
    }

    let start_line = node.start_position().row as u32 + 1;

    // Try to reuse the existing struct/enum/trait symbol as parent.
    // Filter to type-definition kinds to avoid matching a function with the same name.
    let existing_parent = symbols
        .iter()
        .find(|s| {
            s.name == impl_type
                && s.file_path == file_path
                && matches!(
                    s.kind,
                    SymbolKind::Class | SymbolKind::Enum | SymbolKind::Trait
                )
        })
        .map(|s| s.id.clone());

    let impl_parent_id = existing_parent.unwrap_or_else(|| {
        // No prior struct/enum/trait — emit a Class symbol so edges have a valid source_id.
        // This happens for impl blocks on types defined in other files.
        let id = symbol_id(file_path, SymbolKind::Class, &impl_type, parent_qname);
        let end_line = node.end_position().row as u32 + 1;
        symbols.push(Symbol::new(
            impl_type.clone(),
            SymbolKind::Class,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        ));
        id
    });

    // Check if this is a trait impl: impl Trait for Type
    let trait_name = node
        .child_by_field_name("trait")
        .map(|t| extract_type_name(t, source));

    if let Some(ref trait_n) = trait_name {
        if !trait_n.is_empty() {
            edges.push(Edge::new(
                impl_parent_id.clone(),
                trait_n.clone(),
                EdgeKind::Implements,
                file_path,
                start_line,
            ));
        }
    }

    // The qualified name for methods inside this impl is the impl type name
    let impl_qname = match parent_qname {
        Some(pq) => format!("{pq}.{impl_type}"),
        None => impl_type.clone(),
    };

    // Walk impl body for methods
    if let Some(body) = node.child_by_field_name("body") {
        for child in body.named_children(&mut body.walk()) {
            if child.kind() == "function_item" {
                extract_function(
                    child,
                    source,
                    file_path,
                    Some(&impl_parent_id),
                    Some(&impl_qname),
                    true,
                    symbols,
                    edges,
                );
            }
        }
    }
}

// ── Use declarations ──

fn extract_use(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    let import_text = node_text(node, source).to_string();

    // Get the path from the use declaration
    let use_path = extract_use_path(node, source);
    if use_path.is_empty() {
        return;
    }

    let sym_id = symbol_id(file_path, SymbolKind::Import, &use_path, parent_qname);
    symbols.push(
        Symbol::new(
            use_path.clone(),
            SymbolKind::Import,
            file_path,
            line,
            line,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_signature(Some(import_text)),
    );

    // Collect imported names from the use tree
    let names = collect_use_names(node, source);
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

fn extract_use_path(node: Node, source: &str) -> String {
    // use foo::bar::Baz  →  "foo::bar"
    // use foo::bar::{A, B}  →  "foo::bar"
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "use_as_clause" | "scoped_identifier" | "identifier" | "use_wildcard"
            | "scoped_use_list" | "use_list" => {
                return extract_path_prefix(child, source);
            }
            _ => {}
        }
    }
    String::new()
}

fn extract_path_prefix(node: Node, source: &str) -> String {
    crate::parse::guard_recursion!(String::new());
    match node.kind() {
        "scoped_identifier" => {
            // foo::bar::Baz — get "foo::bar"
            if let Some(path) = node.child_by_field_name("path") {
                return node_text(path, source).to_string();
            }
            node_text(node, source).to_string()
        }
        "scoped_use_list" => {
            // foo::bar::{A, B} — get "foo::bar"
            if let Some(path) = node.child_by_field_name("path") {
                return node_text(path, source).to_string();
            }
            String::new()
        }
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                return extract_path_prefix(path, source);
            }
            String::new()
        }
        "identifier" => node_text(node, source).to_string(),
        _ => node_text(node, source).to_string(),
    }
}

fn collect_use_names(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    collect_use_names_recursive(node, source, &mut names);
    names
}

fn collect_use_names_recursive(node: Node, source: &str, names: &mut Vec<String>) {
    crate::parse::guard_recursion!();
    match node.kind() {
        "use_as_clause" => {
            // use foo::Bar as Baz  →  collect "Bar"
            if let Some(path) = node.child_by_field_name("path") {
                let name = last_path_segment(path, source);
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
        "scoped_identifier" => {
            // use foo::Bar  →  collect "Bar"
            if let Some(name) = node.child_by_field_name("name") {
                names.push(node_text(name, source).to_string());
            }
        }
        "identifier" => {
            names.push(node_text(node, source).to_string());
        }
        "use_wildcard" => {
            // use foo::*  →  nothing specific to collect
        }
        "scoped_use_list" | "use_list" => {
            // use foo::{A, B}  →  recurse into children
            for child in node.named_children(&mut node.walk()) {
                collect_use_names_recursive(child, source, names);
            }
        }
        "use_declaration" => {
            for child in node.named_children(&mut node.walk()) {
                collect_use_names_recursive(child, source, names);
            }
        }
        _ => {}
    }
}

fn last_path_segment(node: Node, source: &str) -> String {
    let text = node_text(node, source);
    last_segment(text, "::").to_string()
}

// ── Mod items ──

fn extract_mod(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);

    let sym_id = symbol_id(file_path, SymbolKind::Module, &name, parent_qname);

    // Only emit a symbol if it has a body (inline module)
    if let Some(body) = node.child_by_field_name("body") {
        symbols.push(
            Symbol::new(
                name.clone(),
                SymbolKind::Module,
                file_path,
                start_line,
                node.end_position().row as u32 + 1,
                node.start_byte() as u32,
                node.end_byte() as u32,
                parent_qname,
            )
            .with_parent(parent_id)
            .with_visibility(visibility),
        );

        let child_qname = match parent_qname {
            Some(pq) => format!("{pq}.{name}"),
            None => name.clone(),
        };

        for child in body.named_children(&mut body.walk()) {
            extract_node(
                child,
                source,
                file_path,
                Some(&sym_id),
                Some(&child_qname),
                symbols,
                edges,
            );
        }
    }
}

// ── Constants / statics ──

fn extract_const(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);
    let docstring = extract_doc_comment(node, source);
    let sym_id = symbol_id(file_path, SymbolKind::Variable, &name, parent_qname);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Variable,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_visibility(visibility)
        .with_docstring(docstring),
    );

    // Walk initializer expression for calls
    if let Some(value) = node.child_by_field_name("value") {
        walk_for_calls(value, source, file_path, &sym_id, edges);
    }
}

// ── Macro definitions ──

fn extract_macro_definition(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Macro,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_visibility(visibility)
        .with_docstring(docstring),
    );
}

// ── Type aliases ──

fn extract_type_alias(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let visibility = rust_visibility(node, source);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::TypeAlias,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_parent(parent_id)
        .with_visibility(visibility),
    );
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
                "call_expression" => {
                    if let Some(func) = current.child_by_field_name("function") {
                        let callee_name = node_text(func, source);
                        if !callee_name.is_empty() {
                            edges.push(Edge::new(
                                context_id.to_string(),
                                callee_name.to_string(),
                                EdgeKind::Calls,
                                file_path,
                                current.start_position().row as u32 + 1,
                            ));
                        }
                    }
                }
                "macro_invocation" => {
                    // e.g. println!(), vec![]
                    if let Some(mac) = current.child_by_field_name("macro") {
                        let macro_name = node_text(mac, source);
                        if !macro_name.is_empty() {
                            edges.push(Edge::new(
                                context_id.to_string(),
                                format!("{macro_name}!"),
                                EdgeKind::Calls,
                                file_path,
                                current.start_position().row as u32 + 1,
                            ));
                        }
                    }
                }
                // Don't descend into nested function/impl/mod scopes
                "function_item" | "impl_item" | "mod_item" | "closure_expression" => {
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

// ── Type reference extraction ──

/// Extract type references from function parameter types and return type.
fn extract_fn_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    // Parameter types
    if let Some(params) = node.child_by_field_name("parameters") {
        collect_type_refs_recursive(params, source, file_path, sym_id, edges);
    }
    // Return type
    if let Some(ret) = node.child_by_field_name("return_type") {
        collect_type_refs_recursive(ret, source, file_path, sym_id, edges);
    }
}

/// Recursively walk a subtree collecting type_identifier and scoped_type_identifier references.
fn collect_type_refs_recursive(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    match node.kind() {
        "type_identifier" => {
            let name = node_text(node, source);
            // Skip primitive types (lowercase: i32, u64, bool, str, etc.)
            // and common generic wrappers that are always in scope
            if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_uppercase()) {
                edges.push(Edge::new(
                    sym_id,
                    name,
                    EdgeKind::References,
                    file_path,
                    node.start_position().row as u32 + 1,
                ));
            }
        }
        "scoped_type_identifier" => {
            // e.g. std::io::Error — extract the full path as a reference
            let name = extract_type_name(node, source);
            if !name.is_empty() {
                edges.push(Edge::new(
                    sym_id,
                    name,
                    EdgeKind::References,
                    file_path,
                    node.start_position().row as u32 + 1,
                ));
            }
        }
        // Don't recurse into scoped_type_identifier children (already handled above)
        _ => {
            for child in node.named_children(&mut node.walk()) {
                collect_type_refs_recursive(child, source, file_path, sym_id, edges);
            }
        }
    }
}

// ── Helpers ──

fn has_child_kind(node: Node, kind: &str) -> bool {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == kind {
                return true;
            }
            // Check inside function_modifiers for async/const/unsafe
            if child.kind() == "function_modifiers" {
                for j in 0..child.child_count() {
                    if let Some(gc) = child.child(j as u32) {
                        if gc.kind() == kind {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

fn rust_visibility(node: Node, source: &str) -> Visibility {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "visibility_modifier" {
                let text = node_text(child, source);
                return match text {
                    "pub" => Visibility::Public,
                    _ if text.starts_with("pub(") => Visibility::Protected,
                    _ => Visibility::Public,
                };
            }
        }
    }
    // No pub → private by default in Rust
    Visibility::Private
}

fn extract_fn_signature(node: Node, source: &str) -> Option<String> {
    let params = node.child_by_field_name("parameters")?;
    let params_text = node_text(params, source);

    let return_type = node.child_by_field_name("return_type").map(|r| {
        let text = node_text(r, source);
        format!(" -> {text}")
    });

    Some(format!("{params_text}{}", return_type.unwrap_or_default()))
}

/// True when a preceding `#[...]` marks this item as a test: `#[test]`,
/// `#[bench]`, or any path ending in `::test` (`#[tokio::test]`, `#[rstest]`…).
///
/// Attributes are *preceding siblings* of the item, not children, so this walks
/// backwards the same way [`extract_doc_comment`] does.
fn has_test_attribute(node: Node, source: &str) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let text = node_text(p, source);
                // Trim the `#[...]` wrapper and any argument list, then compare the
                // final path segment so `#[tokio::test]` matches but `#[testing]` does not.
                let inner = text
                    .trim_start_matches('#')
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .trim();
                let path = inner.split('(').next().unwrap_or(inner).trim();
                let last = path.rsplit("::").next().unwrap_or(path).trim();
                if matches!(last, "test" | "bench") || path == "rstest" {
                    return true;
                }
            }
            // Doc comments sit between the attribute and the item.
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_sibling();
    }
    false
}

/// True when `node` sits inside a `#[cfg(test)]` module, so it is test code even
/// without its own `#[test]` (helpers, fixtures, and the `mod tests` convention).
///
/// Derived by walking ancestors rather than threaded as a parameter: every
/// extractor arm would otherwise need to forward the flag.
fn is_inside_cfg_test_mod(node: Node, source: &str) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "mod_item" && has_cfg_test_attribute(n, source) {
            return true;
        }
        cur = n.parent();
    }
    false
}

/// True when a preceding `#[cfg(test)]` (or `#[cfg(all(test, …))]`) gates this item.
fn has_cfg_test_attribute(node: Node, source: &str) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                // Whitespace-insensitive so `#[cfg( test )]` and `cfg(all(test, x))` match.
                let text: String = node_text(p, source)
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                if text.contains("cfg(test)") || text.contains("(test,") {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_sibling();
    }
    false
}

fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut prev = node.prev_sibling();

    // Collect preceding line_comment or attribute nodes that are doc comments
    while let Some(p) = prev {
        if p.kind() == "line_comment" {
            let text = node_text(p, source);
            if let Some(stripped) = text.strip_prefix("///") {
                lines.push(stripped.trim().to_string());
            } else if text.starts_with("//!") {
                // Inner doc comment, skip
                break;
            } else {
                break;
            }
        } else if p.kind() == "attribute_item" {
            // Skip attributes between doc comments and the item
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

fn extract_type_name(node: Node, source: &str) -> String {
    crate::parse::guard_recursion!(String::new());
    match node.kind() {
        "type_identifier" | "identifier" => node_text(node, source).to_string(),
        "scoped_type_identifier" | "scoped_identifier" => {
            // foo::bar::Baz — get last segment
            if let Some(name) = node.child_by_field_name("name") {
                return node_text(name, source).to_string();
            }
            node_text(node, source).to_string()
        }
        "generic_type" => {
            // Foo<T> — get "Foo"
            if let Some(t) = node.child_by_field_name("type") {
                return extract_type_name(t, source);
            }
            node.named_child(0)
                .map(|c| node_text(c, source).to_string())
                .unwrap_or_default()
        }
        _ => node_text(node, source).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> ExtractionResult {
        let mut ext = RustExtractor::new();
        ext.extract(source, "test.rs").unwrap()
    }

    #[test]
    fn test_function() {
        let result = extract(
            r#"
/// Greets someone by name.
pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#,
        );

        let func = result.symbols.iter().find(|s| s.name == "greet");
        assert!(func.is_some());
        let f = func.unwrap();
        assert_eq!(f.kind, SymbolKind::Function);
        assert_eq!(f.visibility, Visibility::Public);
        assert_eq!(f.signature.as_deref(), Some("(name: &str) -> String"));
        assert_eq!(f.docstring.as_deref(), Some("Greets someone by name."));
    }

    #[test]
    fn test_async_function() {
        let result = extract(
            r#"
pub async fn fetch_data(url: &str) -> Result<String> {
    reqwest::get(url).await?.text().await
}
"#,
        );

        let func = result.symbols.iter().find(|s| s.name == "fetch_data");
        assert!(func.is_some());
        assert!(func.unwrap().is_async);
    }

    #[test]
    fn test_struct_and_impl() {
        let result = extract(
            r#"
pub struct UserService {
    db: Database,
}

impl UserService {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub fn get_user(&self, id: u64) -> Option<User> {
        self.db.find(id)
    }

    fn internal(&self) {}
}
"#,
        );

        // struct + impl should produce exactly ONE class symbol, not two
        let user_services: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.name == "UserService")
            .collect();
        assert_eq!(
            user_services.len(),
            1,
            "impl block should reuse struct symbol, not create duplicate"
        );
        assert_eq!(user_services[0].kind, SymbolKind::Class);

        let new_fn = result.symbols.iter().find(|s| s.name == "new");
        assert!(new_fn.is_some());
        assert_eq!(new_fn.unwrap().kind, SymbolKind::Method);
        assert_eq!(new_fn.unwrap().visibility, Visibility::Public);

        let internal = result.symbols.iter().find(|s| s.name == "internal");
        assert!(internal.is_some());
        assert_eq!(internal.unwrap().visibility, Visibility::Private);
    }

    #[test]
    fn test_trait_impl() {
        let result = extract(
            r#"
pub trait Serializable {
    fn serialize(&self) -> String;
}

impl Serializable for UserService {
    fn serialize(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}
"#,
        );

        let trait_sym = result.symbols.iter().find(|s| s.name == "Serializable");
        assert!(trait_sym.is_some());

        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].target_name, "Serializable");
    }

    #[test]
    fn test_struct_with_multiple_impl_blocks() {
        let result = extract(
            r#"
pub struct AppError {
    message: String,
    code: u16,
}

impl AppError {
    pub fn new(message: String, code: u16) -> Self {
        Self { message, code }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
"#,
        );

        // struct + 2 impl blocks should produce exactly ONE class symbol
        let app_errors: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.name == "AppError")
            .collect();
        assert_eq!(
            app_errors.len(),
            1,
            "multiple impl blocks should reuse struct symbol"
        );

        // Methods from both impls should be extracted
        let new_fn = result.symbols.iter().find(|s| s.name == "new");
        assert!(new_fn.is_some());
        let fmt_fn = result.symbols.iter().find(|s| s.name == "fmt");
        assert!(fmt_fn.is_some());

        // trait impl should create Implements edge
        let implements: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(implements.len(), 1);
        assert_eq!(implements[0].target_name, "Display");
    }

    #[test]
    fn test_enum() {
        let result = extract(
            r#"
pub enum Status {
    Active,
    Inactive,
    Pending,
}
"#,
        );

        let e = result.symbols.iter().find(|s| s.name == "Status");
        assert!(e.is_some());
        assert_eq!(e.unwrap().kind, SymbolKind::Enum);
        assert_eq!(e.unwrap().visibility, Visibility::Public);
    }

    #[test]
    fn test_use_declarations() {
        let result = extract(
            r#"
use std::collections::HashMap;
use anyhow::{Context, Result};
use crate::types::Symbol;
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
        assert!(import_edges.len() >= 3); // HashMap, Context, Result, Symbol
    }

    #[test]
    fn test_function_calls() {
        let result = extract(
            r#"
fn process() {
    let data = fetch_data();
    let result = transform(data);
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
        assert!(targets.contains(&"fetch_data"));
        assert!(targets.contains(&"transform"));
        assert!(targets.contains(&"save"));
    }

    #[test]
    fn test_macro_calls() {
        let result = extract(
            r#"
fn example() {
    println!("hello");
    let v = vec![1, 2, 3];
}
"#,
        );

        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();

        let targets: Vec<&str> = calls.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"println!"));
        assert!(targets.contains(&"vec!"));
    }

    #[test]
    fn test_visibility() {
        let result = extract(
            r#"
pub fn public_fn() {}
fn private_fn() {}
pub(crate) fn crate_fn() {}
"#,
        );

        let public = result.symbols.iter().find(|s| s.name == "public_fn");
        assert_eq!(public.unwrap().visibility, Visibility::Public);

        let private = result.symbols.iter().find(|s| s.name == "private_fn");
        assert_eq!(private.unwrap().visibility, Visibility::Private);

        let crate_fn = result.symbols.iter().find(|s| s.name == "crate_fn");
        assert_eq!(crate_fn.unwrap().visibility, Visibility::Protected);
    }

    #[test]
    fn test_type_annotation_refs() {
        let result = extract(
            r#"
fn process(user: User, count: u32) -> Response {
    todo!()
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
        assert!(targets.contains(&"Response"));
        // u32 is lowercase → not captured
        assert!(!targets.contains(&"u32"));
    }

    #[test]
    fn test_generic_type_refs() {
        let result = extract(
            r#"
fn find(id: u64) -> Option<User> {
    todo!()
}
"#,
        );

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();

        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"Option"));
        assert!(targets.contains(&"User"));
    }

    #[test]
    fn test_result_type_refs() {
        let result = extract(
            r#"
fn connect(addr: &str) -> Result<Connection> {
    todo!()
}
"#,
        );

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();

        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"Result"));
        assert!(targets.contains(&"Connection"));
    }

    #[test]
    fn test_const_and_static() {
        let result = extract(
            r#"
/// Maximum retries.
pub const MAX_RETRIES: u32 = 3;
static DB_POOL: Pool = Pool::new();
"#,
        );

        let max = result
            .symbols
            .iter()
            .find(|s| s.name == "MAX_RETRIES")
            .unwrap();
        assert_eq!(max.kind, SymbolKind::Variable);
        assert_eq!(max.visibility, Visibility::Public);
        assert_eq!(max.docstring.as_deref(), Some("Maximum retries."));

        let pool = result.symbols.iter().find(|s| s.name == "DB_POOL").unwrap();
        assert_eq!(pool.kind, SymbolKind::Variable);
        assert_eq!(pool.visibility, Visibility::Private);

        // Static initializer call should be captured
        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].target_name, "Pool::new");
    }

    #[test]
    fn test_type_alias() {
        let result = extract(
            r#"
pub type Result<T> = std::result::Result<T, Error>;
type Handler = Box<dyn Fn()>;
"#,
        );

        let alias = result.symbols.iter().find(|s| s.name == "Result").unwrap();
        assert_eq!(alias.kind, SymbolKind::TypeAlias);
        assert_eq!(alias.visibility, Visibility::Public);

        let handler = result.symbols.iter().find(|s| s.name == "Handler").unwrap();
        assert_eq!(handler.visibility, Visibility::Private);
    }

    #[test]
    fn test_inline_mod() {
        let result = extract(
            r#"
pub mod auth {
    pub fn login() {}
    fn verify() {}
}
"#,
        );

        let module = result.symbols.iter().find(|s| s.name == "auth").unwrap();
        assert_eq!(module.kind, SymbolKind::Module);
        assert_eq!(module.visibility, Visibility::Public);

        let login = result.symbols.iter().find(|s| s.name == "login").unwrap();
        assert!(login.parent_id.is_some());
        assert_eq!(login.visibility, Visibility::Public);

        let verify = result.symbols.iter().find(|s| s.name == "verify").unwrap();
        assert!(verify.parent_id.is_some());
        assert_eq!(verify.visibility, Visibility::Private);
    }

    #[test]
    fn test_extern_mod_ignored() {
        // `mod foo;` (no body) should not emit a symbol
        let result = extract("mod foo;");
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn test_use_wildcard() {
        let result = extract("use std::io::*;");

        let imports: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 1);

        // Wildcard imports don't produce specific import edges
        let edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();
        assert!(edges.is_empty());
    }

    #[test]
    fn test_use_as_clause() {
        let result = extract("use std::collections::HashMap as Map;");

        let imports: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 1);

        let edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();
        // `use_as_clause` collects the original name from the path
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_name, "HashMap");
    }

    #[test]
    fn test_scoped_type_refs() {
        let result = extract(
            r#"
fn connect(cfg: &crate::Config) -> io::Result<Connection> {
    todo!()
}
"#,
        );

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(refs.contains(&"Config"));
        assert!(refs.contains(&"Result"));
        assert!(refs.contains(&"Connection"));
    }

    #[test]
    fn test_empty_file() {
        let result = extract("");
        assert!(result.symbols.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_syntax_error_partial_parse() {
        let result = extract("fn broken( { }");
        let _ = result.symbols.len();
    }

    #[test]
    fn test_enum_variants() {
        let result = extract(
            r#"
pub enum Shape {
    Unit,
    Circle(f64),
    Rectangle { width: f64, height: f64 },
}
"#,
        );

        let e = result.symbols.iter().find(|s| s.name == "Shape").unwrap();
        assert_eq!(e.kind, SymbolKind::Enum);

        let variants: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::EnumMember)
            .collect();
        assert_eq!(variants.len(), 3);
        for variant in &variants {
            assert_eq!(variant.parent_id.as_deref(), Some(e.id.as_str()));
        }

        let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"Unit"));
        assert!(names.contains(&"Circle"));
        assert!(names.contains(&"Rectangle"));

        // Inner fields of a struct-like variant are not emitted as their own symbols.
        assert!(!result.symbols.iter().any(|s| s.name == "width"));
    }

    #[test]
    fn test_struct_named_fields() {
        let result = extract(
            r#"
pub struct User {
    pub name: String,
    age: u32,
}
"#,
        );

        let user = result.symbols.iter().find(|s| s.name == "User").unwrap();
        assert_eq!(user.kind, SymbolKind::Class);

        let fields: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Variable)
            .collect();
        assert_eq!(fields.len(), 2);

        let name_field = fields.iter().find(|f| f.name == "name").unwrap();
        assert_eq!(name_field.parent_id.as_deref(), Some(user.id.as_str()));
        assert_eq!(name_field.visibility, Visibility::Public);
        assert_eq!(name_field.signature.as_deref(), Some("String"));

        let age_field = fields.iter().find(|f| f.name == "age").unwrap();
        assert_eq!(age_field.visibility, Visibility::Private);
        assert_eq!(age_field.signature.as_deref(), Some("u32"));
    }

    #[test]
    fn test_tuple_struct_has_no_field_symbols() {
        let result = extract("pub struct Point(f32, f32);");

        let point = result.symbols.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(point.kind, SymbolKind::Class);

        let fields: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Variable)
            .collect();
        assert!(fields.is_empty());
    }

    #[test]
    fn test_macro_rules_definition() {
        let result = extract(
            r#"
#[macro_export]
macro_rules! foo {
    () => {};
}
"#,
        );

        let mac = result.symbols.iter().find(|s| s.name == "foo").unwrap();
        assert_eq!(mac.kind, SymbolKind::Macro);
    }

    #[test]
    fn test_fn_inside_mod_is_function_not_method() {
        let result = extract(
            r#"
mod tests {
    fn helper() {}
}
"#,
        );

        let module = result.symbols.iter().find(|s| s.name == "tests").unwrap();
        let helper = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper.kind, SymbolKind::Function);
        assert_eq!(helper.parent_id.as_deref(), Some(module.id.as_str()));
    }

    #[test]
    fn test_fn_inside_impl_is_method() {
        let result = extract(
            r#"
struct Foo;

impl Foo {
    fn bar() {}
}
"#,
        );

        let bar = result.symbols.iter().find(|s| s.name == "bar").unwrap();
        assert_eq!(bar.kind, SymbolKind::Method);
    }

    #[test]
    fn test_top_level_fn_is_function() {
        let result = extract("fn standalone() {}");

        let f = result
            .symbols
            .iter()
            .find(|s| s.name == "standalone")
            .unwrap();
        assert_eq!(f.kind, SymbolKind::Function);
    }

    #[test]
    fn test_fn_inside_nested_mod_is_function() {
        let result = extract(
            r#"
mod outer {
    mod inner {
        fn deep() {}
    }
}
"#,
        );

        let deep = result.symbols.iter().find(|s| s.name == "deep").unwrap();
        assert_eq!(deep.kind, SymbolKind::Function);
        assert!(deep.parent_id.is_some());
    }

    // ── #[test] flagging ──

    fn f<'a>(r: &'a ExtractionResult, name: &str) -> &'a Symbol {
        r.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol {name:?}"))
    }

    #[test]
    fn plain_test_attribute_sets_is_test() {
        let r = extract("#[test]\nfn checks_it() {}\n");
        assert!(f(&r, "checks_it").is_test);
    }

    #[test]
    fn qualified_test_attributes_set_is_test() {
        for attr in [
            "#[tokio::test]",
            "#[async_std::test]",
            "#[bench]",
            "#[rstest]",
        ] {
            let r = extract(&format!("{attr}\nfn t() {{}}\n"));
            assert!(f(&r, "t").is_test, "{attr} not recognized");
        }
    }

    #[test]
    fn attribute_below_a_doc_comment_still_sets_is_test() {
        let r = extract("/// Docs.\n#[test]\nfn documented() {}\n");
        let s = f(&r, "documented");
        assert!(s.is_test);
        assert!(s.docstring.is_some(), "doc comment must survive too");
    }

    #[test]
    fn every_fn_in_a_cfg_test_mod_is_test_including_helpers() {
        let r = extract(
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn case() {}\n    fn helper() {}\n}\n",
        );
        assert!(f(&r, "case").is_test);
        // A helper in a test module is test code even without its own attribute.
        assert!(f(&r, "helper").is_test);
    }

    #[test]
    fn cfg_test_applies_to_deeply_nested_fns() {
        let r =
            extract("#[cfg(test)]\nmod outer {\n    mod inner {\n        fn deep() {}\n    }\n}\n");
        assert!(f(&r, "deep").is_test);
    }

    #[test]
    fn methods_in_a_cfg_test_mod_are_flagged() {
        let r = extract("#[cfg(test)]\nmod tests {\n    struct H;\n    impl H {\n        fn m(&self) {}\n    }\n}\n");
        let m = f(&r, "m");
        assert_eq!(m.kind, SymbolKind::Method);
        assert!(m.is_test);
    }

    #[test]
    fn production_code_is_never_flagged_as_test() {
        let r = extract("pub fn validate(x: u8) -> bool { x > 0 }\n");
        assert!(!f(&r, "validate").is_test);
    }

    #[test]
    fn similarly_named_attributes_do_not_false_positive() {
        // `#[testing]`/`#[test_case]` are not test markers; only a `test` path segment is.
        for attr in ["#[testing]", "#[test_util]", "#[derive(Debug)]"] {
            let r = extract(&format!("{attr}\nfn t() {{}}\n"));
            assert!(!f(&r, "t").is_test, "{attr} wrongly flagged");
        }
    }

    #[test]
    fn a_non_cfg_test_mod_does_not_flag_its_fns() {
        let r = extract("mod util {\n    fn helper() {}\n}\n");
        assert!(!f(&r, "helper").is_test);
    }

    #[test]
    fn cfg_test_combined_with_other_predicates_is_recognized() {
        let r = extract("#[cfg(all(test, feature = \"x\"))]\nmod tests {\n    fn helper() {}\n}\n");
        assert!(f(&r, "helper").is_test);
    }

    /// A Rust variant carries no `pub` of its own — it is as visible as its enum.
    #[test]
    fn enum_variants_inherit_the_enums_visibility() {
        let r = extract("pub enum Color { Red, Green }\nenum Hidden { A }\n");
        for v in ["Red", "Green"] {
            let s = r.symbols.iter().find(|s| s.name == v).expect("variant");
            assert_eq!(s.kind, SymbolKind::EnumMember);
            assert_eq!(s.visibility, Visibility::Public, "{v} of a pub enum");
        }
        let a = r.symbols.iter().find(|s| s.name == "A").expect("variant");
        assert_eq!(
            a.visibility,
            Visibility::Private,
            "variant of a private enum"
        );
    }

    /// Struct fields DO have their own visibility — guard against the enum fix
    /// leaking into them.
    #[test]
    fn struct_fields_keep_their_own_visibility() {
        let r = extract("pub struct P { pub x: i32, y: i32 }\n");
        let x = r.symbols.iter().find(|s| s.name == "x").expect("field x");
        let y = r.symbols.iter().find(|s| s.name == "y").expect("field y");
        assert_eq!(x.visibility, Visibility::Public);
        assert_eq!(y.visibility, Visibility::Private);
    }
}
