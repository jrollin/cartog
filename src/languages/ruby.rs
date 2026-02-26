use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use crate::types::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{node_text, ExtractionResult, Extractor};

/// Extracts symbols and edges from Ruby source files.
pub struct RubyExtractor {
    parser: Parser,
}

impl RubyExtractor {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_ruby::LANGUAGE))
            .expect("Ruby grammar should always load");
        Self { parser }
    }
}

impl Default for RubyExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for RubyExtractor {
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
        "method" => {
            extract_method(node, source, file_path, parent_id, symbols, edges);
        }
        "singleton_method" => {
            extract_singleton_method(node, source, file_path, parent_id, symbols, edges);
        }
        "class" => {
            extract_class(node, source, file_path, parent_id, symbols, edges);
        }
        "module" => {
            extract_module(node, source, file_path, parent_id, symbols, edges);
        }
        "call" => {
            // Top-level calls: require/require_relative/include/extend/raise
            extract_top_level_call(node, source, file_path, parent_id, symbols, edges);
        }
        "assignment" => {
            extract_assignment(node, source, file_path, parent_id, symbols);
        }
        _ => {
            for child in node.named_children(&mut node.walk()) {
                extract_node(child, source, file_path, parent_id, symbols, edges);
            }
        }
    }
}

// ── Methods ──

fn extract_method(
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

    let visibility = ruby_visibility(&name);
    let signature = extract_method_signature(node, source);
    let docstring = extract_doc_comment(node, source);

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
    .with_signature(signature)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    // Walk the method body for calls, raises, rescue refs
    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls_and_raises(body, source, file_path, &sym_id, edges);
        // Recurse for nested definitions
        for child in body.named_children(&mut body.walk()) {
            match child.kind() {
                "method" | "singleton_method" | "class" | "module" => {
                    extract_node(child, source, file_path, Some(&sym_id), symbols, edges);
                }
                _ => {}
            }
        }
    }
}

fn extract_singleton_method(
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

    // self.method_name → extract as method with "self." prefix removed
    // It's always a class-level method
    let kind = if parent_id.is_some() {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };

    let signature = extract_method_signature(node, source);
    let docstring = extract_doc_comment(node, source);

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
    .with_signature(signature)
    .with_docstring(docstring);
    // Singleton methods (def self.foo) are public by default
    let visibility = ruby_visibility(&name);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls_and_raises(body, source, file_path, &sym_id, edges);
        for child in body.named_children(&mut body.walk()) {
            match child.kind() {
                "method" | "singleton_method" | "class" | "module" => {
                    extract_node(child, source, file_path, Some(&sym_id), symbols, edges);
                }
                _ => {}
            }
        }
    }
}

// ── Classes ──

fn extract_class(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => extract_constant_name(n, source),
        None => return,
    };
    if name.is_empty() {
        return;
    }

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    let sym = Symbol::new(
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
    symbols.push(sym);

    // Superclass → inherits edge
    if let Some(superclass) = node.child_by_field_name("superclass") {
        let base_name = extract_constant_name(superclass, source);
        if !base_name.is_empty() {
            edges.push(Edge::new(
                &sym_id,
                base_name,
                EdgeKind::Inherits,
                file_path,
                superclass.start_position().row as u32 + 1,
            ));
        }
    }

    // Walk class body
    if let Some(body) = node.child_by_field_name("body") {
        for child in body.named_children(&mut body.walk()) {
            extract_node(child, source, file_path, Some(&sym_id), symbols, edges);
        }
    }
}

// ── Modules ──

fn extract_module(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => extract_constant_name(n, source),
        None => return,
    };
    if name.is_empty() {
        return;
    }

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, &name, start_line);
    let sym = Symbol::new(
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
    symbols.push(sym);

    // Walk module body
    if let Some(body) = node.child_by_field_name("body") {
        for child in body.named_children(&mut body.walk()) {
            extract_node(child, source, file_path, Some(&sym_id), symbols, edges);
        }
    }
}

// ── Top-level calls (require, include, extend, raise) ──

fn extract_top_level_call(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let method_name = match node.child_by_field_name("method") {
        Some(n) => node_text(n, source),
        None => return,
    };

    let line = node.start_position().row as u32 + 1;

    match method_name {
        "require" | "require_relative" => {
            extract_require(
                node,
                source,
                file_path,
                parent_id,
                method_name,
                symbols,
                edges,
            );
        }
        "include" | "extend" | "prepend" => {
            // include/extend/prepend ModuleName → inherits edge
            if let Some(ctx) = parent_id {
                if let Some(args) = node.child_by_field_name("arguments") {
                    for arg in args.named_children(&mut args.walk()) {
                        let name = extract_constant_name(arg, source);
                        if !name.is_empty() {
                            edges.push(Edge::new(ctx, name, EdgeKind::Inherits, file_path, line));
                        }
                    }
                }
            }
        }
        "raise" | "fail" => {
            if let Some(ctx) = parent_id {
                extract_raise_from_call(node, source, file_path, ctx, edges);
            }
        }
        "attr_reader" | "attr_writer" | "attr_accessor" => {
            // Skip — these define dynamic methods, not symbols we track
        }
        _ => {
            // Regular call — ignore at top level, handled by walk_for_calls_and_raises
        }
    }
}

fn extract_require(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    method_name: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;

    // Get the argument (the required file path)
    let arg_text = if let Some(args) = node.child_by_field_name("arguments") {
        if let Some(first_arg) = args.named_child(0) {
            let text = node_text(first_arg, source);
            strip_ruby_string(text)
        } else {
            return;
        }
    } else {
        return;
    };

    if arg_text.is_empty() {
        return;
    }

    let import_text = format!(
        "{method_name} {}",
        node_text(node, source)
            .split_once(' ')
            .map(|(_, r)| r)
            .unwrap_or(&arg_text)
    );
    let sym_id = symbol_id(file_path, &arg_text, line);

    symbols.push(
        Symbol::new(
            &arg_text,
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

    // Use the last segment of the path as the imported name
    let imported_name = arg_text.rsplit('/').next().unwrap_or(&arg_text);
    edges.push(Edge::new(
        sym_id,
        imported_name,
        EdgeKind::Imports,
        file_path,
        line,
    ));
}

// ── Assignments ──

fn extract_assignment(
    node: Node,
    source: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    if let Some(left) = node.child_by_field_name("left") {
        let name = match left.kind() {
            "identifier" => node_text(left, source).to_string(),
            "constant" => node_text(left, source).to_string(),
            _ => return,
        };
        if name.is_empty() {
            return;
        }

        let line = node.start_position().row as u32 + 1;
        let visibility = ruby_visibility(&name);

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

// ── Call and raise walking ──

fn walk_for_calls_and_raises(
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
                "call" => {
                    let method_name = current
                        .child_by_field_name("method")
                        .map(|n| node_text(n, source))
                        .unwrap_or("");

                    match method_name {
                        "raise" | "fail" => {
                            extract_raise_from_call(current, source, file_path, context_id, edges);
                        }
                        _ if !method_name.is_empty() => {
                            // Build callee name: receiver.method or just method
                            let callee = if let Some(recv) = current.child_by_field_name("receiver")
                            {
                                let recv_text = node_text(recv, source);
                                format!("{recv_text}.{method_name}")
                            } else {
                                method_name.to_string()
                            };
                            edges.push(Edge::new(
                                context_id,
                                callee,
                                EdgeKind::Calls,
                                file_path,
                                current.start_position().row as u32 + 1,
                            ));
                        }
                        _ => {}
                    }
                }
                "rescue" => {
                    // Extract exception type references from rescue clauses
                    if let Some(exceptions) = current.child_by_field_name("exceptions") {
                        for child in exceptions.named_children(&mut exceptions.walk()) {
                            let type_name = extract_constant_name(child, source);
                            if !type_name.is_empty() {
                                edges.push(Edge::new(
                                    context_id,
                                    type_name,
                                    EdgeKind::References,
                                    file_path,
                                    child.start_position().row as u32 + 1,
                                ));
                            }
                        }
                    }
                }
                // Don't descend into nested method/class/module definitions
                "method" | "singleton_method" | "class" | "module" => {
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

fn extract_raise_from_call(
    node: Node,
    source: &str,
    file_path: &str,
    context_id: &str,
    edges: &mut Vec<Edge>,
) {
    // raise ExceptionClass, "message" or raise ExceptionClass.new("msg")
    if let Some(args) = node.child_by_field_name("arguments") {
        if let Some(first_arg) = args.named_child(0) {
            let exc_name = match first_arg.kind() {
                "constant" | "scope_resolution" => extract_constant_name(first_arg, source),
                "call" => {
                    // raise MyError.new("msg")
                    first_arg
                        .child_by_field_name("receiver")
                        .map(|r| extract_constant_name(r, source))
                        .unwrap_or_default()
                }
                _ => String::new(),
            };
            if !exc_name.is_empty() {
                edges.push(Edge::new(
                    context_id,
                    exc_name,
                    EdgeKind::Raises,
                    file_path,
                    node.start_position().row as u32 + 1,
                ));
            }
        }
    }
}

// ── Helpers ──

/// Ruby visibility heuristic based on naming convention.
/// Ruby's actual visibility uses `private`/`protected`/`public` keyword sections,
/// which are not tracked here (would require scope-aware analysis).
/// - Names starting with `_` → Private (convention)
/// - Everything else → Public
fn ruby_visibility(name: &str) -> Visibility {
    if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

/// Extract a constant name from a node, handling `constant`, `scope_resolution`, etc.
fn extract_constant_name(node: Node, source: &str) -> String {
    match node.kind() {
        "constant" => node_text(node, source).to_string(),
        "scope_resolution" => {
            // Foo::Bar — use the full name
            node_text(node, source).to_string()
        }
        "superclass" => {
            // The superclass node wraps the actual type
            node.named_child(0)
                .map(|c| extract_constant_name(c, source))
                .unwrap_or_default()
        }
        _ => node_text(node, source).to_string(),
    }
}

/// Extract method signature: `(params)`
fn extract_method_signature(node: Node, source: &str) -> Option<String> {
    let params = node.child_by_field_name("parameters")?;
    Some(node_text(params, source).to_string())
}

/// Extract preceding `#` comments as docstring.
fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut prev = node.prev_sibling();

    while let Some(p) = prev {
        if p.kind() == "comment" {
            let text = node_text(p, source);
            if let Some(stripped) = text.strip_prefix('#') {
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

/// Strip quotes from a Ruby string literal.
fn strip_ruby_string(s: &str) -> String {
    s.trim_matches('"').trim_matches('\'').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> ExtractionResult {
        let mut ext = RubyExtractor::new();
        ext.extract(source, "test.rb").unwrap()
    }

    #[test]
    fn test_simple_function() {
        let result = extract(
            r#"
# Greet someone by name.
def hello(name)
  "Hello, #{name}!"
end
"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "hello");
        assert_eq!(result.symbols[0].kind, SymbolKind::Function);
        assert_eq!(result.symbols[0].signature.as_deref(), Some("(name)"));
        assert_eq!(
            result.symbols[0].docstring.as_deref(),
            Some("Greet someone by name.")
        );
    }

    #[test]
    fn test_class_with_methods() {
        let result = extract(
            r#"
class UserService
  def initialize(db)
    @db = db
  end

  def get_user(user_id)
    @db.find(user_id)
  end

  def _internal_method
  end
end
"#,
        );

        let class = result.symbols.iter().find(|s| s.name == "UserService");
        assert!(class.is_some());
        assert_eq!(class.unwrap().kind, SymbolKind::Class);

        let init = result.symbols.iter().find(|s| s.name == "initialize");
        assert!(init.is_some());
        assert_eq!(init.unwrap().kind, SymbolKind::Method);
        assert!(init.unwrap().parent_id.is_some());

        let internal = result.symbols.iter().find(|s| s.name == "_internal_method");
        assert!(internal.is_some());
        assert_eq!(internal.unwrap().visibility, Visibility::Private);
    }

    #[test]
    fn test_inheritance() {
        let result = extract(
            r#"
class AdminService < UserService
end
"#,
        );

        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].target_name, "UserService");
    }

    #[test]
    fn test_module() {
        let result = extract(
            r#"
module Authentication
  def authenticate(token)
    validate(token)
  end
end
"#,
        );

        let module = result.symbols.iter().find(|s| s.name == "Authentication");
        assert!(module.is_some());
        assert_eq!(module.unwrap().kind, SymbolKind::Class);

        let method = result.symbols.iter().find(|s| s.name == "authenticate");
        assert!(method.is_some());
        assert_eq!(method.unwrap().kind, SymbolKind::Method);
        assert!(method.unwrap().parent_id.is_some());
    }

    #[test]
    fn test_function_calls() {
        let result = extract(
            r#"
def process
  data = fetch_data
  result = transform(data)
  save(result)
end
"#,
        );

        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert!(calls.len() >= 2);

        let targets: Vec<&str> = calls.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"transform"));
        assert!(targets.contains(&"save"));
    }

    #[test]
    fn test_method_calls_with_receiver() {
        let result = extract(
            r#"
def handle(req)
  req.validate
  logger.info("handled")
end
"#,
        );

        let calls: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert!(calls.len() >= 2);

        let targets: Vec<&str> = calls.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"req.validate"));
        assert!(targets.contains(&"logger.info"));
    }

    #[test]
    fn test_require() {
        let result = extract(
            r#"
require 'json'
require_relative 'auth/tokens'
"#,
        );

        let imports: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);

        let import_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();
        assert_eq!(import_edges.len(), 2);

        let targets: Vec<&str> = import_edges
            .iter()
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(targets.contains(&"json"));
        assert!(targets.contains(&"tokens"));
    }

    #[test]
    fn test_raises() {
        let result = extract(
            r#"
def validate(x)
  raise ArgumentError, "negative" if x < 0
  raise RuntimeError.new("too large") if x > 100
end
"#,
        );

        let raises: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Raises)
            .collect();
        assert_eq!(raises.len(), 2);

        let targets: Vec<&str> = raises.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"ArgumentError"));
        assert!(targets.contains(&"RuntimeError"));
    }

    #[test]
    fn test_rescue_refs() {
        let result = extract(
            r#"
def risky
  do_something
rescue ArgumentError
  handle_arg_error
rescue TypeError, KeyError
  handle_other
end
"#,
        );

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();

        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"ArgumentError"));
        assert!(targets.contains(&"TypeError"));
        assert!(targets.contains(&"KeyError"));
    }

    #[test]
    fn test_include_mixin() {
        let result = extract(
            r#"
class AuthService
  include Authenticatable
  extend ClassMethods
end
"#,
        );

        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();

        let targets: Vec<&str> = inherits.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"Authenticatable"));
        assert!(targets.contains(&"ClassMethods"));
    }

    #[test]
    fn test_singleton_method() {
        let result = extract(
            r#"
class Config
  def self.load(path)
    new(path)
  end
end
"#,
        );

        let method = result.symbols.iter().find(|s| s.name == "load");
        assert!(method.is_some());
        let m = method.unwrap();
        assert_eq!(m.kind, SymbolKind::Method);
        assert!(m.parent_id.is_some());
    }

    #[test]
    fn test_visibility() {
        let result = extract(
            r#"
class Foo
  def public_method; end
  def _private_method; end
end
"#,
        );

        let public = result.symbols.iter().find(|s| s.name == "public_method");
        assert_eq!(public.unwrap().visibility, Visibility::Public);

        let private = result.symbols.iter().find(|s| s.name == "_private_method");
        assert_eq!(private.unwrap().visibility, Visibility::Private);
    }

    #[test]
    fn test_namespaced_class() {
        let result = extract(
            r#"
class Foo::Bar < Baz::Quux
end
"#,
        );

        let class = result.symbols.iter().find(|s| s.name == "Foo::Bar");
        assert!(class.is_some());
        assert_eq!(class.unwrap().kind, SymbolKind::Class);

        let inherits: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].target_name, "Baz::Quux");
    }

    #[test]
    fn test_variable_assignment() {
        let result = extract(
            r#"
MAX_RETRIES = 3
_internal = true

class Config
  DEFAULT_PORT = 8080
end
"#,
        );

        let vars: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Variable)
            .collect();
        assert_eq!(vars.len(), 3);

        let names: Vec<&str> = vars.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MAX_RETRIES"));
        assert!(names.contains(&"_internal"));
        assert!(names.contains(&"DEFAULT_PORT"));

        let internal = vars.iter().find(|s| s.name == "_internal").unwrap();
        assert_eq!(internal.visibility, Visibility::Private);

        let port = vars.iter().find(|s| s.name == "DEFAULT_PORT").unwrap();
        assert!(port.parent_id.is_some());
    }

    #[test]
    fn test_attr_accessor_skipped() {
        let result = extract(
            r#"
class User
  attr_reader :name, :email
  attr_accessor :role

  def initialize(name)
    @name = name
  end
end
"#,
        );

        // attr_reader/attr_accessor should not create symbols
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"attr_reader"));
        assert!(!names.contains(&"attr_accessor"));
        assert!(names.contains(&"User"));
        assert!(names.contains(&"initialize"));
    }

    #[test]
    fn test_top_level_singleton_method() {
        // def self.foo outside a class → Function, not Method
        let result = extract(
            r#"
def self.configure
  # setup
end
"#,
        );

        let sym = result.symbols.iter().find(|s| s.name == "configure");
        assert!(sym.is_some());
        assert_eq!(sym.unwrap().kind, SymbolKind::Function);
        assert!(sym.unwrap().parent_id.is_none());
    }

    #[test]
    fn test_empty_file() {
        let result = extract("");
        assert!(result.symbols.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_syntax_error_partial_parse() {
        let result = extract("def broken(\n  end");
        let _ = result.symbols.len();
    }
}
