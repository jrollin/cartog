use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use crate::types::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{node_text, ExtractionResult, Extractor};

pub struct RustExtractor {
    language: Language,
}

impl RustExtractor {
    pub fn new() -> Self {
        Self {
            language: Language::new(tree_sitter_rust::LANGUAGE),
        }
    }
}

impl Default for RustExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for RustExtractor {
    fn extract(&self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&self.language)?;

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
        "function_item" => {
            extract_function(node, source, file_path, parent_id, symbols, edges);
        }
        "struct_item" => {
            extract_struct(node, source, file_path, parent_id, symbols);
        }
        "enum_item" => {
            extract_enum(node, source, file_path, parent_id, symbols);
        }
        "trait_item" => {
            extract_trait(node, source, file_path, parent_id, symbols);
        }
        "impl_item" => {
            extract_impl(node, source, file_path, parent_id, symbols, edges);
        }
        "use_declaration" => {
            extract_use(node, source, file_path, parent_id, symbols, edges);
        }
        "mod_item" => {
            extract_mod(node, source, file_path, parent_id, symbols, edges);
        }
        "const_item" | "static_item" => {
            extract_const(node, source, file_path, parent_id, symbols);
        }
        "type_item" => {
            extract_type_alias(node, source, file_path, parent_id, symbols);
        }
        "attribute_item" | "inner_attribute_item" => {
            // Skip attributes, but process the next sibling
        }
        _ => {
            for child in node.named_children(&mut node.walk()) {
                extract_node(child, source, file_path, parent_id, symbols, edges);
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

    let visibility = rust_visibility(node, source);
    let is_async = has_child_kind(node, "async");
    let signature = extract_fn_signature(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    symbols.push(
        Symbol::new(
            name,
            kind,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_signature(signature)
        .with_visibility(visibility)
        .with_async(is_async)
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
            SymbolKind::Class,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_visibility(visibility)
        .with_docstring(docstring),
    );
}

// ── Enums ──

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
    let visibility = rust_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Class,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
        )
        .with_parent(parent_id)
        .with_visibility(visibility)
        .with_docstring(docstring),
    );
}

// ── Traits ──

fn extract_trait(
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
    let visibility = rust_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Class,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
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
    let end_line = node.end_position().row as u32 + 1;
    let impl_parent_id = symbol_id(file_path, &impl_type, start_line);

    // Emit a Class symbol for the impl block so edges have a valid source_id
    symbols.push(Symbol::new(
        impl_type.clone(),
        SymbolKind::Class,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
    ));

    // Check if this is a trait impl: impl Trait for Type
    let trait_name = node
        .child_by_field_name("trait")
        .map(|t| extract_type_name(t, source));

    if let Some(ref trait_n) = trait_name {
        if !trait_n.is_empty() {
            edges.push(Edge::new(
                impl_parent_id.clone(),
                trait_n.clone(),
                EdgeKind::Inherits,
                file_path,
                start_line,
            ));
        }
    }

    // Walk impl body for methods
    if let Some(body) = node.child_by_field_name("body") {
        for child in body.named_children(&mut body.walk()) {
            if child.kind() == "function_item" {
                extract_function(
                    child,
                    source,
                    file_path,
                    Some(&impl_parent_id),
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

    let sym_id = symbol_id(file_path, &use_path, line);
    symbols.push(
        Symbol::new(
            use_path.clone(),
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
    text.rsplit("::").next().unwrap_or(text).to_string()
}

// ── Mod items ──

fn extract_mod(
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
    let visibility = rust_visibility(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);

    // Only emit a symbol if it has a body (inline module)
    if let Some(body) = node.child_by_field_name("body") {
        symbols.push(
            Symbol::new(
                name,
                SymbolKind::Class,
                file_path,
                start_line,
                node.end_position().row as u32 + 1,
                node.start_byte() as u32,
                node.end_byte() as u32,
            )
            .with_parent(parent_id)
            .with_visibility(visibility),
        );

        for child in body.named_children(&mut body.walk()) {
            extract_node(child, source, file_path, Some(&sym_id), symbols, edges);
        }
    }
}

// ── Constants / statics ──

fn extract_const(
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
    let visibility = rust_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Variable,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
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
            SymbolKind::Variable,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
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
        if let Some(child) = node.child(i) {
            if child.kind() == kind {
                return true;
            }
            // Check inside function_modifiers for async/const/unsafe
            if child.kind() == "function_modifiers" {
                for j in 0..child.child_count() {
                    if let Some(gc) = child.child(j) {
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
        if let Some(child) = node.child(i) {
            if child.kind() == "visibility_modifier" {
                let text = node_text(child, source);
                if text.contains("pub") {
                    return Visibility::Public;
                }
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
        let ext = RustExtractor::new();
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

        let s = result.symbols.iter().find(|s| s.name == "UserService");
        assert!(s.is_some());
        assert_eq!(s.unwrap().kind, SymbolKind::Class);

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
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].target_name, "Serializable");
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
        assert_eq!(e.unwrap().kind, SymbolKind::Class);
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
        assert_eq!(crate_fn.unwrap().visibility, Visibility::Public);
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
}
