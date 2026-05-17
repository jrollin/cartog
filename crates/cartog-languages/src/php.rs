use std::collections::HashMap;

use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use cartog_core::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{node_text, ExtractionResult, Extractor};

/// Tree-sitter-based extractor for PHP source files.
///
/// Extracts classes, interfaces, traits, methods, functions, namespace declarations,
/// use statements, and edges (inheritance, implementation, trait use, calls, references).
pub struct PhpExtractor {
    parser: Parser,
}

impl PhpExtractor {
    /// Creates a new `PhpExtractor` with an initialised tree-sitter PHP parser.
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_php::LANGUAGE_PHP_ONLY))
            .expect("PHP grammar should always load");
        Self { parser }
    }
}

impl Default for PhpExtractor {
    /// Returns a default `PhpExtractor` via [`PhpExtractor::new`].
    fn default() -> Self {
        Self::new()
    }
}

// Per-file namespace and import context used for name resolution.
struct FileContext {
    namespace: Option<String>,
    // short name → FQCN from `use` statements
    imports: HashMap<String, String>,
}

impl FileContext {
    /// Resolves a name (bare, qualified, or fully-qualified) to its FQCN using the
    /// current namespace and import map.
    ///
    /// Three input shapes are handled:
    /// - `\App\Foo` (leading `\`) is already fully qualified; the leading slash is stripped.
    /// - `Alias\Sub` (no leading `\`, contains `\`) resolves `Alias` through the import map,
    ///   producing `<imported>\Sub`; otherwise the whole name is treated as relative to the
    ///   current namespace.
    /// - `Bar` (single segment) resolves through imports first, then the current namespace.
    fn resolve(&self, name: &str) -> String {
        if let Some(rest) = name.strip_prefix('\\') {
            return rest.to_string();
        }
        if let Some((head, tail)) = name.split_once('\\') {
            if let Some(base) = self.imports.get(head) {
                return format!("{base}\\{tail}");
            }
            if let Some(ns) = &self.namespace {
                return format!("{ns}\\{name}");
            }
            return name.to_string();
        }
        if let Some(fqcn) = self.imports.get(name) {
            return fqcn.clone();
        }
        if let Some(ns) = &self.namespace {
            return format!("{ns}\\{name}");
        }
        name.to_string()
    }
}

/// Lexical scope of the symbol enclosing the declaration being extracted.
///
/// The `id` is the cartog symbol id of the enclosing class/interface/trait/enum (used
/// as the `parent_id` on child symbols), and `qname` is the qualified name used both
/// as the `parent_name` segment of [`symbol_id`] and as the prefix when building child
/// qualified names. At the top level of a file, `id` is `None` and `qname` carries the
/// PHP namespace (if any) so cross-namespace symbol ids don't collide.
#[derive(Clone, Copy, Default)]
struct ParentScope<'a> {
    id: Option<&'a str>,
    qname: Option<&'a str>,
}

impl<'a> ParentScope<'a> {
    /// Scope at the top level of a (possibly empty) PHP namespace.
    fn top_level(namespace: Option<&'a str>) -> Self {
        Self {
            id: None,
            qname: namespace,
        }
    }

    /// Scope nested inside a class/interface/trait/enum identified by `id` + `qname`.
    fn nested(id: &'a str, qname: &'a str) -> Self {
        Self {
            id: Some(id),
            qname: Some(qname),
        }
    }
}

/// Builds a [`FileContext`] by scanning the AST root for namespace and use declarations.
fn collect_file_context(root: Node, source: &str) -> FileContext {
    let mut namespace = None;
    let mut imports = HashMap::new();
    collect_context_in(root, source, &mut namespace, &mut imports);
    FileContext { namespace, imports }
}

/// Collects namespace and import information from `node`'s direct children into the
/// provided accumulators.
///
/// Only statement-style `namespace Foo;` (without a body) contributes to the outer
/// scope; brace-style `namespace Foo { ... }` owns its own scope and is handled
/// separately by [`extract_top_level`], so its contents are intentionally not merged
/// here.
fn collect_context_in(
    node: Node,
    source: &str,
    namespace: &mut Option<String>,
    imports: &mut HashMap<String, String>,
) {
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "namespace_definition" => {
                if child.child_by_field_name("body").is_some() {
                    continue;
                }
                if let Some(name_node) = child.child_by_field_name("name") {
                    *namespace = Some(node_text(name_node, source).to_string());
                }
            }
            "namespace_use_declaration" => {
                collect_imports(child, source, imports);
            }
            _ => {}
        }
    }
}

/// Resolves a single `namespace_use_clause` to its FQCN and alias, then inserts it into `imports`.
fn collect_use_clause(
    clause: Node,
    source: &str,
    prefix: &str,
    imports: &mut HashMap<String, String>,
) {
    let Some(name_node) = clause
        .named_children(&mut clause.walk())
        .find(|c| matches!(c.kind(), "qualified_name" | "name"))
    else {
        return;
    };
    let relative = node_text(name_node, source);
    let fqcn = if prefix.is_empty() {
        relative.to_string()
    } else {
        format!("{prefix}\\{relative}")
    };
    let alias = clause
        .child_by_field_name("alias")
        .map(|n| node_text(n, source).to_string())
        .unwrap_or_else(|| fqcn.rsplit('\\').next().unwrap_or(&fqcn).to_string());
    imports.insert(alias, fqcn);
}

/// Populates `imports` from a `namespace_use_declaration` node, handling both grouped and simple forms.
fn collect_imports(node: Node, source: &str, imports: &mut HashMap<String, String>) {
    // Grouped form: `use Prefix\{A, B};`
    if let Some(group) = node.child_by_field_name("body") {
        let prefix = node
            .named_children(&mut node.walk())
            .find(|c| c.kind() == "namespace_name")
            .map(|n| node_text(n, source).to_string())
            .unwrap_or_default();
        for clause in group.named_children(&mut group.walk()) {
            if clause.kind() == "namespace_use_clause" {
                collect_use_clause(clause, source, &prefix, imports);
            }
        }
        return;
    }

    // Simple / comma-separated form: `use A\B;` or `use A\B, C\D;`
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "namespace_use_clause" => collect_use_clause(child, source, "", imports),
            "qualified_name" | "name" => {
                let fqcn = node_text(child, source).to_string();
                let short = fqcn.rsplit('\\').next().unwrap_or(&fqcn).to_string();
                imports.insert(short, fqcn);
            }
            _ => {}
        }
    }
}

/// Emits an `Import` symbol and `Imports`/`References` edges for a single use clause.
fn extract_use_clause_edge(
    clause: Node,
    source: &str,
    file_path: &str,
    prefix: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(name_node) = clause
        .named_children(&mut clause.walk())
        .find(|c| matches!(c.kind(), "qualified_name" | "name"))
    else {
        return;
    };
    let relative = node_text(name_node, source);
    let fqcn = if prefix.is_empty() {
        relative.to_string()
    } else {
        format!("{prefix}\\{relative}")
    };
    if fqcn.is_empty() {
        return;
    }
    let line = clause.start_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, "import", &fqcn, None);
    symbols.push(Symbol::new(
        fqcn.clone(),
        SymbolKind::Import,
        file_path,
        line,
        line,
        clause.start_byte() as u32,
        clause.end_byte() as u32,
        None,
    ));
    edges.push(Edge::new(
        &sym_id,
        fqcn.clone(),
        EdgeKind::Imports,
        file_path,
        line,
    ));
    let short_name = fqcn.rsplit('\\').next().unwrap_or(&fqcn).to_string();
    edges.push(Edge::new(
        &sym_id,
        short_name,
        EdgeKind::References,
        file_path,
        line,
    ));
}

/// Processes a `namespace_use_declaration` node and emits symbols and edges for each imported name.
fn extract_namespace_use(
    node: Node,
    source: &str,
    file_path: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // Grouped form: `use Prefix\{A, B};`
    if let Some(group) = node.child_by_field_name("body") {
        let prefix = node
            .named_children(&mut node.walk())
            .find(|c| c.kind() == "namespace_name")
            .map(|n| node_text(n, source).to_string())
            .unwrap_or_default();
        for clause in group.named_children(&mut group.walk()) {
            if clause.kind() == "namespace_use_clause" {
                extract_use_clause_edge(clause, source, file_path, &prefix, symbols, edges);
            }
        }
        return;
    }

    // Simple / comma-separated form
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "namespace_use_clause" {
            extract_use_clause_edge(child, source, file_path, "", symbols, edges);
        }
    }
}

/// Collects all `name`/`qualified_name` text values under `node`, used for base classes, interfaces, and traits.
fn collect_type_names<'a>(node: Node<'a>, source: &'a str) -> Vec<(&'a str, u32)> {
    let mut out = Vec::new();
    collect_type_names_in(node, source, &mut out);
    out
}

/// Recursively accumulates `name`/`qualified_name` nodes into `out`.
fn collect_type_names_in<'a>(node: Node<'a>, source: &'a str, out: &mut Vec<(&'a str, u32)>) {
    match node.kind() {
        "name" | "qualified_name" => {
            let text = node_text(node, source);
            if !text.is_empty() {
                out.push((text, node.start_position().row as u32 + 1));
            }
        }
        _ => {
            for child in node.named_children(&mut node.walk()) {
                collect_type_names_in(child, source, out);
            }
        }
    }
}

impl Extractor for PhpExtractor {
    /// Parses `source` as PHP and extracts symbols and edges into an [`ExtractionResult`].
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        let tree = self
            .parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {file_path}"))?;

        let ctx = collect_file_context(tree.root_node(), source);
        let mut symbols = Vec::new();
        let mut edges = Vec::new();

        extract_top_level(
            tree.root_node(),
            source,
            file_path,
            &ctx,
            &mut symbols,
            &mut edges,
        );

        Ok(ExtractionResult { symbols, edges })
    }
}

/// Iterates top-level AST nodes and dispatches to the appropriate extractor for each declaration kind.
fn extract_top_level(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let parent = ParentScope::top_level(ctx.namespace.as_deref());
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "namespace_use_declaration" => {
                extract_namespace_use(child, source, file_path, symbols, edges);
            }
            "class_declaration" => {
                extract_class(child, source, file_path, ctx, parent, symbols, edges);
            }
            "interface_declaration" => {
                extract_interface(child, source, file_path, ctx, parent, symbols, edges);
            }
            "trait_declaration" => {
                extract_trait(child, source, file_path, ctx, parent, symbols, edges);
            }
            "enum_declaration" => {
                extract_enum(child, source, file_path, ctx, parent, symbols, edges);
            }
            "function_definition" => {
                extract_function(child, source, file_path, ctx, parent, symbols, edges);
            }
            "namespace_definition" => {
                // Brace-style namespace body: each block owns its own
                // namespace + import scope, so build a fresh context for the
                // body and recurse with it instead of inheriting `ctx`.
                if let Some(body) = child.child_by_field_name("body") {
                    let namespace = child
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source).to_string());
                    let mut imports = HashMap::new();
                    collect_context_in(body, source, &mut None, &mut imports);
                    let scope_ctx = FileContext { namespace, imports };
                    extract_top_level(body, source, file_path, &scope_ctx, symbols, edges);
                }
            }
            _ => {}
        }
    }
}

// ── Class declarations ──

/// Extracts a class declaration node into a `Class` symbol along with inheritance, implementation, and body edges.
fn extract_class(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = php_visibility(node, source);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, "class", &name, parent.qname);
    let class_qname = match parent.qname {
        Some(pq) => format!("{pq}.{name}"),
        None => name.clone(),
    };

    let mut sym = Symbol::new(
        name,
        SymbolKind::Class,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "base_clause" => {
                for (target, line) in collect_type_names(child, source) {
                    edges.push(Edge::new(
                        &sym_id,
                        ctx.resolve(target),
                        EdgeKind::Inherits,
                        file_path,
                        line,
                    ));
                }
            }
            "class_interface_clause" => {
                for (target, line) in collect_type_names(child, source) {
                    edges.push(Edge::new(
                        &sym_id,
                        ctx.resolve(target),
                        EdgeKind::Implements,
                        file_path,
                        line,
                    ));
                }
            }
            _ => {}
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_class_body(
            body,
            source,
            file_path,
            ctx,
            ParentScope::nested(&sym_id, &class_qname),
            symbols,
            edges,
        );
    }
}

// ── Interface declarations ──

/// Extracts an interface declaration node into an `Interface` symbol along with extension edges.
fn extract_interface(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, "interface", &name, parent.qname);
    let iface_qname = match parent.qname {
        Some(pq) => format!("{pq}.{name}"),
        None => name.clone(),
    };

    let sym = Symbol::new(
        name,
        SymbolKind::Interface,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_docstring(docstring);
    symbols.push(sym);

    // interface extends
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "base_clause" {
            for (target, line) in collect_type_names(child, source) {
                edges.push(Edge::new(
                    &sym_id,
                    ctx.resolve(target),
                    EdgeKind::Inherits,
                    file_path,
                    line,
                ));
            }
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_class_body(
            body,
            source,
            file_path,
            ctx,
            ParentScope::nested(&sym_id, &iface_qname),
            symbols,
            edges,
        );
    }
}

// ── Trait declarations ──

/// Extracts a trait declaration node into a `Trait` symbol and processes its body.
fn extract_trait(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, "trait", &name, parent.qname);
    let trait_qname = match parent.qname {
        Some(pq) => format!("{pq}.{name}"),
        None => name.clone(),
    };

    let sym = Symbol::new(
        name,
        SymbolKind::Trait,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_docstring(docstring);
    symbols.push(sym);

    if let Some(body) = node.child_by_field_name("body") {
        extract_class_body(
            body,
            source,
            file_path,
            ctx,
            ParentScope::nested(&sym_id, &trait_qname),
            symbols,
            edges,
        );
    }
}

// ── Enum declarations ──

/// Extracts an enum declaration node into an `Enum` symbol along with implementation edges and methods.
fn extract_enum(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, "enum", &name, parent.qname);
    let enum_qname = match parent.qname {
        Some(pq) => format!("{pq}.{name}"),
        None => name.clone(),
    };

    let sym = Symbol::new(
        name,
        SymbolKind::Enum,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_docstring(docstring);
    symbols.push(sym);

    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "class_interface_clause" {
            for (target, line) in collect_type_names(child, source) {
                edges.push(Edge::new(
                    &sym_id,
                    ctx.resolve(target),
                    EdgeKind::Implements,
                    file_path,
                    line,
                ));
            }
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_class_body(
            body,
            source,
            file_path,
            ctx,
            ParentScope::nested(&sym_id, &enum_qname),
            symbols,
            edges,
        );
    }
}

// ── Class body ──

/// Iterates a class/interface/trait body and extracts methods and trait-use edges.
fn extract_class_body(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let parent_id = parent
        .id
        .expect("extract_class_body called without a parent symbol id");
    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "method_declaration" => {
                extract_method(child, source, file_path, ctx, parent, symbols, edges);
            }
            "use_declaration" => {
                // trait use inside class body: `use TimestampTrait;`
                for (target, line) in collect_type_names(child, source) {
                    edges.push(Edge::new(
                        parent_id,
                        ctx.resolve(target),
                        EdgeKind::References,
                        file_path,
                        line,
                    ));
                }
            }
            _ => {}
        }
    }
}

// ── Methods ──

/// Extracts a method declaration into a `Method` symbol, emitting constructor-injection and call edges.
fn extract_method(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let parent_id = parent
        .id
        .expect("extract_method called without a parent symbol id");
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let visibility = php_visibility(node, source);
    let signature = build_method_signature(node, source, &name, visibility);
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, "method", &name, parent.qname);
    let mut sym = Symbol::new(
        name.clone(),
        SymbolKind::Method,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(Some(parent_id))
    .with_signature(signature)
    .with_docstring(docstring);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    if name == "__construct" {
        if let Some(params) = node.child_by_field_name("parameters") {
            extract_constructor_injection_edges(params, source, file_path, ctx, parent_id, edges);
        }
    }

    let mut scope = node
        .child_by_field_name("parameters")
        .map(|p| build_param_scope(p, source, ctx))
        .unwrap_or_default();

    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, ctx, &sym_id, &mut scope, edges);
    }
}

/// Collects named (non-primitive) types from a type hint node, including unions and optional types.
fn collect_injected_named_types<'a>(
    node: Node<'a>,
    source: &'a str,
    out: &mut Vec<(&'a str, u32)>,
) {
    match node.kind() {
        "named_type" => {
            if let Some(child) = node.named_children(&mut node.walk()).next() {
                if matches!(child.kind(), "name" | "qualified_name") {
                    let text = node_text(child, source);
                    if !matches!(text, "self" | "static" | "parent") {
                        out.push((text, child.start_position().row as u32 + 1));
                    }
                }
            }
        }
        "optional_type" | "union_type" | "intersection_type" => {
            for child in node.named_children(&mut node.walk()) {
                collect_injected_named_types(child, source, out);
            }
        }
        _ => {}
    }
}

/// Emits `References` edges from the class to each injected dependency type found in constructor parameters.
fn extract_constructor_injection_edges(
    params: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    class_id: &str,
    edges: &mut Vec<Edge>,
) {
    for param in params.named_children(&mut params.walk()) {
        if !matches!(
            param.kind(),
            "simple_parameter" | "property_promotion_parameter"
        ) {
            continue;
        }
        let Some(type_node) = param.child_by_field_name("type") else {
            continue;
        };
        let mut types = Vec::new();
        collect_injected_named_types(type_node, source, &mut types);
        for (name, line) in types {
            edges.push(Edge::new(
                class_id,
                ctx.resolve(name),
                EdgeKind::References,
                file_path,
                line,
            ));
        }
    }
}

/// Builds a variable-to-FQCN scope map from a parameter list using type hints.
///
/// Only simple named types are inferred (not unions or optionals). PHP scalar types
/// (`string`, `int`, …) and pseudo-types (`self`, `static`, `parent`) are skipped.
fn build_param_scope(params: Node, source: &str, ctx: &FileContext) -> HashMap<String, String> {
    let mut scope = HashMap::new();
    for param in params.named_children(&mut params.walk()) {
        if !matches!(
            param.kind(),
            "simple_parameter" | "property_promotion_parameter"
        ) {
            continue;
        }
        let Some(type_node) = param.child_by_field_name("type") else {
            continue;
        };
        if type_node.kind() != "named_type" {
            continue;
        }
        let Some(name_node) = type_node.named_children(&mut type_node.walk()).next() else {
            continue;
        };
        let type_name = node_text(name_node, source);
        if matches!(
            type_name,
            "self"
                | "static"
                | "parent"
                | "string"
                | "int"
                | "float"
                | "bool"
                | "array"
                | "callable"
                | "iterable"
                | "void"
                | "null"
                | "never"
                | "mixed"
                | "object"
                | "false"
                | "true"
        ) {
            continue;
        }
        if let Some(var_node) = param.child_by_field_name("name") {
            let var_name = node_text(var_node, source).to_string();
            scope.insert(var_name, ctx.resolve(type_name));
        }
    }
    scope
}

// ── Top-level functions ──

/// Extracts a top-level function definition into a `Function` symbol and emits call edges from its body.
fn extract_function(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let signature = node
        .child_by_field_name("parameters")
        .map(|p| format!("function {}{}", name, node_text(p, source)));
    let docstring = extract_doc_comment(node, source);

    let sym_id = symbol_id(file_path, "function", &name, parent.qname);
    let sym = Symbol::new(
        name,
        SymbolKind::Function,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_signature(signature)
    .with_docstring(docstring);
    symbols.push(sym);

    let mut scope = node
        .child_by_field_name("parameters")
        .map(|p| build_param_scope(p, source, ctx))
        .unwrap_or_default();

    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, ctx, &sym_id, &mut scope, edges);
    }
}

// ── Call graph traversal ──

/// Walks `node`'s subtree and emits `Calls`/`References` edges for every call and object-creation expression found.
///
/// `scope` is pre-seeded with typed parameters and updated inline as `$x = new Foo()` assignments
/// are encountered, enabling qualified call targets like `"Foo.method"` instead of `"method"`.
fn walk_for_calls(
    node: Node,
    source: &str,
    file_path: &str,
    ctx: &FileContext,
    context_id: &str,
    scope: &mut HashMap<String, String>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();

    loop {
        let current = cursor.node();

        match current.kind() {
            "assignment_expression" => {
                // Infer $x = new Foo() → scope["$x"] = FQCN(Foo)
                // Collect named children (skips the literal "=" token) into a Vec
                // to avoid borrow conflicts with the outer cursor.
                let named: Vec<_> = {
                    let mut w = current.walk();
                    current.named_children(&mut w).collect()
                };
                if named.len() == 2 && named[1].kind() == "object_creation_expression" {
                    let mut oce_walk = named[1].walk();
                    let class_node = named[1]
                        .named_children(&mut oce_walk)
                        .find(|n| matches!(n.kind(), "name" | "qualified_name"));
                    if let Some(class_node) = class_node {
                        let var_name = node_text(named[0], source);
                        let class_name = node_text(class_node, source);
                        if var_name.starts_with('$') && !class_name.is_empty() {
                            scope.insert(var_name.to_string(), ctx.resolve(class_name));
                        }
                    }
                }
            }
            "function_call_expression" => {
                if let Some(fn_node) = current.child_by_field_name("function") {
                    let callee = node_text(fn_node, source);
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
            "member_call_expression" => {
                if let (Some(obj_node), Some(name_node)) = (
                    current.child_by_field_name("object"),
                    current.child_by_field_name("name"),
                ) {
                    let method_name = node_text(name_node, source);
                    if !method_name.is_empty() {
                        let obj_text = node_text(obj_node, source);
                        let target = scope
                            .get(obj_text)
                            .map(|fqcn| format!("{fqcn}.{method_name}"))
                            .unwrap_or_else(|| method_name.to_string());
                        edges.push(Edge::new(
                            context_id,
                            target,
                            EdgeKind::Calls,
                            file_path,
                            current.start_position().row as u32 + 1,
                        ));
                    }
                }
            }
            "scoped_call_expression" => {
                if let (Some(scope_node), Some(name_node)) = (
                    current.child_by_field_name("scope"),
                    current.child_by_field_name("name"),
                ) {
                    let method_name = node_text(name_node, source);
                    if !method_name.is_empty() {
                        let target = match scope_node.kind() {
                            "name" | "qualified_name" => {
                                let scope_text = node_text(scope_node, source);
                                // self/static/parent depend on the surrounding class and
                                // can't be resolved here without a class stack; leave them
                                // unqualified so heuristic resolution can still match.
                                if matches!(scope_text, "self" | "static" | "parent") {
                                    method_name.to_string()
                                } else {
                                    let fqcn = ctx.resolve(scope_text);
                                    format!("{fqcn}.{method_name}")
                                }
                            }
                            "variable_name" => {
                                let var = node_text(scope_node, source);
                                scope
                                    .get(var)
                                    .map(|fqcn| format!("{fqcn}.{method_name}"))
                                    .unwrap_or_else(|| method_name.to_string())
                            }
                            _ => method_name.to_string(),
                        };
                        edges.push(Edge::new(
                            context_id,
                            target,
                            EdgeKind::Calls,
                            file_path,
                            current.start_position().row as u32 + 1,
                        ));
                    }
                }
            }
            "object_creation_expression" => {
                let mut oce_walk = current.walk();
                let class_node = current
                    .named_children(&mut oce_walk)
                    .find(|n| matches!(n.kind(), "name" | "qualified_name"));
                if let Some(class_node) = class_node {
                    let class_name = node_text(class_node, source);
                    if !class_name.is_empty() {
                        edges.push(Edge::new(
                            context_id,
                            ctx.resolve(class_name),
                            EdgeKind::References,
                            file_path,
                            current.start_position().row as u32 + 1,
                        ));
                    }
                }
            }
            _ => {}
        }

        if cursor.goto_first_child() {
            continue;
        }
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

// ── Helpers ──

/// Returns the [`Visibility`] declared on `node` by inspecting its `visibility_modifier` child, defaulting to `Public`.
fn php_visibility(node: Node, source: &str) -> Visibility {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "visibility_modifier" {
            return Visibility::from_str_lossy(node_text(child, source));
        }
    }
    Visibility::Public
}

/// Builds a human-readable method signature string including visibility, name, parameters, and return type.
fn build_method_signature(node: Node, source: &str, name: &str, vis: Visibility) -> Option<String> {
    let params = node
        .child_by_field_name("parameters")
        .map(|p| node_text(p, source))?;
    let return_type = node
        .child_by_field_name("return_type")
        .map(|r| format!(": {}", node_text(r, source)))
        .unwrap_or_default();
    Some(format!(
        "{} function {}{}{}",
        vis.as_str(),
        name,
        params,
        return_type
    ))
}

/// Extracts the PHPDoc or line-comment block immediately preceding `node`, returning its text content.
fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    let prev = node.prev_sibling()?;
    if prev.kind() != "comment" {
        return None;
    }
    let text = node_text(prev, source);
    if text.starts_with("/**") {
        let inner = text.trim_start_matches("/**").trim_end_matches("*/").trim();
        let lines: Vec<&str> = inner
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .filter(|l| !l.is_empty())
            .collect();
        if !lines.is_empty() {
            return Some(lines.join(" "));
        }
    } else if text.starts_with("//") {
        let mut lines = Vec::new();
        let mut p: Option<Node> = Some(prev);
        while let Some(n) = p {
            if n.kind() == "comment" {
                let t = node_text(n, source);
                if let Some(stripped) = t.strip_prefix("//") {
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
        PhpExtractor::new()
            .extract(source, "src/Foo.php")
            .expect("parse failed")
    }

    #[test]
    fn test_class_extracted() {
        let result = extract(
            r#"<?php
namespace App\Domain;
class MaintenancePlan {}
"#,
        );
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "MaintenancePlan" && s.kind == SymbolKind::Class));
    }

    #[test]
    fn test_interface_extracted() {
        let result = extract(
            r#"<?php
interface Repository {}
"#,
        );
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Repository" && s.kind == SymbolKind::Interface));
    }

    #[test]
    fn test_trait_extracted() {
        let result = extract(
            r#"<?php
trait TimestampTrait {
    public function getCreatedAt(): \DateTimeImmutable {}
}
"#,
        );
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "TimestampTrait" && s.kind == SymbolKind::Trait));
        assert!(result.symbols.iter().any(|s| s.name == "getCreatedAt"));
    }

    #[test]
    fn test_inheritance_edge() {
        let result = extract(
            r#"<?php
use Vendor\BaseClass;
class Child extends BaseClass {}
"#,
        );
        assert!(result
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Inherits && e.target_name == "Vendor\\BaseClass"));
    }

    #[test]
    fn test_implements_edge() {
        let result = extract(
            r#"<?php
use App\Contracts\Repository;
class InMemoryRepository implements Repository {}
"#,
        );
        assert!(result.edges.iter().any(
            |e| e.kind == EdgeKind::Implements && e.target_name == "App\\Contracts\\Repository"
        ));
    }

    #[test]
    fn test_method_call_edge() {
        let result = extract(
            r#"<?php
class Service {
    public function handle(): void {
        $this->repository->save($entity);
    }
}
"#,
        );
        assert!(result
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.target_name == "save"));
    }

    #[test]
    fn test_namespace_resolution() {
        let result = extract(
            r#"<?php
namespace App\Domain;
use App\Contracts\RepositoryInterface;
class TaskRepository implements RepositoryInterface {}
"#,
        );
        assert!(result.edges.iter().any(|e| {
            e.kind == EdgeKind::Implements && e.target_name == "App\\Contracts\\RepositoryInterface"
        }));
    }

    #[test]
    fn test_import_simple_creates_symbol_and_edge() {
        let result = extract(
            r#"<?php
use MobilityWork\SparePartStocksManagement\SparePartStockDualWriteDispatcher;
"#,
        );
        assert!(result.symbols.iter().any(|s| {
            s.kind == SymbolKind::Import
                && s.name
                    == "MobilityWork\\SparePartStocksManagement\\SparePartStockDualWriteDispatcher"
        }));
        assert!(result.edges.iter().any(|e| {
            e.kind == EdgeKind::Imports
                && e.target_name
                    == "MobilityWork\\SparePartStocksManagement\\SparePartStockDualWriteDispatcher"
        }));
    }

    #[test]
    fn test_import_multi_clause() {
        let result = extract(
            r#"<?php
use MobilityWork\Core\MessageBus\CorrelationContext,
    MobilityWork\Core\Identifiers\LegacyNetworkId;
"#,
        );
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
        assert!(targets.contains(&"MobilityWork\\Core\\MessageBus\\CorrelationContext"));
        assert!(targets.contains(&"MobilityWork\\Core\\Identifiers\\LegacyNetworkId"));
    }

    #[test]
    fn test_import_grouped_creates_edges_with_full_fqcn() {
        let result = extract(
            r#"<?php
use MobilityWork\Core\{
    MessageBus\CorrelationContext,
    Identifiers\LegacyNetworkId
};
"#,
        );
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
        assert!(targets.contains(&"MobilityWork\\Core\\MessageBus\\CorrelationContext"));
        assert!(targets.contains(&"MobilityWork\\Core\\Identifiers\\LegacyNetworkId"));
    }

    #[test]
    fn test_import_grouped_with_alias_uses_fqcn_as_target() {
        let result = extract(
            r#"<?php
use MobilityWork\Core\{
    Identifiers\LegacyNetworkId as LegacyId
};
class Foo {
    public function handle(LegacyId $id): void {}
}
"#,
        );
        assert!(result.edges.iter().any(|e| {
            e.kind == EdgeKind::Imports
                && e.target_name == "MobilityWork\\Core\\Identifiers\\LegacyNetworkId"
        }));
    }

    #[test]
    fn test_grouped_import_resolves_in_inheritance() {
        let result = extract(
            r#"<?php
use MobilityWork\Core\{
    AbstractRepository,
    Identifiers\NetworkId
};
class MyRepository extends AbstractRepository {}
"#,
        );
        assert!(result.edges.iter().any(|e| {
            e.kind == EdgeKind::Inherits
                && e.target_name == "MobilityWork\\Core\\AbstractRepository"
        }));
    }

    #[test]
    fn test_import_creates_references_edge_to_short_name() {
        let result = extract(
            r#"<?php
use MobilityWork\SparePartStocksManagement\SparePartStockDualWriteDispatcher;
"#,
        );
        assert!(result.edges.iter().any(|e| {
            e.kind == EdgeKind::References && e.target_name == "SparePartStockDualWriteDispatcher"
        }));
    }

    #[test]
    fn test_constructor_injection_simple() {
        let result = extract(
            r#"<?php
use App\Repository\SparePartStockRepository;
use App\Service\EventDispatcher;

class SparePartStockService {
    public function __construct(
        private SparePartStockRepository $repository,
        private EventDispatcher $dispatcher,
    ) {}
}
"#,
        );
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        let targets: Vec<&str> = refs.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"App\\Repository\\SparePartStockRepository"));
        assert!(targets.contains(&"App\\Service\\EventDispatcher"));
    }

    #[test]
    fn test_constructor_injection_optional_type() {
        let result = extract(
            r#"<?php
use App\Logger\AuditLogger;

class MyService {
    public function __construct(
        private ?AuditLogger $logger,
    ) {}
}
"#,
        );
        assert!(result.edges.iter().any(|e| {
            e.kind == EdgeKind::References && e.target_name == "App\\Logger\\AuditLogger"
        }));
    }

    #[test]
    fn test_constructor_injection_union_type() {
        let result = extract(
            r#"<?php
use App\Contracts\HandlerInterface;
use App\Contracts\FallbackHandler;

class Router {
    public function __construct(
        private HandlerInterface|FallbackHandler $handler,
    ) {}
}
"#,
        );
        let targets: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(targets.contains(&"App\\Contracts\\HandlerInterface"));
        assert!(targets.contains(&"App\\Contracts\\FallbackHandler"));
    }

    #[test]
    fn test_constructor_injection_skips_primitives() {
        let result = extract(
            r#"<?php
use App\Service\MyService;

class Foo {
    public function __construct(
        private MyService $service,
        private string $name,
        private int $count,
    ) {}
}
"#,
        );
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert!(refs
            .iter()
            .any(|e| e.target_name == "App\\Service\\MyService" || e.target_name == "MyService"));
        assert!(!refs
            .iter()
            .any(|e| e.target_name == "string" || e.target_name == "int"));
    }

    #[test]
    fn test_constructor_injection_source_is_class_not_method() {
        let result = extract(
            r#"<?php
use App\Repository\Repo;

class MyHandler {
    public function __construct(
        private Repo $repo,
    ) {}
}
"#,
        );
        let ref_edge = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::References && e.target_name == "App\\Repository\\Repo")
            .expect("injection edge not found");
        // source should be the class symbol, not the __construct method symbol
        assert!(
            ref_edge.source_id.contains("class"),
            "source: {}",
            ref_edge.source_id
        );
        assert!(
            !ref_edge.source_id.contains("method"),
            "source: {}",
            ref_edge.source_id
        );
    }

    #[test]
    fn test_get_extractor_php() {
        use crate::get_extractor;
        assert!(get_extractor("php").is_some());
    }

    // ── Enum declarations ──

    #[test]
    fn test_pure_enum_extracted() {
        let result = extract(
            r#"<?php
enum Suit { case Hearts; case Spades; }
"#,
        );
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Suit" && s.kind == SymbolKind::Enum));
    }

    #[test]
    fn test_backed_enum_extracted() {
        let result = extract(
            r#"<?php
enum Status: string { case Active = 'active'; case Inactive = 'inactive'; }
"#,
        );
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Status" && s.kind == SymbolKind::Enum));
    }

    #[test]
    fn test_enum_not_classified_as_class() {
        let result = extract(
            r#"<?php
enum PurchaseOrderStatus: string { case Draft = 'draft'; case Confirmed = 'confirmed'; }
"#,
        );
        assert!(!result
            .symbols
            .iter()
            .any(|s| s.name == "PurchaseOrderStatus" && s.kind == SymbolKind::Class));
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "PurchaseOrderStatus" && s.kind == SymbolKind::Enum));
    }

    #[test]
    fn test_enum_implements_edge() {
        let result = extract(
            r#"<?php
use App\Contracts\HasLabel;
enum Color: string implements HasLabel { case Red = 'red'; }
"#,
        );
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Implements
                    && e.target_name == "App\\Contracts\\HasLabel")
        );
    }

    #[test]
    fn test_enum_with_method_extracted() {
        let result = extract(
            r#"<?php
enum Status: string {
    case Active = 'active';

    public function label(): string {
        return match($this) {
            Status::Active => 'Active',
        };
    }
}
"#,
        );
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Status" && s.kind == SymbolKind::Enum));
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "label" && s.kind == SymbolKind::Method));
    }

    #[test]
    fn test_enum_in_namespace_extracted() {
        let result = extract(
            r#"<?php
namespace App\Domain\PurchaseOrder;
enum PurchaseOrderStatus: string { case Draft = 'draft'; }
"#,
        );
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "PurchaseOrderStatus" && s.kind == SymbolKind::Enum));
    }

    // ── Variable type inference ──

    #[test]
    fn test_infers_type_from_new_expression() {
        let result = extract(
            r#"<?php
use App\Repository\UserRepository;

class Service {
    public function handle(): void {
        $repo = new UserRepository();
        $repo->save($entity);
    }
}
"#,
        );
        assert!(
            result.edges.iter().any(|e| {
                e.kind == EdgeKind::Calls && e.target_name == "App\\Repository\\UserRepository.save"
            }),
            "expected qualified Calls edge; got: {:?}",
            result
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Calls)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_infers_type_from_typed_parameter() {
        let result = extract(
            r#"<?php
use App\Repository\UserRepository;

function handle(UserRepository $repo): void {
    $repo->save($entity);
}
"#,
        );
        assert!(result.edges.iter().any(|e| {
            e.kind == EdgeKind::Calls && e.target_name == "App\\Repository\\UserRepository.save"
        }));
    }

    #[test]
    fn test_unresolved_variable_falls_back_to_short_name() {
        let result = extract(
            r#"<?php
class Service {
    public function handle(): void {
        $unknown->doSomething();
    }
}
"#,
        );
        // No type known → unqualified edge preserved
        assert!(result
            .edges
            .iter()
            .any(|e| { e.kind == EdgeKind::Calls && e.target_name == "doSomething" }));
        assert!(!result.edges.iter().any(|e| e.target_name.contains('.')));
    }

    #[test]
    fn test_multiple_vars_inferred_independently() {
        let result = extract(
            r#"<?php
use App\Repo\PostRepo;
use App\Repo\TagRepo;

class Publisher {
    public function run(): void {
        $posts = new PostRepo();
        $tags  = new TagRepo();
        $posts->findAll();
        $tags->findAll();
    }
}
"#,
        );
        let calls: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(calls.contains(&"App\\Repo\\PostRepo.findAll"));
        assert!(calls.contains(&"App\\Repo\\TagRepo.findAll"));
    }

    // ── FQCN resolution ──

    #[test]
    fn test_resolve_strips_leading_backslash() {
        let result = extract(
            r#"<?php
namespace App;
class Child extends \Vendor\BaseClass {}
"#,
        );
        assert!(result
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Inherits && e.target_name == "Vendor\\BaseClass"));
    }

    #[test]
    fn test_resolve_qualified_alias_head() {
        // `use Pkg\Inner as Pkg;` makes `Pkg\Service` resolve to `Pkg\Inner\Service`,
        // not stay as `Pkg\Service`.
        let result = extract(
            r#"<?php
use Pkg\Inner as Pkg;
class Foo extends Pkg\Service {}
"#,
        );
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Inherits && e.target_name == "Pkg\\Inner\\Service"),
            "expected inherits target Pkg\\Inner\\Service; got: {:?}",
            result
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Inherits)
                .map(|e| &e.target_name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_resolve_qualified_relative_to_namespace() {
        // No alias for `Sub`: `Sub\Service` is relative to current namespace.
        let result = extract(
            r#"<?php
namespace App\Domain;
class Foo extends Sub\Service {}
"#,
        );
        assert!(result.edges.iter().any(|e| {
            e.kind == EdgeKind::Inherits && e.target_name == "App\\Domain\\Sub\\Service"
        }));
    }

    // ── Multi-namespace files ──

    #[test]
    fn test_brace_namespaces_have_independent_imports() {
        let result = extract(
            r#"<?php
namespace App\Foo {
    use Other\B;
    class A extends B {}
}

namespace App\Bar {
    class C extends B {}
}
"#,
        );

        // In App\Foo, `B` resolves through the import to Other\B.
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Inherits && e.target_name == "Other\\B"),
            "expected App\\Foo's B to resolve via import to Other\\B"
        );

        // In App\Bar, `B` has no import → falls back to current namespace.
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Inherits && e.target_name == "App\\Bar\\B"),
            "expected App\\Bar's B to resolve to App\\Bar\\B, not leak Other\\B"
        );
    }

    #[test]
    fn test_brace_namespaces_classify_symbols_by_block() {
        let result = extract(
            r#"<?php
namespace App\Foo { class A {} }
namespace App\Bar { class A {} }
"#,
        );
        let class_a_ids: Vec<&str> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class && s.name == "A")
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(
            class_a_ids.len(),
            2,
            "expected two A classes, got {class_a_ids:?}"
        );
        assert!(class_a_ids.iter().any(|id| id.contains("App\\Foo.A")));
        assert!(class_a_ids.iter().any(|id| id.contains("App\\Bar.A")));
    }

    // ── Namespace-qualified symbol IDs ──

    #[test]
    fn test_top_level_class_symbol_id_includes_namespace() {
        let result = extract(
            r#"<?php
namespace App\Domain;
class Service {}
"#,
        );
        let svc = result
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Class && s.name == "Service")
            .expect("Service class not found");
        assert!(
            svc.id.ends_with(":class:App\\Domain.Service"),
            "expected namespace-qualified symbol id; got: {}",
            svc.id
        );
    }

    #[test]
    fn test_top_level_function_symbol_id_includes_namespace() {
        let result = extract(
            r#"<?php
namespace App\Util;
function helper(): void {}
"#,
        );
        let f = result
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Function && s.name == "helper")
            .expect("helper function not found");
        assert!(
            f.id.ends_with(":function:App\\Util.helper"),
            "expected namespace-qualified function id; got: {}",
            f.id
        );
    }

    // ── Static method calls ──

    #[test]
    fn test_static_call_qualifies_with_class_name() {
        let result = extract(
            r#"<?php
class Service {
    public function handle(): void {
        Foo::bar();
    }
}
"#,
        );
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Calls && e.target_name == "Foo.bar"),
            "expected Calls edge target Foo.bar; got: {:?}",
            result
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Calls)
                .map(|e| &e.target_name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_static_call_resolves_imported_class() {
        let result = extract(
            r#"<?php
use Lib\Foo;
class Service {
    public function handle(): void {
        Foo::bar();
    }
}
"#,
        );
        assert!(result
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.target_name == "Lib\\Foo.bar"));
    }

    #[test]
    fn test_static_call_self_stays_unqualified() {
        // `self::method()` depends on the surrounding class; without a class
        // stack we leave it unqualified so heuristic resolution can match it.
        let result = extract(
            r#"<?php
class Service {
    public function handle(): void {
        self::work();
    }
    public function work(): void {}
}
"#,
        );
        assert!(result
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.target_name == "work"));
    }
}
