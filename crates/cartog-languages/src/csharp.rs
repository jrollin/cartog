use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use cartog_core::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{last_segment, node_text, ExtractionResult, Extractor};

pub struct CSharpExtractor {
    parser: Parser,
}

impl CSharpExtractor {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_c_sharp::LANGUAGE))
            .expect("C# grammar should always load");
        Self { parser }
    }
}

impl Default for CSharpExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for CSharpExtractor {
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
        "class_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "record_struct_declaration" => {
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
        "interface_declaration" => {
            extract_interface(
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
            extract_enum(node, source, file_path, parent_id, parent_qname, symbols);
        }
        "delegate_declaration" => {
            extract_delegate(node, source, file_path, parent_id, parent_qname, symbols);
        }
        // Block namespace (`namespace Foo { }`): its declarations are children, so
        // fold the name into their qname prefix here (D2 — no symbol emitted).
        "namespace_declaration" => {
            extract_block_namespace(
                node,
                source,
                file_path,
                parent_id,
                parent_qname,
                symbols,
                edges,
            );
        }
        "using_directive" => {
            extract_using(node, source, file_path, parent_qname, symbols, edges);
        }
        // Default: walk children, tracking a running file-scoped namespace prefix.
        // A `namespace Foo;` declaration has no body — the declarations that follow
        // are its siblings, so its name must scope every later sibling (D2/D3).
        // Top-level statements (`global_statement`) are the implicit entry point:
        // synthesize one `Main` symbol per file and anchor their calls to it, so
        // every edge keeps a valid source_id (the dedup invariant).
        _ => {
            let mut ns_prefix: Option<String> = None;
            let mut main_id: Option<String> = None;
            for child in node.named_children(&mut node.walk()) {
                match child.kind() {
                    "file_scoped_namespace_declaration" => {
                        ns_prefix = child
                            .child_by_field_name("name")
                            .map(|n| join_qname(parent_qname, node_text(n, source)));
                    }
                    "global_statement" => {
                        // A top-level local function is its own symbol (its body's
                        // calls belong to it, not the synthetic Main). Every other
                        // top-level statement is entry-point code: anchor its calls
                        // to one synthesized `Main` per file, whose span grows to
                        // cover the whole run of statements.
                        if let Some(func) = child
                            .named_children(&mut child.walk())
                            .find(|c| c.kind() == "local_function_statement")
                        {
                            extract_method(
                                func,
                                source,
                                file_path,
                                parent_id,
                                parent_qname,
                                symbols,
                                edges,
                            );
                        } else {
                            let id = main_id.get_or_insert_with(|| {
                                emit_implicit_main(child, source, file_path, symbols)
                            });
                            extend_symbol_span(symbols, id, child);
                            walk_for_calls(child, source, file_path, id, edges);
                        }
                    }
                    _ => {
                        let qname = ns_prefix.as_deref().or(parent_qname);
                        extract_node(child, source, file_path, parent_id, qname, symbols, edges);
                    }
                }
            }
        }
    }
}

/// Synthesize the implicit entry-point symbol for a file of top-level statements
/// (the C# compiler generates a `Main`). Returns its id so calls anchor to it.
fn emit_implicit_main(
    node: Node,
    _source: &str,
    file_path: &str,
    symbols: &mut Vec<Symbol>,
) -> String {
    let line = node.start_position().row as u32 + 1;
    let sym = Symbol::new(
        "Main",
        SymbolKind::Function,
        file_path,
        line,
        line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        None,
    );
    let id = sym.id.clone();
    symbols.push(sym);
    id
}

/// Grow the synthetic Main symbol's span to cover a later top-level statement,
/// so its byte/line range spans the whole run rather than just the first one.
fn extend_symbol_span(symbols: &mut [Symbol], id: &str, node: Node) {
    if let Some(sym) = symbols.iter_mut().find(|s| s.id == id) {
        sym.end_line = sym.end_line.max(node.end_position().row as u32 + 1);
        sym.end_byte = sym.end_byte.max(node.end_byte() as u32);
    }
}

// ── Namespaces (qname prefix only, not a symbol) ──

fn extract_block_namespace(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let child_qname = node
        .child_by_field_name("name")
        .map(|n| join_qname(parent_qname, node_text(n, source)));
    let child_qname = child_qname.as_deref().or(parent_qname);

    if let Some(body) = node.child_by_field_name("body") {
        // The body is itself walked by the default arm (which handles any nested
        // file-scoped namespace), so recurse into it with the extended prefix.
        extract_node(
            body,
            source,
            file_path,
            parent_id,
            child_qname,
            symbols,
            edges,
        );
    }
}

/// Extend an optional qname prefix with a namespace name (`A.B` under `Web` → `Web.A.B`).
fn join_qname(parent_qname: Option<&str>, ns: &str) -> String {
    match parent_qname {
        Some(pq) if !pq.is_empty() => format!("{pq}.{ns}"),
        _ => ns.to_string(),
    }
}

// ── Class / struct / record ──

fn extract_class_like(
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
    let end_line = node.end_position().row as u32 + 1;
    let visibility = csharp_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    // record/struct/class all map onto Class (D2).
    let sym_id = symbol_id(file_path, SymbolKind::Class, &name, parent_qname);
    let class_qname = qualified(parent_qname, &name);
    let mut sym = Symbol::new(
        name.clone(),
        SymbolKind::Class,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent_qname,
    )
    .with_parent(parent_id)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // base_list is flat and unlabeled (D1): the grammar cannot distinguish a base
    // class from an interface, so every entry becomes Implements. "extends" is
    // recovered downstream from the resolved target's SymbolKind (Class).
    if let Some(bl) = node.named_children(&mut node.walk()).find(base_list) {
        extract_base_list(bl, source, file_path, &sym_id, start_line, edges);
    }

    // Positional record parameters become Variable children (D2). The grammar
    // attaches the `parameter_list` as an unlabeled child, not a `parameters` field.
    if let Some(params) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "parameter_list")
    {
        extract_record_params(
            params,
            source,
            file_path,
            &sym_id,
            &class_qname,
            symbols,
            edges,
        );
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_type_body(
            body,
            source,
            file_path,
            &sym_id,
            &class_qname,
            symbols,
            edges,
        );
    }
}

fn base_list(n: &Node) -> bool {
    n.kind() == "base_list"
}

// ── Interfaces ──

fn extract_interface(
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
    let end_line = node.end_position().row as u32 + 1;
    let visibility = csharp_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, SymbolKind::Interface, &name, parent_qname);
    let iface_qname = qualified(parent_qname, &name);
    let mut sym = Symbol::new(
        name.clone(),
        SymbolKind::Interface,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent_qname,
    )
    .with_parent(parent_id)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Interfaces only extend interfaces, so a base_list here is Inherits (D1).
    if let Some(bl) = node.named_children(&mut node.walk()).find(base_list) {
        extract_base_list_as(
            bl,
            source,
            file_path,
            &sym_id,
            start_line,
            EdgeKind::Inherits,
            edges,
        );
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_type_body(
            body,
            source,
            file_path,
            &sym_id,
            &iface_qname,
            symbols,
            edges,
        );
    }
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
    let end_line = node.end_position().row as u32 + 1;
    let visibility = csharp_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, SymbolKind::Enum, &name, parent_qname);
    let enum_qname = qualified(parent_qname, &name);
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
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    if let Some(body) = node.child_by_field_name("body") {
        for member in body.named_children(&mut body.walk()) {
            if member.kind() == "enum_member_declaration" {
                if let Some(mn) = member.child_by_field_name("name") {
                    let member_name = node_text(mn, source).to_string();
                    let m_line = member.start_position().row as u32 + 1;
                    symbols.push(
                        Symbol::new(
                            member_name,
                            SymbolKind::EnumMember,
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
        }
    }
}

// ── Delegates (map onto Function) ──

fn extract_delegate(
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
    let end_line = node.end_position().row as u32 + 1;
    let visibility = csharp_visibility(node, source);
    let signature = node
        .child_by_field_name("parameters")
        .map(|p| node_text(p, source).to_string());

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
    .with_signature(signature);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);
}

// ── Type body traversal (methods, properties, fields, nested types) ──

fn extract_type_body(
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
            "method_declaration" | "local_function_statement" => {
                extract_method(
                    child,
                    source,
                    file_path,
                    Some(parent_id),
                    Some(parent_qname),
                    symbols,
                    edges,
                );
            }
            "constructor_declaration" => {
                extract_constructor(
                    child,
                    source,
                    file_path,
                    parent_id,
                    parent_qname,
                    symbols,
                    edges,
                );
            }
            "property_declaration" => {
                extract_property(
                    child,
                    source,
                    file_path,
                    parent_id,
                    parent_qname,
                    symbols,
                    edges,
                );
            }
            "field_declaration" => {
                extract_field(
                    child,
                    source,
                    file_path,
                    parent_id,
                    parent_qname,
                    symbols,
                    edges,
                );
            }
            "class_declaration"
            | "struct_declaration"
            | "record_declaration"
            | "record_struct_declaration" => {
                extract_class_like(
                    child,
                    source,
                    file_path,
                    Some(parent_id),
                    Some(parent_qname),
                    symbols,
                    edges,
                );
            }
            "interface_declaration" => {
                extract_interface(
                    child,
                    source,
                    file_path,
                    Some(parent_id),
                    Some(parent_qname),
                    symbols,
                    edges,
                );
            }
            "enum_declaration" => {
                extract_enum(
                    child,
                    source,
                    file_path,
                    Some(parent_id),
                    Some(parent_qname),
                    symbols,
                );
            }
            "delegate_declaration" => {
                extract_delegate(
                    child,
                    source,
                    file_path,
                    Some(parent_id),
                    Some(parent_qname),
                    symbols,
                );
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
    let end_line = node.end_position().row as u32 + 1;
    let visibility = csharp_visibility(node, source);
    let is_async = has_modifier(node, source, "async");
    let signature = extract_method_signature(node, source);
    let docstring = extract_doc_comment(node, source);

    // parent_qname/id are None for a top-level local function (no enclosing
    // type) — pass through so the symbol_id has no leading `.` and the parent
    // stays NULL rather than an empty string.
    let sym_id = symbol_id(file_path, SymbolKind::Method, &name, parent_qname);
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
    .with_signature(signature)
    .with_docstring(docstring)
    .with_async(is_async);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    extract_method_type_refs(node, source, file_path, &sym_id, edges);
    walk_body_for_calls(node, source, file_path, &sym_id, edges);
}

// ── Constructors ──

fn extract_constructor(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    parent_qname: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = csharp_visibility(node, source);
    let signature = node
        .child_by_field_name("parameters")
        .map(|p| node_text(p, source).to_string());

    let sym_id = symbol_id(file_path, SymbolKind::Method, &name, Some(parent_qname));
    let mut sym = Symbol::new(
        name,
        SymbolKind::Method,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        Some(parent_qname),
    )
    .with_parent(Some(parent_id))
    .with_signature(signature);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    walk_body_for_calls(node, source, file_path, &sym_id, edges);
}

// ── Properties (→ Variable; call edges from get/set/arrow bodies) ──

fn extract_property(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    parent_qname: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = csharp_visibility(node, source);
    let type_node = node.child_by_field_name("type");
    let signature = type_node.map(|t| format!("{} {name}", node_text(t, source)));

    let sym_id = symbol_id(file_path, SymbolKind::Variable, &name, Some(parent_qname));
    let mut sym = Symbol::new(
        name,
        SymbolKind::Variable,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        Some(parent_qname),
    )
    .with_parent(Some(parent_id))
    .with_signature(signature);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    if let Some(tn) = type_node {
        collect_type_refs(
            tn,
            source,
            file_path,
            &sym_id,
            tn.start_position().row as u32 + 1,
            edges,
        );
    }

    // Call edges from accessor bodies (get/set) and expression-bodied properties.
    walk_for_calls(node, source, file_path, &sym_id, edges);
}

// ── Fields ──

fn extract_field(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    parent_qname: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let visibility = csharp_visibility(node, source);
    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    let decl = match node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "variable_declaration")
    {
        Some(d) => d,
        None => return,
    };
    let type_node = decl.child_by_field_name("type");
    let type_text = type_node
        .map(|t| node_text(t, source).to_string())
        .unwrap_or_default();

    // A field_declaration may declare several names: `int a, b;`
    for declarator in decl.named_children(&mut decl.walk()) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let name = match declarator.child_by_field_name("name") {
            Some(n) => node_text(n, source).to_string(),
            None => continue,
        };
        let signature = if type_text.is_empty() {
            None
        } else {
            Some(format!("{type_text} {name}"))
        };
        let sym_id = symbol_id(file_path, SymbolKind::Variable, &name, Some(parent_qname));
        let mut sym = Symbol::new(
            name,
            SymbolKind::Variable,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
            Some(parent_qname),
        )
        .with_parent(Some(parent_id))
        .with_signature(signature);
        if visibility != Visibility::Public {
            sym = sym.with_visibility(visibility);
        }
        symbols.push(sym);

        if let Some(tn) = type_node {
            collect_type_refs(tn, source, file_path, &sym_id, start_line, edges);
        }
    }
}

// ── Record positional parameters (→ Variable children) ──

fn extract_record_params(
    params: Node,
    source: &str,
    file_path: &str,
    parent_id: &str,
    parent_qname: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    for param in params.named_children(&mut params.walk()) {
        if param.kind() != "parameter" {
            continue;
        }
        let name = match param.child_by_field_name("name") {
            Some(n) => node_text(n, source).to_string(),
            None => continue,
        };
        let line = param.start_position().row as u32 + 1;
        let type_node = param.child_by_field_name("type");
        let signature = type_node.map(|t| format!("{} {name}", node_text(t, source)));
        let sym_id = symbol_id(file_path, SymbolKind::Variable, &name, Some(parent_qname));
        symbols.push(
            Symbol::new(
                name,
                SymbolKind::Variable,
                file_path,
                line,
                line,
                param.start_byte() as u32,
                param.end_byte() as u32,
                Some(parent_qname),
            )
            .with_parent(Some(parent_id))
            .with_signature(signature),
        );
        if let Some(tn) = type_node {
            collect_type_refs(tn, source, file_path, &sym_id, line, edges);
        }
    }
}

// ── Using directives (→ Imports) ──

fn extract_using(
    node: Node,
    source: &str,
    file_path: &str,
    parent_qname: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    // Full directive text minus the leading `global`/`using`/`static` keywords
    // and trailing `;`. Each keyword is followed by whitespace, so trim between
    // strips — without the trim after `using`, ` static X.Y` keeps `static`.
    let import_text = node_text(node, source)
        .trim_start_matches("global")
        .trim_start()
        .trim_start_matches("using")
        .trim_start()
        .trim_start_matches("static")
        .trim_end_matches(';')
        .trim()
        .to_string();
    if import_text.is_empty() {
        return;
    }

    let sym_id = symbol_id(file_path, SymbolKind::Import, &import_text, parent_qname);
    symbols.push(
        Symbol::new(
            import_text.clone(),
            SymbolKind::Import,
            file_path,
            line,
            line,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent_qname,
        )
        .with_signature(Some(
            node_text(node, source)
                .trim_end_matches(';')
                .trim()
                .to_string(),
        )),
    );

    // `using Alias = Some.Namespace.Type;` → the imported name is the alias.
    let target = if let Some(eq) = import_text.find('=') {
        import_text[..eq].trim().to_string()
    } else {
        last_segment(&import_text, ".").to_string()
    };
    if !target.is_empty() {
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

/// Walk a declaration node's body (block or arrow-expression) for call edges.
fn walk_body_for_calls(
    node: Node,
    source: &str,
    file_path: &str,
    context_id: &str,
    edges: &mut Vec<Edge>,
) {
    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, context_id, edges);
    }
}

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
                "invocation_expression" => {
                    if let Some(func) = current.child_by_field_name("function") {
                        let callee = build_callee_name(func, source);
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
                }
                "object_creation_expression" => {
                    if let Some(type_node) = current.child_by_field_name("type") {
                        let type_name = simple_type_name(type_node, source);
                        if !type_name.is_empty() && !is_builtin_type(&type_name) {
                            edges.push(Edge::new(
                                context_id,
                                type_name,
                                EdgeKind::References,
                                file_path,
                                current.start_position().row as u32 + 1,
                            ));
                        }
                    }
                }
                // Don't descend into local lambdas — their calls aren't this scope's.
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

/// Build `object.method` or bare `method` from an invocation's `function` node.
///
/// The method name is always reduced to its bare identifier (a `generic_name`
/// like `Get<User>` unwraps to `Get`) so the `.`-split resolver can match the
/// method symbol. A null-conditional call (`obj?.Method()`) presents its
/// `function` as a `member_binding_expression` (just `.Method`); it is treated
/// like a member access with no static receiver.
fn build_callee_name(node: Node, source: &str) -> String {
    match node.kind() {
        "identifier" => node_text(node, source).to_string(),
        "generic_name" => simple_name(node, source),
        "member_access_expression" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| simple_name(n, source))
                .unwrap_or_default();
            if name.is_empty() {
                return String::new();
            }
            match node.child_by_field_name("expression") {
                Some(obj) => {
                    let obj_text = node_text(obj, source);
                    if obj_text.is_empty() {
                        name
                    } else {
                        format!("{obj_text}.{name}")
                    }
                }
                None => name,
            }
        }
        // Null-conditional `obj?.Method()`: `function` is a
        // conditional_access_expression with a `condition:` receiver and a
        // member_binding_expression (`.Method`) child. Reassemble `obj.Method`.
        "conditional_access_expression" => {
            let binding = node
                .named_children(&mut node.walk())
                .find(|c| c.kind() == "member_binding_expression");
            let name = binding
                .and_then(|b| b.child_by_field_name("name"))
                .map(|n| simple_name(n, source))
                .unwrap_or_default();
            if name.is_empty() {
                return String::new();
            }
            match node.child_by_field_name("condition") {
                Some(cond) => {
                    let obj = node_text(cond, source);
                    if obj.is_empty() {
                        name
                    } else {
                        format!("{obj}.{name}")
                    }
                }
                None => name,
            }
        }
        // A bare `.Method` binding (rare as a top-level function node).
        "member_binding_expression" => node
            .child_by_field_name("name")
            .map(|n| simple_name(n, source))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Bare identifier for a name node, unwrapping a `generic_name` (`Get<T>` → `Get`).
fn simple_name(node: Node, source: &str) -> String {
    match node.kind() {
        "generic_name" => node
            .named_child(0)
            .map(|c| node_text(c, source).to_string())
            .unwrap_or_default(),
        _ => node_text(node, source).to_string(),
    }
}

// ── Inheritance edges ──

/// class/struct/record base_list → Implements for every entry (D1).
fn extract_base_list(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    extract_base_list_as(
        node,
        source,
        file_path,
        sym_id,
        line,
        EdgeKind::Implements,
        edges,
    );
}

fn extract_base_list_as(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edge_kind: EdgeKind,
    edges: &mut Vec<Edge>,
) {
    for base in node.named_children(&mut node.walk()) {
        let name = simple_type_name(base, source);
        if !name.is_empty() {
            edges.push(Edge::new(sym_id, name, edge_kind, file_path, line));
        }
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
    if let Some(ret) = node.child_by_field_name("returns") {
        collect_type_refs(
            ret,
            source,
            file_path,
            sym_id,
            ret.start_position().row as u32 + 1,
            edges,
        );
    }
    if let Some(params) = node.child_by_field_name("parameters") {
        for param in params.named_children(&mut params.walk()) {
            if param.kind() == "parameter" {
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

/// Recursively collect named type references, skipping built-ins.
fn collect_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    match node.kind() {
        "identifier" => {
            let name = node_text(node, source);
            if !name.is_empty() && !is_builtin_type(name) {
                edges.push(Edge::new(
                    sym_id,
                    name,
                    EdgeKind::References,
                    file_path,
                    line,
                ));
            }
        }
        "predefined_type" | "implicit_type" => {}
        _ => {
            for child in node.named_children(&mut node.walk()) {
                collect_type_refs(child, source, file_path, sym_id, line, edges);
            }
        }
    }
}

/// Simple (unqualified) type name from a base_list / type node, unwrapping
/// generics (`Repo<T>` → `Repo`) and qualified names (`A.B.C` → `C`).
fn simple_type_name(node: Node, source: &str) -> String {
    crate::parse::guard_recursion!(String::new());
    match node.kind() {
        "identifier" => node_text(node, source).to_string(),
        "generic_name" => node
            .named_child(0)
            .map(|c| simple_type_name(c, source))
            .unwrap_or_default(),
        "qualified_name" => node
            .child_by_field_name("name")
            .map(|c| simple_type_name(c, source))
            .unwrap_or_default(),
        "nullable_type" | "array_type" => node
            .named_child(0)
            .map(|c| simple_type_name(c, source))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "byte"
            | "sbyte"
            | "char"
            | "decimal"
            | "double"
            | "float"
            | "int"
            | "uint"
            | "long"
            | "ulong"
            | "short"
            | "ushort"
            | "object"
            | "string"
            | "void"
            | "var"
            | "dynamic"
            | "nint"
            | "nuint"
    )
}

// ── Signature ──

fn extract_method_signature(node: Node, source: &str) -> Option<String> {
    let ret = node
        .child_by_field_name("returns")
        .map(|t| node_text(t, source));
    let params = node
        .child_by_field_name("parameters")
        .map(|p| node_text(p, source))
        .unwrap_or("()");
    Some(format!(
        "{}{}",
        ret.map(|r| format!("{r} ")).unwrap_or_default(),
        params,
    ))
}

// ── Modifiers (visibility, async) ──

fn csharp_visibility(node: Node, source: &str) -> Visibility {
    let mut saw_public = false;
    let mut saw_protected = false;
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "modifier" {
            match node_text(child, source) {
                "private" => return Visibility::Private,
                "protected" => saw_protected = true,
                "public" => saw_public = true,
                _ => {}
            }
        }
    }
    if saw_public {
        Visibility::Public
    } else if saw_protected {
        Visibility::Protected
    } else {
        // No access modifier: C# defaults type/member visibility to internal or
        // private, but cartog has no `internal` variant — treat as Public so the
        // symbol stays visible (matches java.rs' package-private handling).
        Visibility::Public
    }
}

fn has_modifier(node: Node, source: &str, keyword: &str) -> bool {
    node.named_children(&mut node.walk())
        .any(|c| c.kind() == "modifier" && node_text(c, source) == keyword)
}

// ── Doc comment extraction (/// XML-doc) ──

fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut p = node.prev_sibling();
    while let Some(n) = p {
        // Stop at the first non-comment node, but skip a plain `//` comment
        // interposed between the declaration and its `///` block rather than
        // truncating the summary.
        if n.kind() != "comment" {
            break;
        }
        let text = node_text(n, source);
        if let Some(stripped) = text.strip_prefix("///") {
            // Drop XML tags (`<summary>`, `<param ...>`) for a plain summary.
            let clean = strip_xml_tags(stripped).trim().to_string();
            if !clean.is_empty() {
                lines.push(clean);
            }
        }
        p = n.prev_sibling();
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join(" "))
}

/// Remove `<...>` XML-doc tags, keeping the inner prose. A `<` that does not
/// open a well-formed tag (`<name`, `</name`, `<!--`) is treated as literal
/// prose, so a `///` comment like `count < 5 and value > 0` survives intact.
fn strip_xml_tags(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(end) = tag_end(bytes, i) {
                i = end + 1; // skip the whole `<...>` tag
                continue;
            }
        }
        // SAFETY: `i` sits on a char boundary — it only ever advances past a
        // full char below or past a whole ASCII `<...>` tag above.
        let ch = s[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// If a `<` at `start` opens a well-formed XML-doc tag, return the index of its
/// closing `>`; otherwise `None` (a bare `<` in prose). A tag begins with an
/// ASCII letter, `/`, or `!` right after `<`.
fn tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let first = *bytes.get(start + 1)?;
    if !(first.is_ascii_alphabetic() || first == b'/' || first == b'!') {
        return None;
    }
    bytes[start + 1..]
        .iter()
        .position(|&b| b == b'>')
        .map(|off| start + 1 + off)
}

/// Qualified name: `Parent.name` when nested, else `name`.
fn qualified(parent_qname: Option<&str>, name: &str) -> String {
    match parent_qname {
        Some(p) => format!("{p}.{name}"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> ExtractionResult {
        let mut ext = CSharpExtractor::new();
        ext.extract(source, "Test.cs").unwrap()
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
        let result = extract("public class Broken {");
        let _ = result.symbols.len();
    }

    #[test]
    fn test_class() {
        let result = extract("public class UserService { }");
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
        let result = extract("public interface IRepository { void Save(object entity); }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "IRepository")
            .unwrap();
        assert_eq!(sym.kind, SymbolKind::Interface);
    }

    #[test]
    fn test_struct_maps_to_class() {
        let result = extract("public struct Point { public int X; }");
        let sym = result.symbols.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(sym.kind, SymbolKind::Class);
    }

    #[test]
    fn test_record_maps_to_class_with_param_variables() {
        let result = extract("public record User(string Name, int Age);");
        let user = result.symbols.iter().find(|s| s.name == "User").unwrap();
        assert_eq!(user.kind, SymbolKind::Class);
        let name = result
            .symbols
            .iter()
            .find(|s| s.name == "Name" && s.kind == SymbolKind::Variable)
            .unwrap();
        assert!(name.parent_id.as_ref().unwrap().contains("User"));
    }

    #[test]
    fn test_method() {
        let result = extract(
            r#"
public class Foo {
    public string Greet(string name) { return "Hi " + name; }
}
"#,
        );
        let m = result.symbols.iter().find(|s| s.name == "Greet").unwrap();
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.visibility, Visibility::Public);
        assert!(m.parent_id.is_some());
        assert!(m.signature.as_deref().unwrap().contains("string"));
    }

    #[test]
    fn test_constructor() {
        let result = extract("public class Foo { public Foo(int x) {} }");
        let ctor = result
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Method);
        assert!(ctor.is_some());
    }

    #[test]
    fn test_property_is_variable() {
        let result = extract("public class Foo { public string Name { get; set; } }");
        let p = result
            .symbols
            .iter()
            .find(|s| s.name == "Name" && s.kind == SymbolKind::Variable)
            .unwrap();
        assert!(p.signature.as_deref().unwrap().contains("string"));
    }

    #[test]
    fn test_property_arrow_body_calls() {
        let result = extract(
            r#"
public class Foo {
    public string Token => Generate();
}
"#,
        );
        assert!(edge_targets(&result, EdgeKind::Calls).contains(&"Generate".to_string()));
    }

    #[test]
    fn test_field() {
        let result = extract(
            r#"
public class Foo {
    private int _count;
    protected string _name;
}
"#,
        );
        let count = result.symbols.iter().find(|s| s.name == "_count").unwrap();
        assert_eq!(count.kind, SymbolKind::Variable);
        assert_eq!(count.visibility, Visibility::Private);
        let name = result.symbols.iter().find(|s| s.name == "_name").unwrap();
        assert_eq!(name.visibility, Visibility::Protected);
    }

    #[test]
    fn test_enum() {
        let result = extract("public enum Status { Active, Inactive }");
        let status = result.symbols.iter().find(|s| s.name == "Status").unwrap();
        assert_eq!(status.kind, SymbolKind::Enum);
        let active = result.symbols.iter().find(|s| s.name == "Active").unwrap();
        assert_eq!(active.kind, SymbolKind::EnumMember);
        assert!(active.parent_id.as_ref().unwrap().contains("Status"));
    }

    #[test]
    fn test_enum_member_and_field_kinds_stay_distinct() {
        // Regression guard: enum-member re-kinding must not leak into fields.
        let result = extract(
            r#"
public enum Status { Active }
public class Foo { private int _count; }
"#,
        );
        let active = result.symbols.iter().find(|s| s.name == "Active").unwrap();
        assert_eq!(active.kind, SymbolKind::EnumMember);
        assert_eq!(active.parent_id.as_deref(), Some("Test.cs:enum:Status"));
        let count = result.symbols.iter().find(|s| s.name == "_count").unwrap();
        assert_eq!(count.kind, SymbolKind::Variable);
    }

    #[test]
    fn test_delegate_is_function() {
        let result = extract("public delegate void Handler(string msg);");
        let d = result.symbols.iter().find(|s| s.name == "Handler").unwrap();
        assert_eq!(d.kind, SymbolKind::Function);
    }

    #[test]
    fn test_method_calls() {
        let result = extract(
            r#"
public class Foo {
    public void Process() {
        FetchData();
        Transform(data);
        _repo.Save(entity);
    }
}
"#,
        );
        let calls = edge_targets(&result, EdgeKind::Calls);
        assert!(calls.contains(&"FetchData".to_string()));
        assert!(calls.contains(&"Transform".to_string()));
        assert!(calls.contains(&"_repo.Save".to_string()));
    }

    #[test]
    fn test_using_import() {
        let result = extract(
            r#"
using System;
using System.Collections.Generic;

public class Foo {}
"#,
        );
        let imports: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);
        let targets = edge_targets(&result, EdgeKind::Imports);
        assert!(targets.contains(&"System".to_string()));
        assert!(targets.contains(&"Generic".to_string()));
    }

    #[test]
    fn test_global_using_import() {
        let result = extract("global using System.Text;\npublic class Foo {}");
        let targets = edge_targets(&result, EdgeKind::Imports);
        assert!(targets.contains(&"Text".to_string()));
    }

    #[test]
    fn test_visibility() {
        let result = extract(
            r#"
public class Foo {
    public void Pub() {}
    private void Priv() {}
    protected void Prot() {}
    void Internal() {}
}
"#,
        );
        assert_eq!(
            result
                .symbols
                .iter()
                .find(|s| s.name == "Pub")
                .unwrap()
                .visibility,
            Visibility::Public
        );
        assert_eq!(
            result
                .symbols
                .iter()
                .find(|s| s.name == "Priv")
                .unwrap()
                .visibility,
            Visibility::Private
        );
        assert_eq!(
            result
                .symbols
                .iter()
                .find(|s| s.name == "Prot")
                .unwrap()
                .visibility,
            Visibility::Protected
        );
        assert_eq!(
            result
                .symbols
                .iter()
                .find(|s| s.name == "Internal")
                .unwrap()
                .visibility,
            Visibility::Public
        );
    }

    #[test]
    fn test_async_modifier() {
        let result = extract(
            r#"
public class Foo {
    public async Task Run() {}
}
"#,
        );
        let run = result.symbols.iter().find(|s| s.name == "Run").unwrap();
        assert!(run.is_async);
    }

    #[test]
    fn test_docstring_xml() {
        let result = extract(
            r#"
/// <summary>
/// Processes the request.
/// </summary>
public class Processor { }
"#,
        );
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "Processor")
            .unwrap();
        assert_eq!(sym.docstring.as_deref(), Some("Processes the request."));
    }

    // ── D1: base_list edge semantics ──

    #[test]
    fn test_class_base_list_is_implements() {
        // Both the base class (BaseService) and the interface (IThing) become
        // Implements — the grammar cannot tell them apart (D1).
        let result = extract("public class Foo : BaseService, IThing { }");
        let inherits = edge_targets(&result, EdgeKind::Inherits);
        assert!(inherits.is_empty());
        let implements = edge_targets(&result, EdgeKind::Implements);
        assert!(implements.contains(&"BaseService".to_string()));
        assert!(implements.contains(&"IThing".to_string()));
    }

    #[test]
    fn test_interface_base_list_is_inherits() {
        let result = extract("public interface IUserRepo : IRepository, ICrud { }");
        let inherits = edge_targets(&result, EdgeKind::Inherits);
        assert!(inherits.contains(&"IRepository".to_string()));
        assert!(inherits.contains(&"ICrud".to_string()));
        assert!(edge_targets(&result, EdgeKind::Implements).is_empty());
    }

    #[test]
    fn test_generic_base_clean_name() {
        let result = extract("public class Foo : Repo<int> { }");
        let implements = edge_targets(&result, EdgeKind::Implements);
        assert!(implements.contains(&"Repo".to_string()));
        assert!(!implements.iter().any(|t| t.contains('<')));
    }

    // ── D3: namespaces, partial classes, generics, nested types, top-level ──

    #[test]
    fn test_file_scoped_namespace_qname() {
        let result = extract("namespace Web.Services;\npublic class AuthService { }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "AuthService")
            .unwrap();
        // The namespace folds into the stable id as a qname prefix (D2).
        assert_eq!(sym.id, "Test.cs:class:Web.Services.AuthService");
    }

    #[test]
    fn test_block_namespace_qname() {
        let result = extract("namespace Web.Handlers { public class LoginHandler { } }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "LoginHandler")
            .unwrap();
        assert_eq!(sym.id, "Test.cs:class:Web.Handlers.LoginHandler");
    }

    #[test]
    fn test_generic_class_clean_name() {
        let result = extract("public class Repo<T> where T : class { }");
        let sym = result.symbols.iter().find(|s| s.name == "Repo");
        assert!(sym.is_some(), "generic type name should be the clean base");
        assert!(!result.symbols.iter().any(|s| s.name.contains('<')));
    }

    #[test]
    fn test_nested_type_qname() {
        let result = extract("public class Outer { public class Inner { } }");
        let inner = result.symbols.iter().find(|s| s.name == "Inner").unwrap();
        assert_eq!(inner.id, "Test.cs:class:Outer.Inner");
        assert!(inner.parent_id.as_ref().unwrap().contains("Outer"));
    }

    #[test]
    fn test_partial_classes_separate_symbols() {
        // Two partial fragments in one file → two Class symbols (no merge). The
        // file:kind:qname stable ID means cross-file fragments stay separate too.
        let result = extract(
            r#"
public partial class Widget { public void A() {} }
public partial class Widget { public void B() {} }
"#,
        );
        let widgets: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.name == "Widget" && s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(widgets.len(), 2);
    }

    #[test]
    fn test_top_level_statements() {
        let result = extract(
            r#"
Console.WriteLine("hello");
var x = Compute();
"#,
        );
        // A synthetic Main anchors the top-level calls so no edge dangles.
        let main = result
            .symbols
            .iter()
            .find(|s| s.name == "Main" && s.kind == SymbolKind::Function)
            .expect("top-level statements synthesize an implicit Main");
        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert!(calls.iter().all(|e| e.source_id == main.id));
        let targets: Vec<&str> = calls.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"Console.WriteLine"));
        assert!(targets.contains(&"Compute"));
    }

    #[test]
    fn test_null_conditional_call_emits_edge() {
        let result = extract(
            r#"
public class Foo {
    public void Process() {
        _repo?.Save(entity);
    }
}
"#,
        );
        let calls = edge_targets(&result, EdgeKind::Calls);
        // `?.` and `.` must behave identically — receiver-qualified, not dropped.
        assert!(
            calls.contains(&"_repo.Save".to_string()),
            "null-conditional call must emit a Calls edge, got {calls:?}"
        );
    }

    #[test]
    fn test_generic_member_call_name_is_clean() {
        let result = extract(
            r#"
public class Foo {
    public void Process() {
        _repo.Get<User>();
    }
}
"#,
        );
        let calls = edge_targets(&result, EdgeKind::Calls);
        // The type argument must be stripped so the `.`-split resolver can match `Get`.
        assert!(
            calls.contains(&"_repo.Get".to_string()),
            "generic member call must drop the type argument, got {calls:?}"
        );
        assert!(!calls.iter().any(|c| c.contains('<')));
    }

    #[test]
    fn test_bare_generic_call_name_is_clean() {
        let result = extract(
            r#"
public class Foo {
    public void Process() {
        Get<User>();
    }
}
"#,
        );
        let calls = edge_targets(&result, EdgeKind::Calls);
        assert!(calls.contains(&"Get".to_string()), "got {calls:?}");
    }

    #[test]
    fn test_user_task_type_gets_reference_edge() {
        let result = extract(
            r#"
public class Foo {
    public Task Pending(Object o) { return null; }
}
"#,
        );
        let refs = edge_targets(&result, EdgeKind::References);
        // Task/Object are framework/user types, not language keywords — they must
        // still produce reference edges (unlike lowercase `object`/`string`).
        assert!(refs.contains(&"Task".to_string()), "got {refs:?}");
        assert!(refs.contains(&"Object".to_string()), "got {refs:?}");
    }

    #[test]
    fn test_top_level_local_function_is_own_symbol() {
        let result = extract(
            r#"
Helper();
void Helper() { DoWork(); }
"#,
        );
        let helper = result
            .symbols
            .iter()
            .find(|s| s.name == "Helper")
            .expect("a top-level local function must be its own symbol");
        // No enclosing type → no leading `.` in the id, and a NULL parent (not "").
        assert_eq!(
            helper.id, "Test.cs:method:Helper",
            "top-level local function id must have no leading `.`"
        );
        assert!(
            helper.parent_id.is_none(),
            "top-level local function must have no parent, got {:?}",
            helper.parent_id
        );
        // DoWork is called from Helper, not the synthetic Main.
        let dowork = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "DoWork")
            .expect("Helper's call to DoWork must be recorded");
        assert_eq!(
            dowork.source_id, helper.id,
            "DoWork must be attributed to Helper, not Main"
        );
    }

    #[test]
    fn test_implicit_main_span_covers_all_statements() {
        let result = extract("First();\nSecond();\nThird();\n");
        let main = result
            .symbols
            .iter()
            .find(|s| s.name == "Main")
            .expect("synthetic Main");
        // Span must reach the last statement, not stop at the first.
        assert!(
            main.end_line >= 3,
            "Main span should cover all top-level statements, end_line={}",
            main.end_line
        );
    }

    #[test]
    fn test_using_static_name_strips_keyword() {
        let result = extract("using static System.Math;\npublic class Foo {}");
        let import = result
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .expect("using static → an import symbol");
        assert_eq!(
            import.name, "System.Math",
            "`static` keyword must be stripped from the import name"
        );
    }

    #[test]
    fn test_docstring_keeps_angle_brackets_in_prose() {
        let result = extract(
            r#"
public class Foo {
    /// <summary>Returns true if count < 5 and value > 0.</summary>
    public bool Check() { return true; }
}
"#,
        );
        let check = result
            .symbols
            .iter()
            .find(|s| s.name == "Check")
            .expect("Check method");
        // A literal `<`/`>` in prose (not opening a tag) must survive; only
        // well-formed tags like `<summary>` are dropped.
        let doc = check.docstring.as_deref().unwrap_or("");
        assert!(
            doc.contains("count < 5") && doc.contains("value > 0"),
            "prose angle brackets must survive tag stripping, got {doc:?}"
        );
        assert!(
            !doc.contains("<summary>"),
            "real tags still dropped, got {doc:?}"
        );
    }

    #[test]
    fn test_docstring_survives_interposed_line_comment() {
        let result = extract(
            r#"
public class Foo {
    /// <summary>Does the thing.</summary>
    // implementation note
    public void Do() {}
}
"#,
        );
        let d = result
            .symbols
            .iter()
            .find(|s| s.name == "Do")
            .expect("Do method");
        assert_eq!(
            d.docstring.as_deref(),
            Some("Does the thing."),
            "a plain // comment between the /// block and the decl must not drop the summary"
        );
    }
}
