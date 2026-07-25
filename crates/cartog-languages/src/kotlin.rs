//! Kotlin extractor: symbols and edges from Kotlin source via tree-sitter.
//!
//! The fwcd grammar folds class / interface / `enum class` / `data class` /
//! `sealed class` into a single `class_declaration` node, disambiguated by the
//! leading keyword token (`class` / `interface` / `enum`). Objects are separate
//! `object_declaration` / `companion_object` nodes. Members are direct named
//! children of `class_body` / `enum_class_body`. KDoc (`/** */`) and line comments
//! are `extras`, so they float as siblings rather than attaching to declarations.

use anyhow::Result;
use cartog_core::{Edge, EdgeKind, Symbol, SymbolKind, Visibility};
use tree_sitter::{Language, Node, Parser};

use crate::{
    node_text, qualified, tree_depth_exceeds, ExtractionResult, ParentScope, MAX_TREE_DEPTH,
};

/// Extracts symbols and edges from Kotlin source files.
pub struct KotlinExtractor {
    parser: Parser,
}

impl KotlinExtractor {
    /// Create a new Kotlin extractor with the tree-sitter Kotlin grammar loaded.
    #[must_use]
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_kotlin_sg::LANGUAGE))
            .expect("Kotlin grammar should always load");
        Self { parser }
    }
}

impl Default for KotlinExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::Extractor for KotlinExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        let tree = crate::parse_bounded(&mut self.parser, source)
            .ok_or_else(|| anyhow::anyhow!("failed to parse {file_path}"))?;

        if tree_depth_exceeds(tree.root_node(), MAX_TREE_DEPTH) {
            tracing::warn!(
                "skipping {file_path}: AST nesting exceeds {MAX_TREE_DEPTH}, too deep to extract safely"
            );
            return Ok(ExtractionResult::default());
        }

        let mut symbols = Vec::new();
        let mut edges = Vec::new();
        extract_node(
            tree.root_node(),
            source,
            file_path,
            ParentScope::default(),
            &mut symbols,
            &mut edges,
        );
        Ok(ExtractionResult { symbols, edges })
    }
}

/// Dispatch a node to its extractor; the default arm recurses into named children.
fn extract_node(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    match node.kind() {
        "class_declaration" => {
            extract_type_like(node, source, file_path, parent, symbols, edges);
        }
        "object_declaration" | "companion_object" => {
            extract_object(node, source, file_path, parent, symbols, edges);
        }
        "function_declaration" => {
            extract_function(node, source, file_path, parent, symbols, edges);
        }
        "secondary_constructor" => {
            extract_constructor(node, source, file_path, parent, symbols, edges);
        }
        "property_declaration" => {
            extract_property(node, source, file_path, parent, symbols, edges);
        }
        "enum_entry" => extract_enum_entry(node, source, file_path, parent, symbols),
        "type_alias" => extract_typealias(node, source, file_path, parent, symbols, edges),
        "import_header" => extract_import(node, source, file_path, parent, symbols, edges),
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_node(child, source, file_path, parent, symbols, edges);
            }
        }
    }
}

/// class / interface / `enum class` / `data class` / `sealed class` — one
/// `class_declaration` node keyed by the leading keyword token.
fn extract_type_like(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(name) = type_identifier_name(node, source) else {
        return;
    };

    let kind = class_kind(node);
    let is_interface = kind == SymbolKind::Interface;
    let is_enum = kind == SymbolKind::Enum;

    let qname = qualified(parent.qname, &name);
    let id = cartog_core::symbol_id(file_path, kind, &name, parent.qname);
    let sym = Symbol::new(
        &name,
        kind,
        file_path,
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_visibility(visibility(node, source))
    .with_docstring(doc_comment(node, source))
    .with_signature(Some(header_signature(node, source)));
    symbols.push(sym);

    let is_class = !is_interface && !is_enum;
    inheritance_edges(node, source, file_path, &id, is_class, edges);

    let scope = ParentScope::nested(&id, &qname);
    // Primary-constructor `val`/`var` parameters are member properties.
    if let Some(pc) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "primary_constructor")
    {
        extract_primary_constructor(pc, source, file_path, scope, symbols, edges);
    }
    extract_body_members(node, source, file_path, scope, symbols, edges);
}

/// object / companion object — maps to Class; its body members attach to it.
fn extract_object(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // A `companion object` may be anonymous; default its name to `Companion`.
    let name = type_identifier_name(node, source).unwrap_or_else(|| "Companion".to_string());
    let qname = qualified(parent.qname, &name);
    let id = cartog_core::symbol_id(file_path, SymbolKind::Class, &name, parent.qname);
    let sym = Symbol::new(
        &name,
        SymbolKind::Class,
        file_path,
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_visibility(visibility(node, source))
    .with_docstring(doc_comment(node, source))
    .with_signature(Some(header_signature(node, source)));
    symbols.push(sym);

    // An object is a singleton class: a superclass call is real inheritance.
    inheritance_edges(node, source, file_path, &id, true, edges);
    let scope = ParentScope::nested(&id, &qname);
    extract_body_members(node, source, file_path, scope, symbols, edges);
}

/// Recurse the `class_body` / `enum_class_body` members of a type-like node.
fn extract_body_members(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if matches!(child.kind(), "class_body" | "enum_class_body") {
            let mut bc = child.walk();
            for member in child.named_children(&mut bc) {
                match member.kind() {
                    // `init {}` and multiline `get()`/`set()` own no symbol; their calls are the type's.
                    "anonymous_initializer" | "getter" | "setter" => {
                        if let Some(owner) = scope.id {
                            walk_calls(member, source, file_path, owner, edges);
                        }
                    }
                    _ => extract_node(member, source, file_path, scope, symbols, edges),
                }
            }
        }
    }
}

/// fun (top-level → Function, in a type → Method).
fn extract_function(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(name) = func_name(node, source) else {
        return;
    };
    let kind = if parent.id.is_some() {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let id = cartog_core::symbol_id(file_path, kind, &name, parent.qname);
    let sym = Symbol::new(
        &name,
        kind,
        file_path,
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_visibility(visibility(node, source))
    .with_async(is_suspend(node))
    .with_docstring(doc_comment(node, source))
    .with_signature(Some(header_signature(node, source)));
    symbols.push(sym);

    signature_type_refs(node, source, file_path, &id, edges);
    // `function_body` holds either a `statements` block or an `= expr` body. The
    // grammar exposes it as a child by kind (no `body` field).
    if let Some(body) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "function_body")
    {
        walk_calls(body, source, file_path, &id, edges);
        let qname = qualified(parent.qname, &name);
        let scope = ParentScope::nested(&id, &qname);
        walk_nested_decls(body, source, file_path, scope, symbols, edges);
    }
}

/// `constructor(...) { ... }` — a Method of the enclosing type named `constructor`
/// (its Kotlin identity). Multiple secondary constructors share that name, so they
/// collide on `file:method:Owner.constructor` — the same overload limitation as
/// methods elsewhere in the codebase, not a Kotlin-specific quirk.
fn extract_constructor(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let id = cartog_core::symbol_id(file_path, SymbolKind::Method, "constructor", parent.qname);
    let sym = Symbol::new(
        "constructor",
        SymbolKind::Method,
        file_path,
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
        node.start_byte() as u32,
        node.end_byte() as u32,
        parent.qname,
    )
    .with_parent(parent.id)
    .with_visibility(visibility(node, source))
    .with_docstring(doc_comment(node, source))
    .with_signature(Some(header_signature(node, source)));
    symbols.push(sym);

    signature_type_refs(node, source, file_path, &id, edges);
    // Body calls live in `statements`; `: this(...)`/`: super(...)` delegation is a
    // sibling `constructor_delegation_call`. Walk both.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "statements" | "constructor_delegation_call") {
            walk_calls(child, source, file_path, &id, edges);
        }
    }
}

/// `class Foo(val a: A, b: B)` — each `val`/`var` parameter is a member property;
/// every parameter's type contributes a References edge from the owning type.
fn extract_primary_constructor(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    for param in node.named_children(&mut cursor) {
        if param.kind() != "class_parameter" {
            continue;
        }
        // Only `val`/`var` parameters declare a property.
        let is_property = param
            .named_children(&mut param.walk())
            .any(|c| c.kind() == "binding_pattern_kind");
        if let Some(name_node) = param
            .named_children(&mut param.walk())
            .find(|c| c.kind() == "simple_identifier")
        {
            if is_property {
                let name = node_text(name_node, source);
                symbols.push(
                    Symbol::new(
                        name,
                        SymbolKind::Variable,
                        file_path,
                        param.start_position().row as u32 + 1,
                        param.end_position().row as u32 + 1,
                        param.start_byte() as u32,
                        param.end_byte() as u32,
                        parent.qname,
                    )
                    .with_parent(parent.id)
                    .with_visibility(visibility(param, source)),
                );
            }
        }
        // Parameter types reference the owning type regardless of val/var, and a
        // default value (`= buildClient()`) contributes the owner's calls.
        if let Some(owner) = parent.id {
            collect_type_refs(param, source, file_path, owner, edges);
            walk_calls(param, source, file_path, owner, edges);
        }
    }
}

/// val / var bindings (top-level or member). One Variable per bound identifier.
fn extract_property(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "variable_declaration" => {
                if let Some(id) = child
                    .named_children(&mut child.walk())
                    .find(|c| c.kind() == "simple_identifier")
                {
                    names.push(node_text(id, source).to_string());
                }
            }
            "multi_variable_declaration" => {
                let mut mc = child.walk();
                for vd in child.named_children(&mut mc) {
                    if let Some(id) = vd
                        .named_children(&mut vd.walk())
                        .find(|c| c.kind() == "simple_identifier")
                    {
                        names.push(node_text(id, source).to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if names.is_empty() {
        return;
    }
    let vis = visibility(node, source);
    let docstring = doc_comment(node, source);
    let first_id = cartog_core::symbol_id(file_path, SymbolKind::Variable, &names[0], parent.qname);
    for name in &names {
        symbols.push(
            Symbol::new(
                name,
                SymbolKind::Variable,
                file_path,
                node.start_position().row as u32 + 1,
                node.end_position().row as u32 + 1,
                node.start_byte() as u32,
                node.end_byte() as u32,
                parent.qname,
            )
            .with_parent(parent.id)
            .with_visibility(vis)
            .with_docstring(docstring.clone()),
        );
    }
    // Type annotations live inside the `variable_declaration`; the initializer,
    // accessor, and delegate calls live in the remaining children. Attribute all
    // to the first binding to avoid N-fold duplication for `val a, b: User`.
    let mut tc = node.walk();
    for child in node.named_children(&mut tc) {
        match child.kind() {
            "variable_declaration" | "multi_variable_declaration" => {
                collect_type_refs(child, source, file_path, &first_id, edges);
            }
            "modifiers" | "binding_pattern_kind" => {}
            // `by lazy { ... }` runs in the property's init context, so its lambda calls are the property's.
            "property_delegate" => {
                walk_calls_through_lambdas(child, source, file_path, &first_id, edges);
            }
            _ => walk_calls(child, source, file_path, &first_id, edges),
        }
    }
}

/// enum entries — one EnumMember per `enum_entry`.
fn extract_enum_entry(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
) {
    let Some(name_node) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "simple_identifier")
    else {
        return;
    };
    let name = node_text(name_node, source);
    symbols.push(
        Symbol::new(
            name,
            SymbolKind::EnumMember,
            file_path,
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent.qname,
        )
        .with_parent(parent.id),
    );
}

/// typealias → TypeAlias; the RHS contributes References.
fn extract_typealias(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(name_node) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "type_identifier")
    else {
        return;
    };
    let name = node_text(name_node, source).to_string();
    let id = cartog_core::symbol_id(file_path, SymbolKind::TypeAlias, &name, parent.qname);
    symbols.push(
        Symbol::new(
            &name,
            SymbolKind::TypeAlias,
            file_path,
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent.qname,
        )
        .with_parent(parent.id)
        .with_visibility(visibility(node, source))
        .with_signature(Some(header_signature(node, source))),
    );

    // Emit refs for every type identifier except the alias's own name node.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.id() == name_node.id() {
            continue;
        }
        collect_type_refs(child, source, file_path, &id, edges);
    }
}

/// import a.b.C / import a.b.* / import a.b.C as D → Import symbol + Imports edge.
/// Target is the last path segment (the imported name), matching the other
/// extractors' `last_segment` convention.
fn extract_import(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(ident) = node
        .named_children(&mut node.walk())
        .find(|n| n.kind() == "identifier")
    else {
        return;
    };
    let full = node_text(ident, source).trim().to_string();
    if full.is_empty() {
        return;
    }
    let is_wildcard = node
        .named_children(&mut node.walk())
        .any(|n| n.kind() == "wildcard_import");
    // `import a.b.C as D` binds the local name `D`; key the edge on the alias so
    // usages of `D` resolve. Wildcard has no single name → the package tail.
    let alias = node
        .named_children(&mut node.walk())
        .find(|n| n.kind() == "import_alias")
        .and_then(|a| {
            a.named_children(&mut a.walk())
                .find(|c| c.kind() == "type_identifier")
        })
        .map(|c| node_text(c, source).to_string());
    let target = match (&alias, is_wildcard) {
        (Some(a), _) => a.clone(),
        (None, true) => full.clone(),
        (None, false) => crate::last_segment(&full, ".").to_string(),
    };
    let id = cartog_core::symbol_id(file_path, SymbolKind::Import, &full, parent.qname);
    symbols.push(
        Symbol::new(
            &full,
            SymbolKind::Import,
            file_path,
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent.qname,
        )
        .with_parent(parent.id),
    );
    edges.push(Edge::new(
        id,
        target,
        EdgeKind::Imports,
        file_path,
        node.start_position().row as u32 + 1,
    ));
}

/// Emit Inherits/Implements from `delegation_specifier` children. A specifier whose
/// child is a `constructor_invocation` (`Base(...)`) is a superclass call → Inherits
/// for a class; a bare `user_type` (no call) is an interface → Implements.
fn inheritance_edges(
    node: Node,
    source: &str,
    file_path: &str,
    source_id: &str,
    is_class: bool,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    // Only the first superclass-call specifier inherits; any further constructor
    // calls (only reachable on malformed input) degrade to Implements.
    let mut first_inherit = true;
    for spec in node.named_children(&mut cursor) {
        if spec.kind() != "delegation_specifier" {
            continue;
        }
        let Some(inner) = spec.named_child(0) else {
            continue;
        };
        let is_super_call = inner.kind() == "constructor_invocation";
        let Some(target) = type_name(inner, source) else {
            continue;
        };
        if is_builtin_type(&target) {
            continue;
        }
        let inherits = is_class && is_super_call && first_inherit;
        if inherits {
            first_inherit = false;
        }
        let kind = if inherits {
            EdgeKind::Inherits
        } else {
            EdgeKind::Implements
        };
        edges.push(Edge::new(
            source_id.to_string(),
            target,
            kind,
            file_path,
            spec.start_position().row as u32 + 1,
        ));
    }
}

/// Collect References from a declaration's signature subtrees (params, return type,
/// property type annotation), skipping bodies, generic-parameter declarations,
/// modifiers (annotations), and supertype lists handled elsewhere.
fn signature_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    source_id: &str,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_body"
            | "class_body"
            | "enum_class_body"
            | "type_parameters"
            | "modifiers"
            | "primary_constructor"
            | "delegation_specifier" => {}
            _ => collect_type_refs(child, source, file_path, source_id, edges),
        }
    }
}

/// Recurse a subtree emitting a References edge per non-builtin `type_identifier`,
/// skipping anything reachable through an `annotation` or `type_parameters` node
/// (a bare annotation `@Override` and a generic param `T` both parse as a
/// `type_identifier` and would otherwise leak as phantom References).
fn collect_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    source_id: &str,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    match node.kind() {
        "annotation" | "type_parameters" | "modifiers" => return,
        // A qualified `user_type` is ONE reference (its leaf), not one per dotted
        // segment — emit the leaf, then recurse only into its `type_arguments` so
        // generic args like `Map<String, User>` still surface `User`.
        "user_type" => {
            if let Some(name) = type_name(node, source) {
                if !is_builtin_type(&name) {
                    edges.push(Edge::new(
                        source_id.to_string(),
                        name,
                        EdgeKind::References,
                        file_path,
                        node.start_position().row as u32 + 1,
                    ));
                }
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "type_arguments" {
                    collect_type_refs(child, source, file_path, source_id, edges);
                }
            }
            return;
        }
        "type_identifier" => {
            let name = node_text(node, source);
            if !name.is_empty() && !is_builtin_type(name) {
                edges.push(Edge::new(
                    source_id.to_string(),
                    name,
                    EdgeKind::References,
                    file_path,
                    node.start_position().row as u32 + 1,
                ));
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_refs(child, source, file_path, source_id, edges);
    }
}

/// Walk a subtree for `call_expression` nodes, emitting Calls edges. Recurses,
/// skipping nested function/lambda bodies — their calls belong to the nested symbol.
fn walk_calls(node: Node, source: &str, file_path: &str, source_id: &str, edges: &mut Vec<Edge>) {
    crate::parse::guard_recursion!();
    match node.kind() {
        "function_declaration" | "lambda_literal" | "anonymous_function" => return,
        "call_expression" => {
            if let Some(name) = callee_name(node, source) {
                edges.push(Edge::new(
                    source_id.to_string(),
                    name,
                    EdgeKind::Calls,
                    file_path,
                    node.start_position().row as u32 + 1,
                ));
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(child, source, file_path, source_id, edges);
    }
}

/// Like [`walk_calls`] but descends into lambda bodies; for `property_delegate` only.
fn walk_calls_through_lambdas(
    node: Node,
    source: &str,
    file_path: &str,
    source_id: &str,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    if node.kind() == "call_expression" {
        if let Some(name) = callee_name(node, source) {
            edges.push(Edge::new(
                source_id.to_string(),
                name,
                EdgeKind::Calls,
                file_path,
                node.start_position().row as u32 + 1,
            ));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls_through_lambdas(child, source, file_path, source_id, edges);
    }
}

/// Walk a body for nested `function_declaration`s, emitting each as a Method scoped
/// to `parent`. Doesn't descend nested function bodies, lambdas, or local type
/// bodies — their members aren't local functions of the enclosing symbol.
fn walk_nested_decls(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                extract_function(child, source, file_path, parent, symbols, edges);
            }
            "lambda_literal" | "anonymous_function" | "class_declaration"
            | "object_declaration" => {}
            _ => walk_nested_decls(child, source, file_path, parent, symbols, edges),
        }
    }
}

/// Callee name of a `call_expression`: leading identifier for `f()`, trailing
/// navigation segment for `a.b.f()`.
fn callee_name(call: Node, source: &str) -> Option<String> {
    let head = call.named_child(0)?;
    match head.kind() {
        "simple_identifier" => Some(node_text(head, source).to_string()),
        "navigation_expression" => {
            // The trailing `navigation_suffix` holds the called member's identifier.
            let suffix = head
                .named_children(&mut head.walk())
                .find(|c| c.kind() == "navigation_suffix")?;
            let ident = suffix
                .named_children(&mut suffix.walk())
                .find(|c| c.kind() == "simple_identifier")?;
            Some(node_text(ident, source).to_string())
        }
        _ => None,
    }
}

/// Function/method name: the `simple_identifier` child of a `function_declaration`.
fn func_name(node: Node, source: &str) -> Option<String> {
    let found = node
        .named_children(&mut node.walk())
        .find(|n| n.kind() == "simple_identifier")
        .map(|n| node_text(n, source).trim().to_string())
        .filter(|s| !s.is_empty());
    found
}

/// Name of a type-like declaration: its `type_identifier` child.
fn type_identifier_name(node: Node, source: &str) -> Option<String> {
    node.named_children(&mut node.walk())
        .find(|c| c.kind() == "type_identifier")
        .map(|c| node_text(c, source).to_string())
        .filter(|s| !s.is_empty())
}

/// Leaf type name from a `type_identifier` / `user_type` / `constructor_invocation`.
///
/// A qualified type (`com.example.Base`) parses as a `user_type` whose dotted
/// segments are flat sibling `type_identifier`s; the LAST is the type itself and
/// the leading ones are the package path, so we take the last segment, not the
/// first (taking the first would resolve `com.example.Base` to `com`).
fn type_name(node: Node, source: &str) -> Option<String> {
    crate::parse::guard_recursion!(None);
    match node.kind() {
        "type_identifier" => Some(node_text(node, source).to_string()),
        "user_type" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|c| c.kind() == "type_identifier")
                .last()
                .map(|c| node_text(c, source).to_string())
        }
        _ => {
            // constructor_invocation / nullable_type / receiver_type: descend to
            // the first user_type/type_identifier and take its leaf.
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find_map(|c| type_name(c, source));
            found
        }
    }
}

/// SymbolKind of a `class_declaration` from its leading keyword token (one pass).
/// `interface` → Interface, `enum` → Enum, everything else (class/data/sealed) → Class.
fn class_kind(node: Node) -> SymbolKind {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.is_named() {
            continue;
        }
        match c.kind() {
            "interface" => return SymbolKind::Interface,
            "enum" => return SymbolKind::Enum,
            _ => {}
        }
    }
    SymbolKind::Class
}

/// Visibility from a `modifiers → visibility_modifier` child. Kotlin defaults to
/// public; `private` → Private, `protected` → Protected, `internal`/`public` → Public.
fn visibility(node: Node, source: &str) -> Visibility {
    let mut cursor = node.walk();
    let Some(modifiers) = node.children(&mut cursor).find(|c| c.kind() == "modifiers") else {
        return Visibility::Public;
    };
    let mut mc = modifiers.walk();
    for m in modifiers.children(&mut mc) {
        if m.kind() == "visibility_modifier" {
            return match node_text(m, source).trim() {
                "private" => Visibility::Private,
                "protected" => Visibility::Protected,
                _ => Visibility::Public,
            };
        }
    }
    Visibility::Public
}

/// True when the function carries the `suspend` modifier (Kotlin's async marker).
fn is_suspend(node: Node) -> bool {
    let mut cursor = node.walk();
    let Some(modifiers) = node.children(&mut cursor).find(|c| c.kind() == "modifiers") else {
        return false;
    };
    let mut mc = modifiers.walk();
    for m in modifiers.children(&mut mc) {
        if m.kind() != "function_modifier" {
            continue;
        }
        let mut fc = m.walk();
        if m.children(&mut fc).any(|t| t.kind() == "suspend") {
            return true;
        }
    }
    false
}

/// Declaration header (name + params + return type), body stripped. Cuts at the
/// body node or the opening `{` (a secondary ctor wraps `statements` in braces, so
/// stopping at `statements` alone would leak the brace).
fn header_signature(node: Node, source: &str) -> String {
    let body_kinds = [
        "function_body",
        "class_body",
        "enum_class_body",
        "statements",
    ];
    let mut cursor = node.walk();
    let end = node
        .children(&mut cursor)
        .find(|c| body_kinds.contains(&c.kind()) || c.kind() == "{")
        .map(|b| b.start_byte())
        .unwrap_or(node.end_byte());
    source
        .get(node.start_byte()..end)
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// KDoc from a preceding `/** */` `multiline_comment`, but only when it is on the
/// line immediately above the declaration (a blank line breaks the association).
/// KDoc/comments are `extras`, so the comment is a preceding sibling, not a child.
fn doc_comment(node: Node, source: &str) -> Option<String> {
    let prev = node.prev_sibling()?;
    if prev.kind() != "multiline_comment" {
        return None;
    }
    // Reject a comment separated from the declaration by a blank line.
    if prev.end_position().row + 1 < node.start_position().row {
        return None;
    }
    let text = node_text(prev, source);
    let inner = text
        .strip_prefix("/**")
        .and_then(|t| t.strip_suffix("*/"))?;
    let joined = inner
        .lines()
        .map(|l| l.trim().trim_start_matches('*').trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

/// Kotlin stdlib types excluded from References / inheritance edges.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "Int"
            | "Long"
            | "Short"
            | "Byte"
            | "UInt"
            | "ULong"
            | "UShort"
            | "UByte"
            | "Float"
            | "Double"
            | "Boolean"
            | "Char"
            | "String"
            | "CharSequence"
            | "Unit"
            | "Any"
            | "Nothing"
            | "Number"
            | "Array"
            | "IntArray"
            | "LongArray"
            | "DoubleArray"
            | "BooleanArray"
            | "ByteArray"
            | "List"
            | "MutableList"
            | "Set"
            | "MutableSet"
            | "Map"
            | "MutableMap"
            | "Collection"
            | "MutableCollection"
            | "Iterable"
            | "Iterator"
            | "Sequence"
            | "Pair"
            | "Triple"
            | "Comparable"
            | "Throwable"
            | "Exception"
            | "RuntimeException"
            | "Error"
            | "Function"
            | "Enum"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Extractor;

    fn extract(source: &str) -> ExtractionResult {
        KotlinExtractor::new().extract(source, "test.kt").unwrap()
    }

    fn sym<'a>(r: &'a ExtractionResult, name: &str) -> &'a Symbol {
        r.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name:?} not found"))
    }

    fn has_edge(r: &ExtractionResult, kind: EdgeKind, target: &str) -> bool {
        r.edges
            .iter()
            .any(|e| e.kind == kind && e.target_name == target)
    }

    fn edges_to(r: &ExtractionResult, kind: EdgeKind, target: &str) -> usize {
        r.edges
            .iter()
            .filter(|e| e.kind == kind && e.target_name == target)
            .count()
    }

    #[test]
    fn top_level_function() {
        let r = extract("fun greet(name: String): String { return name }");
        let f = sym(&r, "greet");
        assert_eq!(f.kind, SymbolKind::Function);
        assert_eq!(f.visibility, Visibility::Public);
        assert!(f.signature.as_deref().unwrap().contains("greet"));
    }

    #[test]
    fn suspend_function_is_async() {
        let r = extract("suspend fun fetch(): Int { return 0 }");
        assert!(sym(&r, "fetch").is_async);
    }

    #[test]
    fn plain_function_is_not_async() {
        let r = extract("fun work(): Int { return 0 }");
        assert!(!sym(&r, "work").is_async);
    }

    #[test]
    fn class_and_method() {
        let r = extract("class Point { fun dist(): Double { return 0.0 } }");
        assert_eq!(sym(&r, "Point").kind, SymbolKind::Class);
        let m = sym(&r, "dist");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent_id.as_deref(), Some("test.kt:class:Point"));
    }

    #[test]
    fn data_class_is_class_kind() {
        let r = extract("data class Token(val value: String, val userId: Int)");
        assert_eq!(sym(&r, "Token").kind, SymbolKind::Class);
        // Primary-constructor `val` params are member properties.
        assert_eq!(sym(&r, "value").kind, SymbolKind::Variable);
        assert_eq!(sym(&r, "userId").kind, SymbolKind::Variable);
    }

    #[test]
    fn sealed_class_is_class_kind() {
        let r = extract("sealed class Result { }");
        assert_eq!(sym(&r, "Result").kind, SymbolKind::Class);
    }

    #[test]
    fn interface_is_interface_kind() {
        let r = extract("interface Repo { fun find(): User }");
        assert_eq!(sym(&r, "Repo").kind, SymbolKind::Interface);
        assert_eq!(sym(&r, "find").kind, SymbolKind::Method);
    }

    #[test]
    fn object_is_class_kind() {
        let r = extract("object Singleton {\n    fun go() {}\n}\n");
        assert_eq!(sym(&r, "Singleton").kind, SymbolKind::Class);
        assert_eq!(
            sym(&r, "go").parent_id.as_deref(),
            Some("test.kt:class:Singleton")
        );
    }

    #[test]
    fn companion_object_members_attach() {
        let r = extract(
            "class C {\n    companion object {\n        const val NAME = \"c\"\n    }\n}\n",
        );
        // The companion is a Class named `Companion`; its const is a member var.
        assert_eq!(
            sym(&r, "NAME").parent_id.as_deref(),
            Some("test.kt:class:C.Companion")
        );
    }

    #[test]
    fn enum_class_and_entries() {
        let r = extract("enum class LogLevel { DEBUG, INFO, WARN }");
        assert_eq!(sym(&r, "LogLevel").kind, SymbolKind::Enum);
        for entry in ["DEBUG", "INFO", "WARN"] {
            let e = sym(&r, entry);
            assert_eq!(e.kind, SymbolKind::EnumMember);
            assert_eq!(e.parent_id.as_deref(), Some("test.kt:enum:LogLevel"));
        }
    }

    #[test]
    fn class_property_stays_variable_kind() {
        // Regression guard: enum-member re-kinding must not leak into properties.
        let r = extract("class C { val name: String = \"\" }");
        assert_eq!(sym(&r, "name").kind, SymbolKind::Variable);
    }

    #[test]
    fn class_inherits_superclass() {
        let r = extract("class AuthService : BaseService() { }");
        assert!(has_edge(&r, EdgeKind::Inherits, "BaseService"));
    }

    #[test]
    fn class_implements_interface() {
        let r = extract("class UserValidator : Validating { }");
        assert!(has_edge(&r, EdgeKind::Implements, "Validating"));
        assert!(!has_edge(&r, EdgeKind::Inherits, "Validating"));
    }

    #[test]
    fn class_inherits_and_implements() {
        let r = extract("class AuthService : BaseService(), Validating { }");
        assert!(has_edge(&r, EdgeKind::Inherits, "BaseService"));
        assert!(has_edge(&r, EdgeKind::Implements, "Validating"));
    }

    #[test]
    fn interface_supertype_is_implements() {
        let r = extract("interface Admin : User { }");
        assert!(has_edge(&r, EdgeKind::Implements, "User"));
        assert!(r.edges.iter().all(|e| e.kind != EdgeKind::Inherits));
    }

    #[test]
    fn secondary_constructor_is_method() {
        let r = extract("class Conn { constructor(dsn: String) { connect() } }");
        let m = sym(&r, "constructor");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent_id.as_deref(), Some("test.kt:class:Conn"));
        // Calls inside the constructor body attribute to it.
        assert!(r.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.target_name == "connect"
            && e.source_id == "test.kt:method:Conn.constructor"));
    }

    #[test]
    fn property_is_variable() {
        let r = extract("class C { val name: String = \"\"\n var count = 0 }");
        assert_eq!(sym(&r, "name").kind, SymbolKind::Variable);
        assert_eq!(sym(&r, "count").kind, SymbolKind::Variable);
    }

    #[test]
    fn top_level_property() {
        let r = extract("val maxRetries = 3");
        let v = sym(&r, "maxRetries");
        assert_eq!(v.kind, SymbolKind::Variable);
        assert_eq!(v.parent_id, None);
    }

    #[test]
    fn typealias_is_type_alias() {
        let r = extract("typealias Handler = (Request) -> User");
        assert_eq!(sym(&r, "Handler").kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn typealias_emits_type_refs() {
        let r = extract("typealias Handler = (Request) -> User");
        assert!(has_edge(&r, EdgeKind::References, "Request"));
        assert!(has_edge(&r, EdgeKind::References, "User"));
    }

    #[test]
    fn import_emits_edge() {
        let r = extract("import webapp.db.DatabaseConnection");
        assert_eq!(
            sym(&r, "webapp.db.DatabaseConnection").kind,
            SymbolKind::Import
        );
        assert!(has_edge(&r, EdgeKind::Imports, "DatabaseConnection"));
    }

    #[test]
    fn wildcard_import_targets_package() {
        let r = extract("import webapp.util.*");
        assert!(has_edge(&r, EdgeKind::Imports, "webapp.util"));
    }

    #[test]
    fn function_calls_attributed_to_caller() {
        let r = extract("fun p() {\n    val d = fetch()\n    save(transform(d))\n}\n");
        assert!(has_edge(&r, EdgeKind::Calls, "fetch"));
        assert!(has_edge(&r, EdgeKind::Calls, "transform"));
        assert!(has_edge(&r, EdgeKind::Calls, "save"));
        assert!(r
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .all(|e| e.source_id == "test.kt:function:p"));
    }

    #[test]
    fn method_call_target_is_trailing_segment() {
        let r = extract("fun p() {\n    obj.doThing()\n}\n");
        assert!(has_edge(&r, EdgeKind::Calls, "doThing"));
    }

    #[test]
    fn type_refs_in_signature() {
        let r = extract("fun h(u: User, n: Int): Response { return r }");
        assert!(has_edge(&r, EdgeKind::References, "User"));
        assert!(has_edge(&r, EdgeKind::References, "Response"));
        assert_eq!(edges_to(&r, EdgeKind::References, "Int"), 0);
    }

    #[test]
    fn builtin_types_not_emitted() {
        let r = extract("fun f(a: String, b: Boolean): Double { return 0.0 }");
        for t in ["String", "Boolean", "Double"] {
            assert_eq!(edges_to(&r, EdgeKind::References, t), 0, "{t} leaked");
        }
    }

    #[test]
    fn visibility_modifiers() {
        let r = extract("private fun secret() {}\npublic fun open() {}\ninternal fun mid() {}");
        assert_eq!(sym(&r, "secret").visibility, Visibility::Private);
        assert_eq!(sym(&r, "open").visibility, Visibility::Public);
        assert_eq!(sym(&r, "mid").visibility, Visibility::Public);
    }

    #[test]
    fn kdoc_block_is_captured() {
        let r = extract("/** Does X. */\nfun x() {}");
        assert_eq!(sym(&r, "x").docstring.as_deref(), Some("Does X."));
    }

    #[test]
    fn kdoc_multiline_is_joined() {
        let r = extract("/**\n * First line.\n * Second line.\n */\nclass Y { }");
        assert_eq!(
            sym(&r, "Y").docstring.as_deref(),
            Some("First line. Second line.")
        );
    }

    #[test]
    fn kdoc_across_blank_line_not_attributed() {
        let r = extract("/** unrelated */\n\nfun gap() {}");
        assert_eq!(sym(&r, "gap").docstring, None);
    }

    #[test]
    fn kdoc_adjacent_is_attributed() {
        let r = extract("/** right above */\nfun near() {}");
        assert_eq!(sym(&r, "near").docstring.as_deref(), Some("right above"));
    }

    #[test]
    fn annotation_does_not_emit_phantom_reference() {
        let r = extract("@Deprecated(\"x\") fun z(): User { return u }");
        assert!(has_edge(&r, EdgeKind::References, "User"));
        assert_eq!(edges_to(&r, EdgeKind::References, "Deprecated"), 0);
    }

    #[test]
    fn generic_param_not_emitted_as_ref() {
        let r = extract("class Bag<T> { fun noop() {} }");
        assert_eq!(edges_to(&r, EdgeKind::References, "T"), 0);
    }

    #[test]
    fn nested_type_qualified_name() {
        let r = extract("class Outer {\n    class Inner {\n        fun go() {}\n    }\n}\n");
        assert_eq!(
            sym(&r, "go").parent_id.as_deref(),
            Some("test.kt:class:Outer.Inner")
        );
    }

    #[test]
    fn empty_file() {
        let r = extract("");
        assert!(r.symbols.is_empty());
        assert!(r.edges.is_empty());
    }

    #[test]
    fn syntax_error_partial_parse_does_not_panic() {
        // A valid declaration before a broken one still yields its symbol, no panic.
        let r = extract("fun ok() {}\nclass Broken {\n");
        assert!(r.symbols.iter().any(|s| s.name == "ok"));
    }

    #[test]
    fn primary_constructor_non_property_param_is_not_a_symbol() {
        let r = extract("class C(name: String) { }");
        // `name` has no val/var, so it is a plain param, not a member property.
        assert!(!r.symbols.iter().any(|s| s.name == "name"));
    }

    #[test]
    fn primary_constructor_param_type_referenced() {
        let r = extract("class C(private val db: DatabaseConnection)");
        assert!(has_edge(&r, EdgeKind::References, "DatabaseConnection"));
    }

    #[test]
    fn property_initializer_calls_are_captured() {
        let r = extract("class Api { val client = buildClient() }");
        assert!(r.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.target_name == "buildClient"
            && e.source_id == "test.kt:variable:Api.client"));
    }

    #[test]
    fn enum_raw_value_supertype_not_builtin() {
        let r = extract("enum class Role(val level: Int) { GUEST(0) }");
        assert_eq!(edges_to(&r, EdgeKind::References, "Int"), 0);
    }

    #[test]
    fn deeply_nested_input_is_skipped_not_panicked() {
        let src = format!("fun f() {{ {} }}", "g(".repeat(2000) + &")".repeat(2000));
        let r = extract(&src);
        assert!(r.symbols.is_empty(), "over-deep file should be skipped");
    }

    // ── Regression tests for the post-review fixes ──

    #[test]
    fn qualified_superclass_resolves_to_leaf() {
        let r = extract("class X : com.example.Base()\n");
        assert!(has_edge(&r, EdgeKind::Inherits, "Base"));
        assert!(!has_edge(&r, EdgeKind::Inherits, "com"));
    }

    #[test]
    fn qualified_interface_resolves_to_leaf() {
        let r = extract("class X : com.example.Drawable\n");
        assert!(has_edge(&r, EdgeKind::Implements, "Drawable"));
        assert!(!has_edge(&r, EdgeKind::Implements, "com"));
    }

    #[test]
    fn qualified_type_ref_is_single_leaf_edge() {
        let r = extract("fun h(): com.example.User { return x }\n");
        assert_eq!(edges_to(&r, EdgeKind::References, "User"), 1);
        assert_eq!(edges_to(&r, EdgeKind::References, "com"), 0);
        assert_eq!(edges_to(&r, EdgeKind::References, "example"), 0);
    }

    #[test]
    fn qualified_builtin_supertype_is_filtered() {
        let r = extract("class Y : kotlin.Comparable\n");
        assert!(!has_edge(&r, EdgeKind::Implements, "kotlin"));
        assert!(!has_edge(&r, EdgeKind::Implements, "Comparable"));
    }

    #[test]
    fn generic_type_argument_ref_emitted() {
        let r = extract("fun h(): Box<User> { return x }\n");
        assert!(has_edge(&r, EdgeKind::References, "User"));
    }

    #[test]
    fn init_block_calls_attributed_to_type() {
        let r = extract("class A {\n    init { setup() }\n}\n");
        assert!(r.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.target_name == "setup"
            && e.source_id == "test.kt:class:A"));
    }

    #[test]
    fn multiline_accessor_calls_captured() {
        let r = extract(
            "class B {\n    var p: Int = 0\n        get() { return compute() }\n        set(v) { store(v) }\n}\n",
        );
        assert!(has_edge(&r, EdgeKind::Calls, "compute"));
        assert!(has_edge(&r, EdgeKind::Calls, "store"));
    }

    #[test]
    fn delegate_lambda_calls_captured() {
        let r = extract("class C {\n    val cache by lazy { buildCache() }\n}\n");
        assert!(has_edge(&r, EdgeKind::Calls, "buildCache"));
    }

    #[test]
    fn import_alias_targets_alias_name() {
        let r = extract("import com.example.Foo as Bar\n");
        assert!(has_edge(&r, EdgeKind::Imports, "Bar"));
        assert!(!has_edge(&r, EdgeKind::Imports, "Foo"));
    }

    #[test]
    fn primary_constructor_default_value_call_captured() {
        let r = extract("class Service(val client: Client = buildClient())\n");
        assert!(has_edge(&r, EdgeKind::Calls, "buildClient"));
    }

    #[test]
    fn secondary_constructor_delegation_call_captured() {
        // The `this`/`super` keyword is not a call node, but calls in the delegation
        // arguments (and the body) are captured.
        let r = extract("class D {\n    constructor() : this(seed()) { log() }\n}\n");
        assert!(has_edge(&r, EdgeKind::Calls, "seed"));
        assert!(has_edge(&r, EdgeKind::Calls, "log"));
    }

    #[test]
    fn object_superclass_is_inherits() {
        let r = extract("object Singleton : BaseService() {\n    fun go() {}\n}\n");
        assert!(has_edge(&r, EdgeKind::Inherits, "BaseService"));
        assert!(!has_edge(&r, EdgeKind::Implements, "BaseService"));
    }

    #[test]
    fn protected_is_protected_visibility() {
        let r = extract("class C {\n    protected fun p() {}\n}\n");
        assert_eq!(sym(&r, "p").visibility, Visibility::Protected);
    }

    #[test]
    fn secondary_constructor_signature_has_no_brace() {
        let r = extract("class Conn {\n    constructor(dsn: String) { connect() }\n}\n");
        let sig = sym(&r, "constructor").signature.as_deref().unwrap();
        assert!(!sig.contains('{'), "signature leaked a brace: {sig:?}");
        assert!(sig.contains("constructor(dsn: String)"));
    }
}
