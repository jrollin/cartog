//! Swift extractor: symbols and edges from Swift source via tree-sitter.
//!
//! The alex-pinkus grammar folds class/struct/enum/extension/actor into a single
//! `class_declaration` node disambiguated by the `declaration_kind` field; `protocol`
//! is a separate `protocol_declaration`. Members are direct named children of the
//! type body (`class_body`/`enum_class_body`/`protocol_body`), with no member wrapper.

use anyhow::Result;
use cartog_core::{Edge, EdgeKind, Symbol, SymbolKind, Visibility};
use tree_sitter::{Language, Node, Parser};

use crate::{
    node_text, qualified, tree_depth_exceeds, ExtractionResult, ParentScope, MAX_TREE_DEPTH,
};

/// Extracts symbols and edges from Swift source files.
pub struct SwiftExtractor {
    parser: Parser,
}

impl SwiftExtractor {
    /// Create a new Swift extractor with the tree-sitter Swift grammar loaded.
    #[must_use]
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_swift::LANGUAGE))
            .expect("Swift grammar should always load");
        Self { parser }
    }
}

impl Default for SwiftExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::Extractor for SwiftExtractor {
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
        "protocol_declaration" => {
            extract_protocol(node, source, file_path, parent, symbols, edges);
        }
        "function_declaration" | "protocol_function_declaration" => {
            extract_function(node, source, file_path, parent, symbols, edges);
        }
        "init_declaration" | "deinit_declaration" | "subscript_declaration" => {
            extract_special_member(node, source, file_path, parent, symbols, edges);
        }
        "property_declaration" | "protocol_property_declaration" => {
            extract_property(node, source, file_path, parent, symbols, edges);
        }
        "enum_entry" => extract_enum_entry(node, source, file_path, parent, symbols),
        "typealias_declaration" | "associatedtype_declaration" => {
            extract_typealias(node, source, file_path, parent, symbols, edges);
        }
        "import_declaration" => extract_import(node, source, file_path, parent, symbols, edges),
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_node(child, source, file_path, parent, symbols, edges);
            }
        }
    }
}

/// class / struct / enum / extension / actor — one `class_declaration` node keyed by
/// the `declaration_kind` field.
fn extract_type_like(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let kw = node
        .child_by_field_name("declaration_kind")
        .map(|n| n.kind())
        .unwrap_or("class");
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| type_name(n, source))
    else {
        return;
    };

    // enum bodies hold cases; the keyword also drives Class-vs-Enum.
    let kind = if kw == "enum" {
        SymbolKind::Enum
    } else {
        SymbolKind::Class
    };
    // An `extension X` is not a definition of X — X is defined elsewhere — so it
    // emits a placeholder symbol (below) rather than a real type symbol. Its
    // members and conformances attach to X's class id.
    let is_extension = kw == "extension";

    let qname = qualified(parent.qname, &name);
    let id = cartog_core::symbol_id(file_path, kind, &name, parent.qname);
    if !is_extension {
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
    }

    // The scope/edge owner is the type's own id; for an extension it's the
    // extended type's class id so members and conformances merge into X.
    let owner_id = if is_extension {
        cartog_core::symbol_id(file_path, SymbolKind::Class, &name, parent.qname)
    } else {
        id
    };

    // The extended type is defined elsewhere, so emit a placeholder Class symbol
    // when none exists yet, mirroring Rust's impl-block handling: conformance
    // edges and member parent_ids need a local symbol with `owner_id`. The guard
    // keeps two `extension X` in one file collapsing onto a single placeholder.
    if is_extension && !symbols.iter().any(|s| s.id == owner_id) {
        symbols.push(Symbol::new(
            &name,
            SymbolKind::Class,
            file_path,
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            parent.qname,
        ));
    }

    // class → first specifier inherits, rest conform; others (incl. extension
    // conformances) → all conform.
    let is_class = kw == "class";
    inheritance_edges(node, source, file_path, &owner_id, is_class, edges);

    let scope = ParentScope::nested(&owner_id, &qname);
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            extract_node(member, source, file_path, scope, symbols, edges);
        }
    }
}

/// protocol — separate node, maps to Interface.
fn extract_protocol(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| type_name(n, source))
    else {
        return;
    };
    let qname = qualified(parent.qname, &name);
    let id = cartog_core::symbol_id(file_path, SymbolKind::Interface, &name, parent.qname);
    let sym = Symbol::new(
        &name,
        SymbolKind::Interface,
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

    // Protocol refinement (`protocol P: Q`) is recorded as Implements — no "refines" edge kind.
    inheritance_edges(node, source, file_path, &id, false, edges);

    let scope = ParentScope::nested(&id, &qname);
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        let mut seen = std::collections::HashSet::new();
        for member in body.named_children(&mut cursor) {
            // protocol_function_declaration appears twice (body field + children); dedup by id range.
            if seen.insert(member.id()) {
                extract_node(member, source, file_path, scope, symbols, edges);
            }
        }
    }
}

/// func (top-level → Function, in a type → Method) and protocol requirements.
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
    .with_async(is_async(node, source))
    .with_docstring(doc_comment(node, source))
    .with_signature(Some(header_signature(node, source)));
    symbols.push(sym);

    signature_type_refs(node, source, file_path, &id, edges);
    if let Some(body) = node.child_by_field_name("body") {
        walk_calls(body, source, file_path, &id, edges);
        let qname = qualified(parent.qname, &name);
        let scope = ParentScope::nested(&id, &qname);
        walk_nested_decls(body, source, file_path, scope, symbols, edges);
    }
}

/// init / deinit / subscript — always methods of the enclosing type.
fn extract_special_member(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let name = match node.kind() {
        "init_declaration" => "init",
        "deinit_declaration" => "deinit",
        _ => "subscript",
    };
    let id = cartog_core::symbol_id(file_path, SymbolKind::Method, name, parent.qname);
    let sym = Symbol::new(
        name,
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
    .with_async(is_async(node, source))
    .with_docstring(doc_comment(node, source))
    .with_signature(Some(header_signature(node, source)));
    symbols.push(sym);

    signature_type_refs(node, source, file_path, &id, edges);
    if let Some(body) = node.child_by_field_name("body") {
        walk_calls(body, source, file_path, &id, edges);
        let qname = qualified(parent.qname, name);
        let scope = ParentScope::nested(&id, &qname);
        walk_nested_decls(body, source, file_path, scope, symbols, edges);
    }
}

/// let / var bindings (top-level or member). One Variable per bound identifier.
fn extract_property(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    let mut names = Vec::new();
    for pat in node.children_by_field_name("name", &mut cursor) {
        pattern_identifiers(pat, source, &mut names);
    }
    if names.is_empty() {
        return;
    }
    let vis = visibility(node, source);
    let docstring = doc_comment(node, source);
    let first_id = cartog_core::symbol_id(file_path, SymbolKind::Variable, &names[0], parent.qname);
    for name in &names {
        let sym = Symbol::new(
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
        .with_docstring(docstring.clone());
        symbols.push(sym);
    }
    // Type annotations and initializer/getter calls are attributed once to the
    // first binding (avoids N-fold duplication for `let a, b: User`).
    signature_type_refs(node, source, file_path, &first_id, edges);
    if let Some(value) = node.child_by_field_name("value") {
        walk_calls(value, source, file_path, &first_id, edges);
    }
    if let Some(computed) = node.child_by_field_name("computed_value") {
        walk_calls(computed, source, file_path, &first_id, edges);
    }
}

/// enum cases — one EnumMember per `name` identifier (`case a, b` → two).
fn extract_enum_entry(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    let names: Vec<String> = node
        .children_by_field_name("name", &mut cursor)
        .filter(|n| n.kind() == "simple_identifier")
        .map(|n| node_text(n, source).to_string())
        .collect();
    for name in names {
        symbols.push(
            Symbol::new(
                &name,
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
}

/// typealias / associatedtype → TypeAlias; the RHS contributes References.
fn extract_typealias(
    node: Node,
    source: &str,
    file_path: &str,
    parent: ParentScope<'_>,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // The first `name`-field child is the alias name; later type children are the RHS.
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| type_name(n, source))
    else {
        return;
    };
    let id = cartog_core::symbol_id(file_path, SymbolKind::TypeAlias, &name, parent.qname);
    let sym = Symbol::new(
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
    .with_signature(Some(header_signature(node, source)));
    symbols.push(sym);

    // Emit refs for every type identifier except the alias's own name node.
    let name_node = node.child_by_field_name("name");
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) == name_node.map(|n| n.id()) {
            continue;
        }
        collect_type_refs(child, source, file_path, &id, edges);
    }
}

/// import Foo / import Foo.Bar → Import symbol + Imports edge (target = first segment).
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
    let target = full.split('.').next().unwrap_or(&full).to_string();
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

/// Emit Inherits/Implements from `inheritance_specifier` children.
/// For a class the first specifier is the superclass (Inherits); the rest conform.
fn inheritance_edges(
    node: Node,
    source: &str,
    file_path: &str,
    source_id: &str,
    is_class: bool,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    let mut first = true;
    for spec in node.named_children(&mut cursor) {
        if spec.kind() != "inheritance_specifier" {
            continue;
        }
        let Some(target) = spec
            .child_by_field_name("inherits_from")
            .and_then(|n| type_name(n, source))
        else {
            continue;
        };
        if is_builtin_type(&target) {
            first = false;
            continue;
        }
        let kind = if is_class && first {
            EdgeKind::Inherits
        } else {
            EdgeKind::Implements
        };
        first = false;
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
/// property type annotation), skipping builtins and generic-parameter declarations.
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
            // Don't descend bodies, generic-parameter declarations, or `modifiers`
            // (attributes like `@MainActor`/`@Published` would otherwise leak as
            // spurious type References).
            "function_body" | "class_body" | "enum_class_body" | "protocol_body"
            | "computed_property" | "type_parameters" | "modifiers" => {}
            _ => collect_type_refs(child, source, file_path, source_id, edges),
        }
    }
}

/// Recurse a subtree emitting a References edge per non-builtin `type_identifier`.
fn collect_type_refs(
    node: Node,
    source: &str,
    file_path: &str,
    source_id: &str,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    if node.kind() == "type_identifier" {
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
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_refs(child, source, file_path, source_id, edges);
    }
}

/// Walk a subtree for `call_expression` nodes, emitting Calls edges. Checks the
/// node itself (so it works when called directly on a property initializer that
/// IS a call), then recurses, skipping nested function/closure bodies — their
/// calls belong to the nested symbol.
fn walk_calls(node: Node, source: &str, file_path: &str, source_id: &str, edges: &mut Vec<Edge>) {
    crate::parse::guard_recursion!();
    match node.kind() {
        // Nested decls own their own calls; don't descend.
        "function_declaration"
        | "lambda_literal"
        | "init_declaration"
        | "deinit_declaration"
        | "subscript_declaration" => return,
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

/// Walk a body for nested `function_declaration`s, emitting each as a Method
/// symbol scoped to `parent` and attributing its own calls/refs to it. Does not
/// descend into the nested function's body for the OUTER symbol's calls (those
/// belong to the nested symbol, handled by its own `extract_function`).
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
            // Don't descend type bodies or closures — their members aren't local
            // functions of the enclosing symbol.
            "lambda_literal" | "class_declaration" | "protocol_declaration" => {}
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
            let suffix = head.child_by_field_name("suffix")?;
            // The trailing `navigation_suffix` holds the called member's identifier.
            let ident = suffix.child_by_field_name("suffix").unwrap_or(suffix);
            if ident.kind() == "simple_identifier" {
                return Some(node_text(ident, source).to_string());
            }
            let mut cursor = ident.walk();
            let found = ident
                .children(&mut cursor)
                .find(|c| c.kind() == "simple_identifier")
                .map(|c| node_text(c, source).to_string());
            found
        }
        _ => None,
    }
}

/// Function/method name: the first `name`-field child that is an identifier or an
/// operator token. The grammar reuses the `name` field for the return type (a
/// `user_type`/`type_identifier`), so skip those; operator methods (`func +`,
/// `func ==`) carry the operator token itself in the `name` field.
fn func_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let found = node
        .children_by_field_name("name", &mut cursor)
        .find(|n| !matches!(n.kind(), "user_type" | "type_identifier"))
        .map(|n| node_text(n, source).trim().to_string())
        .filter(|s| !s.is_empty());
    found
}

/// Leaf type name from a `type_identifier` / `user_type` node.
fn type_name(node: Node, source: &str) -> Option<String> {
    crate::parse::guard_recursion!(None);
    match node.kind() {
        "type_identifier" => Some(node_text(node, source).to_string()),
        "user_type" => {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find(|c| c.kind() == "type_identifier")
                .map(|c| node_text(c, source).to_string());
            found
        }
        _ => {
            // Fallback: first descendant type_identifier.
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find_map(|c| type_name(c, source));
            found
        }
    }
}

/// All identifiers bound by a `pattern` node. A simple binding yields one name;
/// a tuple-destructuring pattern (`(a, b)`) yields one per element.
fn pattern_identifiers(pattern: Node, source: &str, out: &mut Vec<String>) {
    crate::parse::guard_recursion!();
    match pattern.kind() {
        "simple_identifier" => out.push(node_text(pattern, source).to_string()),
        _ => {
            if let Some(n) = pattern.child_by_field_name("bound_identifier") {
                out.push(node_text(n, source).to_string());
                return;
            }
            // Nested patterns (tuple destructuring) or a direct identifier child.
            let mut cursor = pattern.walk();
            for child in pattern.children(&mut cursor) {
                match child.kind() {
                    "simple_identifier" => out.push(node_text(child, source).to_string()),
                    "pattern" => pattern_identifiers(child, source, out),
                    _ => {}
                }
            }
        }
    }
}

/// Visibility from a `modifiers → visibility_modifier` child; default (internal) is Public.
fn visibility(node: Node, source: &str) -> Visibility {
    let mut cursor = node.walk();
    let Some(modifiers) = node.children(&mut cursor).find(|c| c.kind() == "modifiers") else {
        return Visibility::Public;
    };
    let mut mc = modifiers.walk();
    for m in modifiers.children(&mut mc) {
        if m.kind() == "visibility_modifier" {
            // `private(set)`/`fileprivate(set)` carry the access list in the token
            // text, so match the leading keyword rather than the whole string.
            let text = node_text(m, source).trim_start();
            if text.starts_with("private") || text.starts_with("fileprivate") {
                return Visibility::Private;
            }
            return Visibility::Public;
        }
    }
    Visibility::Public
}

/// True when the declaration itself is `async`. The `async` token is a direct
/// child of the function node; an `async` buried inside a parameter's closure
/// type (`(_ work: () async -> Void)`) is NOT the function being async, so a
/// direct-child check avoids that false positive.
fn is_async(node: Node, _source: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == "async");
    found
}

/// Declaration header (name + params + async/throws + return type), body stripped.
fn header_signature(node: Node, source: &str) -> String {
    let body_kinds = [
        "function_body",
        "class_body",
        "enum_class_body",
        "protocol_body",
        "computed_property",
    ];
    let mut cursor = node.walk();
    let end = node
        .children(&mut cursor)
        .find(|c| body_kinds.contains(&c.kind()))
        .map(|b| b.start_byte())
        .unwrap_or(node.end_byte());
    source
        .get(node.start_byte()..end)
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Doc comment from preceding `///` lines or a `/** */` block, but only when it
/// is on the line immediately above the declaration (a blank line breaks the
/// association, matching how Swift doc comments attach).
fn doc_comment(node: Node, source: &str) -> Option<String> {
    let prev = node.prev_sibling()?;
    // Reject a comment separated from the declaration by a blank line.
    if prev.end_position().row + 1 < node.start_position().row {
        return None;
    }
    if prev.kind() == "multiline_comment" {
        let text = node_text(prev, source);
        if let Some(inner) = text.strip_prefix("/**").and_then(|t| t.strip_suffix("*/")) {
            let joined = inner
                .lines()
                .map(|l| l.trim().trim_start_matches('*').trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            return (!joined.is_empty()).then_some(joined);
        }
        return None;
    }

    let mut lines = Vec::new();
    let mut cur = Some(prev);
    let mut next_row = node.start_position().row;
    while let Some(p) = cur {
        if p.kind() != "comment" {
            break;
        }
        // Each `///` line must be contiguous with the line below it.
        if p.end_position().row + 1 < next_row {
            break;
        }
        let text = node_text(p, source);
        match text.strip_prefix("///") {
            Some(stripped) => lines.push(stripped.trim().to_string()),
            None => break,
        }
        next_row = p.start_position().row;
        cur = p.prev_sibling();
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join(" "))
}

/// Swift stdlib types excluded from References / inheritance edges.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "Int"
            | "Int8"
            | "Int16"
            | "Int32"
            | "Int64"
            | "UInt"
            | "UInt8"
            | "UInt16"
            | "UInt32"
            | "UInt64"
            | "Float"
            | "Float32"
            | "Float64"
            | "Double"
            | "Bool"
            | "String"
            | "Character"
            | "Substring"
            | "Void"
            | "Any"
            | "AnyObject"
            | "AnyClass"
            | "Never"
            | "Optional"
            | "Array"
            | "Dictionary"
            | "Set"
            | "Result"
            | "Range"
            | "ClosedRange"
            | "Error"
            | "Sequence"
            | "Collection"
            | "Iterator"
            | "Comparable"
            | "Equatable"
            | "Hashable"
            | "Codable"
            | "Encodable"
            | "Decodable"
            | "Identifiable"
            | "Sendable"
            | "CustomStringConvertible"
            | "Self"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Extractor;

    fn extract(source: &str) -> ExtractionResult {
        SwiftExtractor::new().extract(source, "test.swift").unwrap()
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
        let r = extract("func greet(name: String) -> String { return name }");
        let f = sym(&r, "greet");
        assert_eq!(f.kind, SymbolKind::Function);
        assert_eq!(f.visibility, Visibility::Public);
        assert!(f.signature.as_deref().unwrap().contains("greet"));
    }

    #[test]
    fn async_function() {
        let r = extract("func fetch() async -> Int { return 0 }");
        assert!(sym(&r, "fetch").is_async);
    }

    #[test]
    fn throwing_function_keeps_throws_and_emits_no_raises() {
        let r = extract("func load() throws { try work() }");
        assert!(sym(&r, "load")
            .signature
            .as_deref()
            .unwrap()
            .contains("throws"));
        assert!(r.edges.iter().all(|e| e.kind != EdgeKind::Raises));
    }

    #[test]
    fn struct_and_method() {
        let r = extract("struct Point { func dist() -> Double { return 0 } }");
        assert_eq!(sym(&r, "Point").kind, SymbolKind::Class);
        let m = sym(&r, "dist");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent_id.as_deref(), Some("test.swift:class:Point"));
    }

    #[test]
    fn class_struct_actor_all_class_kind() {
        let r = extract("class C {}\nstruct S {}\nactor A {}");
        assert_eq!(sym(&r, "C").kind, SymbolKind::Class);
        assert_eq!(sym(&r, "S").kind, SymbolKind::Class);
        assert_eq!(sym(&r, "A").kind, SymbolKind::Class);
    }

    #[test]
    fn protocol_is_interface() {
        let r = extract("protocol P { func f() }");
        assert_eq!(sym(&r, "P").kind, SymbolKind::Interface);
        assert_eq!(sym(&r, "f").kind, SymbolKind::Method);
    }

    #[test]
    fn enum_and_cases() {
        let r = extract("enum Color { case red, green\n case blue }");
        assert_eq!(sym(&r, "Color").kind, SymbolKind::Enum);
        for case in ["red", "green", "blue"] {
            let c = sym(&r, case);
            assert_eq!(c.kind, SymbolKind::EnumMember);
            assert_eq!(c.parent_id.as_deref(), Some("test.swift:enum:Color"));
        }
    }

    #[test]
    fn class_property_stays_variable_kind() {
        // Regression guard: enum-member re-kinding must not leak into properties.
        let r = extract("class C { let name: String\n var count = 0 }");
        assert_eq!(sym(&r, "name").kind, SymbolKind::Variable);
        assert_eq!(sym(&r, "count").kind, SymbolKind::Variable);
    }

    #[test]
    fn enum_raw_value_type_not_implemented() {
        let r = extract("enum E: Int { case a }");
        assert!(!has_edge(&r, EdgeKind::Implements, "Int"));
    }

    #[test]
    fn class_inherits_first_then_implements() {
        let r = extract("class Dog: Animal, Pet, Drawable {}");
        assert!(has_edge(&r, EdgeKind::Inherits, "Animal"));
        assert!(has_edge(&r, EdgeKind::Implements, "Pet"));
        assert!(has_edge(&r, EdgeKind::Implements, "Drawable"));
        assert!(!has_edge(&r, EdgeKind::Inherits, "Pet"));
    }

    #[test]
    fn struct_conformance_emits_implements_only() {
        let r = extract("struct S: Drawable {}");
        assert!(has_edge(&r, EdgeKind::Implements, "Drawable"));
        assert!(r.edges.iter().all(|e| e.kind != EdgeKind::Inherits));
    }

    #[test]
    fn protocol_refinement_emits_implements() {
        let r = extract("protocol P: Refinable {}");
        assert!(has_edge(&r, EdgeKind::Implements, "Refinable"));
    }

    #[test]
    fn extension_members_attach_to_extended_type() {
        let r = extract("extension Greeter { func shout() -> Greeter { return self } }");
        // The extended type is defined elsewhere, so the extension emits one
        // placeholder Class symbol; members attach to its class id.
        assert_eq!(r.symbols.iter().filter(|s| s.name == "Greeter").count(), 1);
        assert_eq!(
            sym(&r, "shout").parent_id.as_deref(),
            Some("test.swift:class:Greeter")
        );
    }

    #[test]
    fn duplicate_extensions_emit_single_placeholder_and_share_scope() {
        let r = extract("extension X { func a() {} }\nextension X { func b() {} }");
        // Two extensions of X collapse onto one placeholder (no id collision),
        // and both members survive under X's scope.
        assert_eq!(r.symbols.iter().filter(|s| s.name == "X").count(), 1);
        assert_eq!(
            sym(&r, "a").parent_id.as_deref(),
            Some("test.swift:class:X")
        );
        assert_eq!(
            sym(&r, "b").parent_id.as_deref(),
            Some("test.swift:class:X")
        );
    }

    #[test]
    fn extension_adds_conformance() {
        let r = extract("extension Foo: Bar {}");
        assert!(has_edge(&r, EdgeKind::Implements, "Bar"));
    }

    #[test]
    fn extension_conformance_edge_has_local_source_symbol() {
        // Regression for the cross-file FK crash: the conformance edge's source_id
        // must match a symbol emitted by this extraction, not a dangling id.
        let r = extract("extension Foo: Bar {}");
        let edge = r
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Implements && e.target_name == "Bar")
            .expect("implements edge");
        assert!(
            r.symbols.iter().any(|s| s.id == edge.source_id),
            "conformance edge source_id {} has no matching local symbol",
            edge.source_id
        );
    }

    #[test]
    fn init_is_method() {
        let r = extract("class C { init(x: Int) {} }");
        let m = sym(&r, "init");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent_id.as_deref(), Some("test.swift:class:C"));
    }

    #[test]
    fn deinit_is_method() {
        let r = extract("class C { deinit {} }");
        assert_eq!(sym(&r, "deinit").kind, SymbolKind::Method);
    }

    #[test]
    fn subscript_is_method() {
        let r = extract("struct S { subscript(i: Int) -> Int { return i } }");
        let m = sym(&r, "subscript");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent_id.as_deref(), Some("test.swift:class:S"));
    }

    #[test]
    fn property_is_variable() {
        let r = extract("class C { let name: String\n var count = 0 }");
        assert_eq!(sym(&r, "name").kind, SymbolKind::Variable);
        assert_eq!(sym(&r, "count").kind, SymbolKind::Variable);
    }

    #[test]
    fn top_level_property() {
        let r = extract("let maxRetries = 3");
        let v = sym(&r, "maxRetries");
        assert_eq!(v.kind, SymbolKind::Variable);
        assert_eq!(v.parent_id, None);
    }

    #[test]
    fn typealias_is_type_alias() {
        let r = extract("typealias IntList = Array<Int>");
        assert_eq!(sym(&r, "IntList").kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn typealias_emits_type_refs() {
        let r = extract("typealias Handler = (Request) -> User");
        assert!(has_edge(&r, EdgeKind::References, "Request"));
        assert!(has_edge(&r, EdgeKind::References, "User"));
    }

    #[test]
    fn associatedtype_is_type_alias() {
        let r = extract("protocol P { associatedtype Item }");
        assert_eq!(sym(&r, "Item").kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn import_emits_edge() {
        let r = extract("import Foundation");
        assert_eq!(sym(&r, "Foundation").kind, SymbolKind::Import);
        assert!(has_edge(&r, EdgeKind::Imports, "Foundation"));
    }

    #[test]
    fn dotted_import_targets_first_segment() {
        let r = extract("import UIKit.UIView");
        assert!(has_edge(&r, EdgeKind::Imports, "UIKit"));
    }

    #[test]
    fn function_calls_attributed_to_caller() {
        let r = extract("func p() { let d = fetch()\n save(transform(d)) }");
        assert!(has_edge(&r, EdgeKind::Calls, "fetch"));
        assert!(has_edge(&r, EdgeKind::Calls, "transform"));
        assert!(has_edge(&r, EdgeKind::Calls, "save"));
        assert!(r
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .all(|e| e.source_id == "test.swift:function:p"));
    }

    #[test]
    fn method_call_target_is_trailing_segment() {
        let r = extract("func p() { obj.doThing() }");
        assert!(has_edge(&r, EdgeKind::Calls, "doThing"));
    }

    #[test]
    fn type_refs_in_signature() {
        let r = extract("func h(u: User, n: Int) -> Response { return r }");
        assert!(has_edge(&r, EdgeKind::References, "User"));
        assert!(has_edge(&r, EdgeKind::References, "Response"));
        assert_eq!(edges_to(&r, EdgeKind::References, "Int"), 0);
    }

    #[test]
    fn builtin_types_not_emitted() {
        let r = extract("func f(a: String, b: Bool) -> Double { return 0 }");
        for t in ["String", "Bool", "Double"] {
            assert_eq!(edges_to(&r, EdgeKind::References, t), 0, "{t} leaked");
        }
    }

    #[test]
    fn visibility_modifiers() {
        let r = extract("private func secret() {}\npublic func open() {}\ninternal func mid() {}");
        assert_eq!(sym(&r, "secret").visibility, Visibility::Private);
        assert_eq!(sym(&r, "open").visibility, Visibility::Public);
        assert_eq!(sym(&r, "mid").visibility, Visibility::Public);
    }

    #[test]
    fn doc_comment_line() {
        let r = extract("/// Does X.\nfunc x() {}");
        assert_eq!(sym(&r, "x").docstring.as_deref(), Some("Does X."));
    }

    #[test]
    fn doc_comment_block() {
        let r = extract("/** Block doc. */\nstruct Y {}");
        assert_eq!(sym(&r, "Y").docstring.as_deref(), Some("Block doc."));
    }

    #[test]
    fn generic_param_not_emitted_as_ref() {
        let r = extract("struct Bag<T> { func noop() {} }");
        assert_eq!(edges_to(&r, EdgeKind::References, "T"), 0);
    }

    #[test]
    fn nested_type_qualified_name() {
        let r = extract("struct Outer { struct Inner { func go() {} } }");
        assert_eq!(
            sym(&r, "go").parent_id.as_deref(),
            Some("test.swift:class:Outer.Inner")
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
        let r = extract("struct Broken {");
        assert!(r.symbols.iter().any(|s| s.name == "Broken"));
    }

    // ── Regression tests for the post-review fixes ──

    #[test]
    fn operator_method_is_emitted() {
        let r = extract("struct V { static func == (a: V, b: V) -> Bool { true } }");
        let m = sym(&r, "==");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent_id.as_deref(), Some("test.swift:class:V"));
    }

    #[test]
    fn nested_local_function_is_emitted_with_its_calls() {
        let r = extract("func outer() { func inner() { helper() } }");
        let inner = sym(&r, "inner");
        assert_eq!(inner.kind, SymbolKind::Method);
        assert_eq!(
            inner.parent_id.as_deref(),
            Some("test.swift:function:outer")
        );
        // The call inside `inner` is attributed to inner, not outer.
        assert!(r.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.target_name == "helper"
            && e.source_id == "test.swift:method:outer.inner"));
    }

    #[test]
    fn property_initializer_calls_are_captured() {
        let r = extract("class Api { let client = buildClient() }");
        assert!(r.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.target_name == "buildClient"
            && e.source_id == "test.swift:variable:Api.client"));
    }

    #[test]
    fn computed_getter_calls_are_captured() {
        let r = extract("struct S { var handler: Int { return makeHandler() } }");
        assert!(has_edge(&r, EdgeKind::Calls, "makeHandler"));
    }

    #[test]
    fn attribute_does_not_emit_phantom_reference() {
        let r = extract("@MainActor func z() -> User { return u }");
        assert!(has_edge(&r, EdgeKind::References, "User"));
        assert_eq!(edges_to(&r, EdgeKind::References, "MainActor"), 0);
    }

    #[test]
    fn private_set_is_private() {
        let r = extract("class C { private(set) var v = 1 }");
        assert_eq!(sym(&r, "v").visibility, Visibility::Private);
    }

    #[test]
    fn async_closure_param_does_not_mark_function_async() {
        let r = extract("func run(_ work: () async -> Void) {}");
        assert!(!sym(&r, "run").is_async);
    }

    #[test]
    fn real_async_function_is_marked_async() {
        let r = extract("func fetch() async -> Int { return 0 }");
        assert!(sym(&r, "fetch").is_async);
    }

    #[test]
    fn multi_name_property_type_ref_emitted_once() {
        let r = extract("class C { let a, b: User }");
        assert_eq!(sym(&r, "a").kind, SymbolKind::Variable);
        assert_eq!(sym(&r, "b").kind, SymbolKind::Variable);
        assert_eq!(edges_to(&r, EdgeKind::References, "User"), 1);
    }

    #[test]
    fn tuple_destructured_property_emits_each_binding() {
        let r = extract("struct S { var (x, y) = (1, 2) }");
        assert_eq!(sym(&r, "x").kind, SymbolKind::Variable);
        assert_eq!(sym(&r, "y").kind, SymbolKind::Variable);
    }

    #[test]
    fn doc_comment_across_blank_line_not_attributed() {
        let r = extract("/// unrelated\n\nfunc gap() {}");
        assert_eq!(sym(&r, "gap").docstring, None);
    }

    #[test]
    fn doc_comment_adjacent_is_attributed() {
        let r = extract("/// right above\nfunc near() {}");
        assert_eq!(sym(&r, "near").docstring.as_deref(), Some("right above"));
    }

    #[test]
    fn deeply_nested_input_is_skipped_not_panicked() {
        // Far past MAX_TREE_DEPTH — must degrade to empty, never overflow.
        let src = format!("func f() {{ {} }}", "g(".repeat(2000) + &")".repeat(2000));
        let r = extract(&src);
        assert!(r.symbols.is_empty(), "over-deep file should be skipped");
    }
}
