use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use crate::types::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{node_text, ExtractionResult, Extractor};

pub struct GoExtractor {
    language: Language,
}

impl GoExtractor {
    pub fn new() -> Self {
        Self {
            language: Language::new(tree_sitter_go::LANGUAGE),
        }
    }
}

impl Default for GoExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for GoExtractor {
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
        "function_declaration" => {
            extract_function(node, source, file_path, parent_id, symbols, edges);
        }
        "method_declaration" => {
            extract_method(node, source, file_path, symbols, edges);
        }
        "type_declaration" => {
            extract_type_declaration(node, source, file_path, parent_id, symbols, edges);
        }
        "import_declaration" => {
            extract_import(node, source, file_path, parent_id, symbols, edges);
        }
        "const_declaration" => {
            extract_const(node, source, file_path, parent_id, symbols);
        }
        "var_declaration" => {
            extract_var(node, source, file_path, parent_id, symbols);
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
    let visibility = go_visibility(&name);
    let signature = extract_fn_signature(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    let mut sym = Symbol::new(
        name,
        SymbolKind::Function,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
    )
    .with_parent(parent_id)
    .with_signature(signature)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Extract type references from parameter and return types
    extract_fn_type_refs(node, source, file_path, &sym_id, edges);

    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, &sym_id, edges);
    }
}

// ── Methods ──

fn extract_method(
    node: Node,
    source: &str,
    file_path: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    // Extract receiver type for parent linkage.
    // NOTE: parent_id uses format "file_path:type_name" which doesn't match the
    // symbol_id format "file_path:name:line". This means parent linkage for methods
    // won't resolve to the struct symbol via direct id match. Edge resolution by
    // name (db.resolve_edges) handles cross-symbol references instead.
    let receiver_type = extract_receiver_type(node, source);
    let parent_id = receiver_type.as_ref().map(|rt| format!("{file_path}:{rt}"));

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = go_visibility(&name);
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
    .with_parent(parent_id.as_deref())
    .with_signature(signature)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Extract type references from parameter and return types
    extract_fn_type_refs(node, source, file_path, &sym_id, edges);

    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, &sym_id, edges);
    }
}

/// Extract the receiver type name from a method declaration.
/// `func (s *Server) Handle()` → "Server"
fn extract_receiver_type(node: Node, source: &str) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    for child in receiver.named_children(&mut receiver.walk()) {
        if child.kind() == "parameter_declaration" {
            // The type is the last named child (may be pointer_type or type_identifier)
            if let Some(type_node) = child.child_by_field_name("type") {
                return Some(extract_type_name(type_node, source));
            }
            // Fallback: last named child
            let count = child.named_child_count();
            if count > 0 {
                if let Some(last) = child.named_child(count - 1) {
                    return Some(extract_type_name(last, source));
                }
            }
        }
    }
    None
}

// ── Type declarations (struct, interface, type alias) ──

fn extract_type_declaration(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // type_declaration can contain one type_spec or a type_spec_list
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "type_spec" {
            extract_type_spec(child, source, file_path, parent_id, symbols, edges);
        }
    }
}

fn extract_type_spec(
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
    let visibility = go_visibility(&name);
    let docstring = extract_doc_comment(node.parent().unwrap_or(node), source);

    let type_node = node.child_by_field_name("type");

    let kind = match type_node.map(|t| t.kind()) {
        Some("struct_type") | Some("interface_type") => SymbolKind::Class,
        _ => SymbolKind::Variable, // type alias
    };

    let sym_id = symbol_id(file_path, &name, start_line);
    let mut sym = Symbol::new(
        name.clone(),
        kind,
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

    // For interfaces, extract embedded types as "inherits" edges
    if let Some(type_n) = type_node {
        if type_n.kind() == "interface_type" {
            extract_interface_embeds(type_n, source, file_path, &sym_id, start_line, edges);
        }
    }
}

/// Extract embedded interfaces from an interface type.
/// Walks all descendants looking for embedded type identifiers (not method specs).
fn extract_interface_embeds(
    node: Node,
    source: &str,
    file_path: &str,
    parent_sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            // Direct embedded type at any nesting level
            "type_identifier" | "qualified_type" => {
                let embedded = node_text(child, source).to_string();
                if !embedded.is_empty() {
                    edges.push(Edge::new(
                        parent_sym_id,
                        embedded,
                        EdgeKind::Inherits,
                        file_path,
                        line,
                    ));
                }
            }
            // Recurse into method_spec_list or other container nodes
            // but skip method_spec (those are method declarations, not embeds)
            "method_spec" => {}
            _ => {
                extract_interface_embeds(child, source, file_path, parent_sym_id, line, edges);
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
    // Single import: import_declaration → import_spec
    // Grouped import: import_declaration → import_spec_list → import_spec*
    let mut specs = Vec::new();
    collect_import_specs(node, &mut specs);

    for spec in specs {
        extract_import_spec(spec, source, file_path, parent_id, symbols, edges);
    }
}

fn collect_import_specs<'a>(node: Node<'a>, specs: &mut Vec<Node<'a>>) {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "import_spec" {
            specs.push(child);
        } else if child.kind() == "import_spec_list" {
            collect_import_specs(child, specs);
        }
    }
}

fn extract_import_spec(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    let import_text = node_text(node, source).to_string();

    // Extract the import path (the string literal)
    let path_str = extract_import_path(node, source);
    if path_str.is_empty() {
        return;
    }

    // Use the last segment of the path as the imported name
    let pkg_name = path_str.rsplit('/').next().unwrap_or(&path_str);

    let sym_id = symbol_id(file_path, &path_str, line);
    symbols.push(
        Symbol::new(
            path_str.clone(),
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

    edges.push(Edge::new(
        sym_id,
        pkg_name,
        EdgeKind::Imports,
        file_path,
        line,
    ));
}

/// Extract the path string from an import spec, stripping quotes.
fn extract_import_path(node: Node, source: &str) -> String {
    let path_node = node.child_by_field_name("path");
    let text = match path_node {
        Some(p) => node_text(p, source),
        None => {
            // Fallback: find the string literal child
            for child in node.named_children(&mut node.walk()) {
                let kind = child.kind();
                if kind == "interpreted_string_literal" || kind == "raw_string_literal" {
                    return strip_string_quotes(node_text(child, source));
                }
            }
            return String::new();
        }
    };
    strip_string_quotes(text)
}

fn strip_string_quotes(s: &str) -> String {
    s.trim_matches('"').trim_matches('`').to_string()
}

// ── Constants ──

fn extract_const(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    // const_declaration → const_spec | const_spec_list → const_spec*
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "const_spec" => {
                extract_const_spec(child, source, file_path, parent_id, symbols);
            }
            "const_spec_list" => {
                for spec in child.named_children(&mut child.walk()) {
                    if spec.kind() == "const_spec" {
                        extract_const_spec(spec, source, file_path, parent_id, symbols);
                    }
                }
            }
            _ => {}
        }
    }
}

fn extract_const_spec(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    // const_spec may have multiple identifiers: `const A, B = 1, 2`
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "identifier" {
            let name = node_text(child, source).to_string();
            let line = child.start_position().row as u32 + 1;
            let visibility = go_visibility(&name);

            let mut sym = Symbol::new(
                name,
                SymbolKind::Variable,
                file_path,
                line,
                node.end_position().row as u32 + 1,
                child.start_byte() as u32,
                child.end_byte() as u32,
            )
            .with_parent(parent_id);
            if visibility != Visibility::Public {
                sym = sym.with_visibility(visibility);
            }
            symbols.push(sym);
        }
    }
}

// ── Variables ──

fn extract_var(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "var_spec" => {
                extract_var_spec(child, source, file_path, parent_id, symbols);
            }
            "var_spec_list" => {
                for spec in child.named_children(&mut child.walk()) {
                    if spec.kind() == "var_spec" {
                        extract_var_spec(spec, source, file_path, parent_id, symbols);
                    }
                }
            }
            _ => {}
        }
    }
}

fn extract_var_spec(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "identifier" {
            let name = node_text(child, source).to_string();
            let line = child.start_position().row as u32 + 1;
            let visibility = go_visibility(&name);

            let mut sym = Symbol::new(
                name,
                SymbolKind::Variable,
                file_path,
                line,
                node.end_position().row as u32 + 1,
                child.start_byte() as u32,
                child.end_byte() as u32,
            )
            .with_parent(parent_id);
            if visibility != Visibility::Public {
                sym = sym.with_visibility(visibility);
            }
            symbols.push(sym);
        }
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
                "call_expression" => {
                    if let Some(func) = current.child_by_field_name("function") {
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
                "composite_literal" => {
                    // MyStruct{field: val} — the type is a reference
                    if let Some(type_node) = current.child_by_field_name("type") {
                        let type_name = extract_type_name(type_node, source);
                        if !type_name.is_empty()
                            && type_name.chars().next().is_some_and(|c| c.is_uppercase())
                        {
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
                // Don't descend into nested function literals
                "func_literal" => {
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
    // Return type (Go calls this "result")
    if let Some(ret) = node.child_by_field_name("result") {
        collect_type_refs_recursive(ret, source, file_path, sym_id, edges);
    }
}

/// Recursively walk a subtree collecting type_identifier references.
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
            // Go: exported types start with uppercase, skip builtins (int, string, bool, error, etc.)
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
        "qualified_type" => {
            // pkg.Type — extract the type part
            let name = extract_type_name(node, source);
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
        _ => {
            for child in node.named_children(&mut node.walk()) {
                collect_type_refs_recursive(child, source, file_path, sym_id, edges);
            }
        }
    }
}

// ── Helpers ──

/// Go visibility: exported names start with uppercase.
fn go_visibility(name: &str) -> Visibility {
    if name.starts_with(|c: char| c.is_uppercase()) {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

/// Extract the type name, stripping pointer indirection.
fn extract_type_name(node: Node, source: &str) -> String {
    match node.kind() {
        "pointer_type" => {
            // *Type → "Type"
            node.named_child(0)
                .map(|c| extract_type_name(c, source))
                .unwrap_or_default()
        }
        "type_identifier" | "identifier" => node_text(node, source).to_string(),
        "qualified_type" => {
            // pkg.Type → "Type" (just the type part)
            if let Some(name) = node.child_by_field_name("name") {
                node_text(name, source).to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        "generic_type" => {
            if let Some(t) = node.child_by_field_name("type") {
                extract_type_name(t, source)
            } else {
                node.named_child(0)
                    .map(|c| node_text(c, source).to_string())
                    .unwrap_or_default()
            }
        }
        _ => node_text(node, source).to_string(),
    }
}

/// Build function signature from parameter list and return type.
fn extract_fn_signature(node: Node, source: &str) -> Option<String> {
    let params = node.child_by_field_name("parameters")?;
    let params_text = node_text(params, source);

    let return_text = node.child_by_field_name("result").map(|r| {
        let text = node_text(r, source);
        format!(" {text}")
    });

    Some(format!("{params_text}{}", return_text.unwrap_or_default()))
}

/// Build method signature including receiver.
fn extract_method_signature(node: Node, source: &str) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let receiver_text = node_text(receiver, source);

    let params = node.child_by_field_name("parameters")?;
    let params_text = node_text(params, source);

    let return_text = node.child_by_field_name("result").map(|r| {
        let text = node_text(r, source);
        format!(" {text}")
    });

    Some(format!(
        "{receiver_text} {params_text}{}",
        return_text.unwrap_or_default()
    ))
}

/// Extract doc comment from the preceding comment block.
fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut prev = node.prev_sibling();

    while let Some(p) = prev {
        if p.kind() == "comment" {
            let text = node_text(p, source);
            if let Some(stripped) = text.strip_prefix("//") {
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
        let ext = GoExtractor::new();
        ext.extract(source, "test.go").unwrap()
    }

    #[test]
    fn test_function() {
        let result = extract(
            r#"package main

// Greet returns a greeting message.
func Greet(name string) string {
    return "Hello, " + name
}
"#,
        );

        let func = result.symbols.iter().find(|s| s.name == "Greet");
        assert!(func.is_some());
        let f = func.unwrap();
        assert_eq!(f.kind, SymbolKind::Function);
        assert_eq!(f.visibility, Visibility::Public);
        assert_eq!(f.signature.as_deref(), Some("(name string) string"));
        assert_eq!(
            f.docstring.as_deref(),
            Some("Greet returns a greeting message.")
        );
    }

    #[test]
    fn test_private_function() {
        let result = extract(
            r#"package main

func helper() {}
"#,
        );

        let func = result.symbols.iter().find(|s| s.name == "helper");
        assert!(func.is_some());
        assert_eq!(func.unwrap().visibility, Visibility::Private);
    }

    #[test]
    fn test_method() {
        let result = extract(
            r#"package main

type Server struct {
    port int
}

func (s *Server) Start() error {
    return nil
}

func (s Server) Port() int {
    return s.port
}
"#,
        );

        let start = result.symbols.iter().find(|s| s.name == "Start");
        assert!(start.is_some());
        let m = start.unwrap();
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.visibility, Visibility::Public);
        assert!(m.parent_id.is_some());
        assert!(m.parent_id.as_ref().unwrap().contains("Server"));

        let port = result.symbols.iter().find(|s| s.name == "Port");
        assert!(port.is_some());
        assert_eq!(port.unwrap().kind, SymbolKind::Method);
    }

    #[test]
    fn test_struct() {
        let result = extract(
            r#"package main

// UserService handles user operations.
type UserService struct {
    db Database
    cache Cache
}
"#,
        );

        let s = result.symbols.iter().find(|s| s.name == "UserService");
        assert!(s.is_some());
        let sym = s.unwrap();
        assert_eq!(sym.kind, SymbolKind::Class);
        assert_eq!(sym.visibility, Visibility::Public);
        assert_eq!(
            sym.docstring.as_deref(),
            Some("UserService handles user operations.")
        );
    }

    #[test]
    fn test_interface() {
        let result = extract(
            r#"package main

type Reader interface {
    Read(p []byte) (n int, err error)
}
"#,
        );

        let iface = result.symbols.iter().find(|s| s.name == "Reader");
        assert!(iface.is_some());
        assert_eq!(iface.unwrap().kind, SymbolKind::Class);
    }

    #[test]
    fn test_interface_embedding() {
        let result = extract(
            r#"package main

type ReadWriter interface {
    Reader
    Writer
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
        assert!(targets.contains(&"Reader"));
        assert!(targets.contains(&"Writer"));
    }

    #[test]
    fn test_imports() {
        let result = extract(
            r#"package main

import (
    "fmt"
    "net/http"
    "os"
)
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
        assert_eq!(import_edges.len(), 3);

        let targets: Vec<&str> = import_edges
            .iter()
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(targets.contains(&"fmt"));
        assert!(targets.contains(&"http"));
        assert!(targets.contains(&"os"));
    }

    #[test]
    fn test_single_import() {
        let result = extract(
            r#"package main

import "fmt"
"#,
        );

        let imports: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "fmt");
    }

    #[test]
    fn test_function_calls() {
        let result = extract(
            r#"package main

func process() {
    data := fetchData()
    result := transform(data)
    save(result)
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
    fn test_method_calls() {
        let result = extract(
            r#"package main

func (s *Server) Handle(req Request) {
    s.validate(req)
    fmt.Println("handled")
}
"#,
        );

        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert!(calls.len() >= 2);

        let targets: Vec<&str> = calls.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"s.validate"));
        assert!(targets.contains(&"fmt.Println"));
    }

    #[test]
    fn test_constants() {
        let result = extract(
            r#"package main

const MaxRetries = 3

const (
    StatusOK    = 200
    StatusError = 500
)
"#,
        );

        let vars: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Variable)
            .collect();
        assert_eq!(vars.len(), 3);

        let names: Vec<&str> = vars.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MaxRetries"));
        assert!(names.contains(&"StatusOK"));
        assert!(names.contains(&"StatusError"));
    }

    #[test]
    fn test_variables() {
        let result = extract(
            r#"package main

var DefaultTimeout = 30

var (
    ErrNotFound = errors.New("not found")
    ErrTimeout  = errors.New("timeout")
)
"#,
        );

        let vars: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Variable)
            .collect();
        assert_eq!(vars.len(), 3);
    }

    #[test]
    fn test_visibility() {
        let result = extract(
            r#"package main

func PublicFunc() {}
func privateFunc() {}

type ExportedType struct {}
type unexportedType struct {}
"#,
        );

        let public_fn = result.symbols.iter().find(|s| s.name == "PublicFunc");
        assert_eq!(public_fn.unwrap().visibility, Visibility::Public);

        let private_fn = result.symbols.iter().find(|s| s.name == "privateFunc");
        assert_eq!(private_fn.unwrap().visibility, Visibility::Private);

        let exported = result.symbols.iter().find(|s| s.name == "ExportedType");
        assert_eq!(exported.unwrap().visibility, Visibility::Public);

        let unexported = result.symbols.iter().find(|s| s.name == "unexportedType");
        assert_eq!(unexported.unwrap().visibility, Visibility::Private);
    }

    #[test]
    fn test_type_annotation_refs() {
        let result = extract(
            r#"package main

func Process(user User, count int) Response {
    return Response{}
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
        // int is lowercase → not captured
        assert!(!targets.contains(&"int"));
    }

    #[test]
    fn test_composite_literal_refs() {
        let result = extract(
            r#"package main

func create() {
    user := User{Name: "Alice"}
    cfg := Config{Debug: true}
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
    fn test_empty_file() {
        // Go requires package declaration, but empty string should not panic
        let result = extract("");
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn test_syntax_error_partial_parse() {
        let result = extract("package main\n\nfunc broken( {}\n");
        let _ = result.symbols.len();
    }
}
