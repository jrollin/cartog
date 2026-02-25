use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use crate::types::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{node_text, ExtractionResult, Extractor};

pub struct PythonExtractor {
    language: Language,
}

impl PythonExtractor {
    pub fn new() -> Self {
        Self {
            language: Language::new(tree_sitter_python::LANGUAGE),
        }
    }
}

impl Default for PythonExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for PythonExtractor {
    fn extract(&self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&self.language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {file_path}"))?;

        let mut symbols = Vec::new();
        let mut edges = Vec::new();

        let root = tree.root_node();
        extract_node(
            root,
            source,
            file_path,
            None, // no parent
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
        "function_definition" => {
            extract_function(node, source, file_path, parent_id, symbols, edges);
        }
        "class_definition" => {
            extract_class(node, source, file_path, parent_id, symbols, edges);
        }
        "decorated_definition" => {
            // Find the actual definition first to compute its symbol ID for decorator edges
            let mut def_sym_id = None;
            for child in node.named_children(&mut node.walk()) {
                if child.kind() == "function_definition" || child.kind() == "class_definition" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = node_text(name_node, source);
                        let line = child.start_position().row as u32 + 1;
                        def_sym_id = Some(symbol_id(file_path, name, line));
                    }
                }
            }
            // Extract decorator references using the decorated symbol as source
            for child in node.named_children(&mut node.walk()) {
                if child.kind() == "decorator" {
                    extract_decorator_ref(child, source, file_path, def_sym_id.as_deref(), edges);
                } else if child.kind() == "function_definition"
                    || child.kind() == "class_definition"
                {
                    extract_node(child, source, file_path, parent_id, symbols, edges);
                }
            }
        }
        "import_statement" | "import_from_statement" => {
            extract_import(node, source, file_path, parent_id, symbols, edges);
        }
        "expression_statement" => {
            for child in node.named_children(&mut node.walk()) {
                if child.kind() == "assignment" {
                    extract_assignment(child, source, file_path, parent_id, symbols);
                }
            }
            // Still walk children for call expressions
            walk_for_calls_and_raises(node, source, file_path, parent_id, edges);
        }
        _ => {
            // Recurse into children
            for child in node.named_children(&mut node.walk()) {
                extract_node(child, source, file_path, parent_id, symbols, edges);
            }
        }
    }
}

fn extract_function(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name_ref = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source),
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

    let visibility = python_visibility(name_ref);
    let is_async = node
        .prev_named_sibling()
        .is_some_and(|s| s.kind() == "async")
        || node_text_slice(
            source,
            node.start_byte(),
            node.start_byte().saturating_add(6),
        )
        .starts_with("async ");

    // Check parent for async keyword (async def syntax produces a different tree)
    let is_async = is_async
        || node.parent().is_some_and(|_p| {
            node_text_slice(
                source,
                node.start_byte().saturating_sub(6),
                node.start_byte(),
            )
            .contains("async")
        });
    let name = name_ref.to_string();

    let signature = extract_signature(node, source);
    let docstring = extract_docstring(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    let mut sym = Symbol::new(
        &name,
        kind,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
    )
    .with_parent(parent_id)
    .with_signature(signature);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    if is_async {
        sym = sym.with_async(true);
    }
    sym = sym.with_docstring(docstring);
    symbols.push(sym);

    // Extract type annotation references from parameters and return type
    extract_fn_type_refs(node, source, file_path, &sym_id, edges);

    // Walk the function body for calls, raises, etc.
    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls_and_raises(body, source, file_path, Some(&sym_id), edges);
        // Recurse for nested functions/classes
        for child in body.named_children(&mut body.walk()) {
            match child.kind() {
                "function_definition" | "class_definition" | "decorated_definition" => {
                    extract_node(child, source, file_path, Some(&sym_id), symbols, edges);
                }
                _ => {}
            }
        }
    }
}

fn extract_class(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name_ref = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = python_visibility(name_ref);
    let docstring = extract_docstring(node, source);
    let name = name_ref.to_string();

    let sym_id = symbol_id(file_path, &name, start_line);
    let mut sym = Symbol::new(
        &name,
        SymbolKind::Class,
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

    // Extract base classes → inherits edges
    if let Some(args) = node.child_by_field_name("superclasses") {
        for child in args.named_children(&mut args.walk()) {
            let base_name = node_text(child, source);
            if !base_name.is_empty() {
                edges.push(Edge::new(
                    sym_id.clone(),
                    base_name,
                    EdgeKind::Inherits,
                    file_path,
                    child.start_position().row as u32 + 1,
                ));
            }
        }
    }

    // Walk class body for methods, nested classes, assignments
    if let Some(body) = node.child_by_field_name("body") {
        for child in body.named_children(&mut body.walk()) {
            extract_node(child, source, file_path, Some(&sym_id), symbols, edges);
        }
    }
}

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

    let module_name = extract_import_module(node, source);
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

    // Create import edges for each imported name
    let names = extract_imported_names(node, source);
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

fn extract_assignment(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    // Only extract simple name = value assignments (not unpacking, subscript, etc.)
    if let Some(left) = node.child_by_field_name("left") {
        if left.kind() == "identifier" {
            let name_ref = node_text(left, source);
            let line = node.start_position().row as u32 + 1;
            let visibility = python_visibility(name_ref);
            let name = name_ref.to_string();

            let mut sym = Symbol::new(
                &name,
                SymbolKind::Variable,
                file_path,
                line,
                node.end_position().row as u32 + 1,
                node.start_byte() as u32,
                node.end_byte() as u32,
            )
            .with_parent(parent_id);
            if visibility != Visibility::Public {
                sym = sym.with_visibility(visibility);
            }
            symbols.push(sym);
        }
    }
}

/// Walk a subtree looking for call expressions and raise statements.
fn walk_for_calls_and_raises(
    node: Node,
    source: &str,
    file_path: &str,
    context_id: Option<&str>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    let mut did_visit_children = false;

    loop {
        let current = cursor.node();

        if !did_visit_children {
            match current.kind() {
                "call" => {
                    if let Some(ctx) = context_id {
                        if let Some(func) = current.child_by_field_name("function") {
                            let callee_name = node_text(func, source);
                            if !callee_name.is_empty() {
                                edges.push(Edge::new(
                                    ctx,
                                    callee_name,
                                    EdgeKind::Calls,
                                    file_path,
                                    current.start_position().row as u32 + 1,
                                ));
                            }
                        }
                    }
                }
                "raise_statement" => {
                    if let Some(ctx) = context_id {
                        if let Some(exc) = current.named_child(0) {
                            let exc_name = if exc.kind() == "call" {
                                exc.child_by_field_name("function")
                                    .map(|f| node_text(f, source))
                                    .unwrap_or("")
                            } else {
                                node_text(exc, source)
                            };
                            if !exc_name.is_empty() {
                                edges.push(Edge::new(
                                    ctx,
                                    exc_name,
                                    EdgeKind::Raises,
                                    file_path,
                                    current.start_position().row as u32 + 1,
                                ));
                            }
                        }
                    }
                }
                "except_clause" => {
                    // except ValueError as e: — extract exception type reference
                    if let Some(ctx) = context_id {
                        for child in current.named_children(&mut current.walk()) {
                            if child.kind() == "identifier" || child.kind() == "attribute" {
                                let type_name = node_text(child, source);
                                if !type_name.is_empty()
                                    && type_name.chars().next().is_some_and(|c| c.is_uppercase())
                                {
                                    edges.push(Edge::new(
                                        ctx,
                                        type_name,
                                        EdgeKind::References,
                                        file_path,
                                        child.start_position().row as u32 + 1,
                                    ));
                                }
                                break; // only the first identifier/attribute is the exception type
                            }
                            // except (TypeError, ValueError):
                            if child.kind() == "tuple" {
                                for tc in child.named_children(&mut child.walk()) {
                                    let type_name = node_text(tc, source);
                                    if !type_name.is_empty() {
                                        edges.push(Edge::new(
                                            ctx,
                                            type_name,
                                            EdgeKind::References,
                                            file_path,
                                            tc.start_position().row as u32 + 1,
                                        ));
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
                // Don't descend into nested function/class definitions
                "function_definition" | "class_definition" => {
                    did_visit_children = true;
                    continue;
                }
                _ => {}
            }
        }

        // Tree walking logic
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

// ── Reference helpers ──

/// Extract type annotation references from function parameters and return type.
fn extract_fn_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    // Parameter type annotations
    if let Some(params) = node.child_by_field_name("parameters") {
        for param in params.named_children(&mut params.walk()) {
            // typed_parameter, typed_default_parameter
            if let Some(type_node) = param.child_by_field_name("type") {
                collect_type_refs(type_node, source, file_path, sym_id, edges);
            }
        }
    }
    // Return type annotation
    if let Some(ret) = node.child_by_field_name("return_type") {
        collect_type_refs(ret, source, file_path, sym_id, edges);
    }
}

/// Recursively collect type name references from a type annotation node.
fn collect_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    match node.kind() {
        "identifier" => {
            let name = node_text(node, source);
            // Skip builtins and lowercase names (int, str, bool, etc.)
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
        "attribute" => {
            // e.g. typing.Optional — emit the full dotted name
            let name = node_text(node, source);
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
        // For subscript types like Optional[str], List[int], Dict[str, int]
        // recurse into children to capture the outer type and inner types
        _ => {
            for child in node.named_children(&mut node.walk()) {
                collect_type_refs(child, source, file_path, sym_id, edges);
            }
        }
    }
}

/// Extract a decorator as a reference edge.
fn extract_decorator_ref(
    node: Node,
    source: &str,
    file_path: &str,
    context_id: Option<&str>,
    edges: &mut Vec<Edge>,
) {
    let Some(ctx) = context_id else { return };
    // Decorator node children: "@" + expression
    // The expression can be identifier, attribute, or call
    for child in node.named_children(&mut node.walk()) {
        let name = match child.kind() {
            "identifier" | "attribute" => node_text(child, source).to_string(),
            "call" => child
                .child_by_field_name("function")
                .map(|f| node_text(f, source).to_string())
                .unwrap_or_default(),
            _ => continue,
        };
        if !name.is_empty() {
            edges.push(Edge::new(
                ctx,
                name,
                EdgeKind::References,
                file_path,
                node.start_position().row as u32 + 1,
            ));
        }
    }
}

// ── Helpers ──

fn node_text_slice(source: &str, start: usize, end: usize) -> &str {
    source
        .get(start.min(source.len())..end.min(source.len()))
        .unwrap_or("")
}

fn python_visibility(name: &str) -> Visibility {
    if name.starts_with("__") && name.ends_with("__") {
        // Dunder methods like __init__, __str__ are public
        Visibility::Public
    } else if name.starts_with("__") {
        Visibility::Private
    } else if name.starts_with('_') {
        Visibility::Protected
    } else {
        Visibility::Public
    }
}

fn extract_signature(node: Node, source: &str) -> Option<String> {
    let params_text = node_text(node.child_by_field_name("parameters")?, source);

    let return_type = node
        .child_by_field_name("return_type")
        .map(|r| format!(" -> {}", node_text(r, source)));

    Some(format!("{params_text}{}", return_type.unwrap_or_default()))
}

fn extract_docstring(node: Node, source: &str) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let first = body.named_child(0)?;

    if first.kind() != "expression_statement" {
        return None;
    }

    let expr = first.named_child(0)?;
    if expr.kind() != "string" {
        return None;
    }

    let text = node_text(expr, source);
    let inner = text
        .strip_prefix("\"\"\"")
        .and_then(|s| s.strip_suffix("\"\"\""))
        .or_else(|| text.strip_prefix("'''").and_then(|s| s.strip_suffix("'''")))?;

    let trimmed = inner.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn extract_import_module(node: Node, source: &str) -> String {
    match node.kind() {
        "import_statement" => {
            for child in node.named_children(&mut node.walk()) {
                if child.kind() == "dotted_name" {
                    return node_text(child, source).to_string();
                }
                if child.kind() == "aliased_import" {
                    if let Some(name) = child.child_by_field_name("name") {
                        return node_text(name, source).to_string();
                    }
                }
            }
            String::new()
        }
        "import_from_statement" => {
            if let Some(module) = node.child_by_field_name("module_name") {
                node_text(module, source).to_string()
            } else {
                for child in node.named_children(&mut node.walk()) {
                    if child.kind() == "dotted_name" || child.kind() == "relative_import" {
                        return node_text(child, source).to_string();
                    }
                }
                String::new()
            }
        }
        _ => String::new(),
    }
}

fn extract_imported_names(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();

    match node.kind() {
        "import_statement" => {
            for child in node.named_children(&mut node.walk()) {
                if child.kind() == "dotted_name" {
                    names.push(node_text(child, source).to_string());
                } else if child.kind() == "aliased_import" {
                    if let Some(name) = child.child_by_field_name("name") {
                        names.push(node_text(name, source).to_string());
                    }
                }
            }
        }
        "import_from_statement" => {
            for child in node.named_children(&mut node.walk()) {
                match child.kind() {
                    "dotted_name" => {
                        // Skip the module name (first dotted_name)
                    }
                    "aliased_import" => {
                        if let Some(name) = child.child_by_field_name("name") {
                            names.push(node_text(name, source).to_string());
                        }
                    }
                    _ => {}
                }
            }
            if names.is_empty() {
                let mut past_import = false;
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        if child.kind() == "import" {
                            past_import = true;
                            continue;
                        }
                        if past_import
                            && (child.kind() == "dotted_name" || child.kind() == "identifier")
                        {
                            names.push(node_text(child, source).to_string());
                        }
                    }
                }
            }
        }
        _ => {}
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> ExtractionResult {
        let ext = PythonExtractor::new();
        ext.extract(source, "test.py").unwrap()
    }

    #[test]
    fn test_simple_function() {
        let result = extract(
            r#"
def hello(name: str) -> str:
    """Greet someone."""
    return f"Hello, {name}!"
"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "hello");
        assert_eq!(result.symbols[0].kind, SymbolKind::Function);
        assert_eq!(
            result.symbols[0].signature.as_deref(),
            Some("(name: str) -> str")
        );
        assert_eq!(
            result.symbols[0].docstring.as_deref(),
            Some("Greet someone.")
        );
    }

    #[test]
    fn test_class_with_methods() {
        let result = extract(
            r#"
class UserService:
    """Manages users."""

    def __init__(self, db):
        self.db = db

    def get_user(self, user_id: int) -> User:
        return self.db.find(user_id)

    def _internal_method(self):
        pass
"#,
        );

        let class = result.symbols.iter().find(|s| s.name == "UserService");
        assert!(class.is_some());
        assert_eq!(class.unwrap().kind, SymbolKind::Class);

        let init = result.symbols.iter().find(|s| s.name == "__init__");
        assert!(init.is_some());
        assert_eq!(init.unwrap().kind, SymbolKind::Method);
        assert!(init.unwrap().parent_id.is_some());

        let internal = result.symbols.iter().find(|s| s.name == "_internal_method");
        assert!(internal.is_some());
        assert_eq!(internal.unwrap().visibility, Visibility::Protected);
    }

    #[test]
    fn test_inheritance() {
        let result = extract(
            r#"
class AdminService(UserService, BaseService):
    pass
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
        assert!(targets.contains(&"BaseService"));
    }

    #[test]
    fn test_function_calls() {
        let result = extract(
            r#"
def process():
    data = fetch_data()
    result = transform(data)
    save(result)
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
    fn test_imports() {
        let result = extract(
            r#"
import os
from pathlib import Path
from typing import Optional, List
"#,
        );

        let imports: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert!(!imports.is_empty());

        let import_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();
        assert!(!import_edges.is_empty());
    }

    #[test]
    fn test_raises() {
        let result = extract(
            r#"
def validate(x):
    if x < 0:
        raise ValueError("negative")
    if x > 100:
        raise RuntimeError("too large")
"#,
        );

        let raises: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Raises)
            .collect();
        assert_eq!(raises.len(), 2);
    }

    #[test]
    fn test_private_naming() {
        let result = extract(
            r#"
class Foo:
    def public_method(self): pass
    def _protected_method(self): pass
    def __private_method(self): pass
    def __dunder__(self): pass
"#,
        );

        let public = result.symbols.iter().find(|s| s.name == "public_method");
        assert_eq!(public.unwrap().visibility, Visibility::Public);

        let protected = result
            .symbols
            .iter()
            .find(|s| s.name == "_protected_method");
        assert_eq!(protected.unwrap().visibility, Visibility::Protected);

        let private = result.symbols.iter().find(|s| s.name == "__private_method");
        assert_eq!(private.unwrap().visibility, Visibility::Private);

        let dunder = result.symbols.iter().find(|s| s.name == "__dunder__");
        assert_eq!(dunder.unwrap().visibility, Visibility::Public);
    }

    #[test]
    fn test_empty_file() {
        let result = extract("");
        assert!(result.symbols.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_syntax_error_partial_parse() {
        // Tree-sitter is error-tolerant — should not panic, may return partial results
        let result = extract("def broken(:\n    pass");
        // Should extract something or nothing, but not crash
        let _ = result.symbols.len();
    }

    #[test]
    fn test_type_annotation_refs() {
        let result = extract(
            r#"
def process(user: User, count: int) -> Response:
    pass
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
        // int is lowercase → not captured
        assert!(!targets.contains(&"int"));
    }

    #[test]
    fn test_decorator_refs() {
        let result = extract(
            r#"
@login_required
def protected():
    pass

@app.route("/api")
def endpoint():
    pass
"#,
        );

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();

        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"login_required"));
        assert!(targets.contains(&"app.route"));
    }

    #[test]
    fn test_except_clause_refs() {
        let result = extract(
            r#"
def risky():
    try:
        pass
    except ValueError:
        pass
    except (TypeError, KeyError):
        pass
"#,
        );

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();

        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"ValueError"));
        assert!(targets.contains(&"TypeError"));
        assert!(targets.contains(&"KeyError"));
    }

    #[test]
    fn test_imports_specific_names() {
        let result = extract(
            r#"
from typing import Optional, List
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
        assert!(targets.contains(&"Optional"));
        assert!(targets.contains(&"List"));
    }
}
