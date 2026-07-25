use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use cartog_core::{symbol_id, Edge, EdgeKind, Symbol, SymbolKind, Visibility};

use super::{last_segment, node_text, qualified, ExtractionResult, Extractor, ParentScope};

pub struct CppExtractor {
    parser: Parser,
}

impl CppExtractor {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&Language::new(tree_sitter_cpp::LANGUAGE))
            .expect("C++ grammar should always load");
        Self { parser }
    }
}

impl Default for CppExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for CppExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        let tree = crate::parse_bounded(&mut self.parser, source)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {file_path}"))?;

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

fn extract_node(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    match node.kind() {
        "function_definition" => {
            extract_function(node, source, file_path, scope, symbols, edges);
        }
        "class_specifier" | "struct_specifier" | "union_specifier" => {
            extract_class_like(node, source, file_path, scope, symbols, edges);
        }
        "enum_specifier" => extract_enum(node, source, file_path, scope, symbols),
        "type_definition" => extract_typedef(node, source, file_path, scope, symbols, edges),
        "alias_declaration" => extract_alias(node, source, file_path, scope, symbols, edges),
        // Namespaces fold into the qname prefix rather than emitting a symbol (D2),
        // mirroring how csharp.rs handles `namespace Foo { }`.
        "namespace_definition" => {
            let ns = node
                .child_by_field_name("name")
                .map(|n| join_qname(scope.qname, node_text(n, source)));
            let inner = ParentScope {
                id: scope.id,
                qname: ns.as_deref().or(scope.qname),
            };
            if let Some(body) = node.child_by_field_name("body") {
                extract_node(body, source, file_path, inner, symbols, edges);
            }
        }
        "preproc_include" => extract_include(node, source, file_path, scope, symbols, edges),
        "preproc_def" | "preproc_function_def" => {
            extract_macro(node, source, file_path, scope, symbols);
        }
        // A bodiless prototype (`declaration` + `function_declarator`) emits no symbol
        // (D2): one symbol per function keeps the resolver's UniqueGlobal tier working,
        // since its `disambiguate_two` returns None for two same-kind candidates.
        //
        // Do NOT descend either: a prototype's children are only its return and
        // parameter type specifiers, and `Foo* f` there is a type USE, not a
        // definition. Walking in would emit a phantom `class Foo` from every
        // forward-declared or pointed-to type in a header. (A class body's own
        // members are reached via extract_class_body, not this arm.)
        "declaration" | "parameter_declaration" | "field_declaration" => {}
        _ => {
            for child in node.named_children(&mut node.walk()) {
                extract_node(child, source, file_path, scope, symbols, edges);
            }
        }
    }
}

/// Extend an optional qname prefix with a namespace/class segment.
fn join_qname(parent_qname: Option<&str>, seg: &str) -> String {
    match parent_qname {
        Some(pq) if !pq.is_empty() => format!("{pq}.{seg}"),
        _ => seg.to_string(),
    }
}

// ── Functions and methods ──

/// A `function_definition`. Out-of-line definitions (`int A::m() {}`) carry a
/// `qualified_identifier` declarator, so they become a Method attached to `A`.
fn extract_function(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let declarator = match find_function_declarator(node) {
        Some(d) => d,
        None => return,
    };
    let name_node = match declarator.child_by_field_name("declarator") {
        Some(n) => n,
        None => return,
    };

    // `A::m` / `app::A::m` → owner scope `A` (last segment) + bare name `m`.
    let (owner, name) = split_qualified_name(name_node, source);
    if name.is_empty() {
        return;
    }

    let kind = if owner.is_some() {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    // An out-of-line definition's qname is the class it belongs to, not the
    // enclosing namespace alone, so `refs`/`hierarchy` attach it to the class.
    let owner_qname = owner.as_deref().map(|o| join_qname(scope.qname, o));
    let parent_qname = owner_qname.as_deref().or(scope.qname);

    emit_callable(
        node,
        source,
        file_path,
        Callable {
            name: &name,
            kind,
            declarator,
        },
        ParentScope {
            id: scope.id,
            qname: parent_qname,
        },
        symbols,
        edges,
    );
}

/// What a `function_definition` resolves to: its bare name, whether it is a free
/// function or a method (an out-of-line `A::m` is a Method), and the
/// `function_declarator` holding its parameter list.
struct Callable<'a, 'tree> {
    name: &'a str,
    kind: SymbolKind,
    declarator: Node<'tree>,
}

/// Emit a function/method symbol plus its type-reference and call edges.
fn emit_callable(
    node: Node,
    source: &str,
    file_path: &str,
    callable: Callable<'_, '_>,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Callable {
        name,
        kind,
        declarator,
    } = callable;
    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, kind, name, scope.qname);

    let mut sym = Symbol::new(
        name,
        kind,
        file_path,
        start_line,
        end_line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        scope.qname,
    )
    .with_parent(scope.id)
    .with_signature(signature_of(node, declarator, source))
    .with_docstring(extract_doc_comment(node, source));
    let visibility = declared_visibility(node, source);
    if visibility != Visibility::Public {
        sym = sym.with_visibility(visibility);
    }
    symbols.push(sym);

    collect_param_and_return_refs(node, declarator, source, file_path, &sym_id, edges);
    if let Some(body) = node.child_by_field_name("body") {
        walk_for_calls(body, source, file_path, &sym_id, edges);
    }
}

// Identical to `find_function_declarator` in c.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// The `function_declarator` under a definition/field declarator, descending
/// through pointer/reference wrappers (`int* f()`, `int& g()`).
fn find_function_declarator(node: Node) -> Option<Node> {
    let mut cur = node.child_by_field_name("declarator")?;
    for _ in 0..crate::MAX_TREE_DEPTH {
        if cur.kind() == "function_declarator" {
            return Some(cur);
        }
        cur = cur.child_by_field_name("declarator")?;
    }
    None
}

/// Split a declarator name into (owner scope, bare name).
/// `m` → `(None, "m")`; `A::m` → `(Some("A"), "m")`; `app::A::m` → `(Some("A"), "m")`;
/// `A::~A` → `(Some("A"), "~A")`.
fn split_qualified_name(node: Node, source: &str) -> (Option<String>, String) {
    if node.kind() != "qualified_identifier" {
        return (None, declarator_name(node, source));
    }
    let scope_text = node
        .child_by_field_name("scope")
        .map(|s| node_text(s, source).to_string());
    match node.child_by_field_name("name") {
        // `app::A::m` nests another qualified_identifier — recurse so the owner
        // is the innermost scope (`A`), not the outermost namespace (`app`).
        Some(inner) if inner.kind() == "qualified_identifier" => {
            split_qualified_name(inner, source)
        }
        Some(inner) => (scope_text, declarator_name(inner, source)),
        None => (scope_text, String::new()),
    }
}

/// Bare name of a declarator node (`identifier`, `field_identifier`,
/// `destructor_name` → `~A`, `operator_name` → `operator+`).
fn declarator_name(node: Node, source: &str) -> String {
    match node.kind() {
        "destructor_name" | "operator_name" => node_text(node, source).trim().to_string(),
        _ => node_text(node, source).to_string(),
    }
}

// Identical to `signature_of` in c.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// `(params) -> ret`-style signature: the declared return type plus parameter list.
fn signature_of(node: Node, declarator: Node, source: &str) -> Option<String> {
    let params = declarator
        .child_by_field_name("parameters")
        .map(|p| node_text(p, source))
        .unwrap_or("()");
    let ret = node
        .child_by_field_name("type")
        .map(|t| node_text(t, source));
    Some(match ret {
        Some(r) => format!("{r} {params}"),
        None => params.to_string(),
    })
}

// ── Classes, structs, unions ──

fn extract_class_like(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // An anonymous struct/union (or a `struct Foo x;` type reference with no body)
    // carries no name to key a symbol on — skip it rather than inventing one.
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };
    // A bodiless specifier is a forward declaration (`class Fwd;`), not a
    // definition — the definition elsewhere owns the symbol (same one-symbol-per-
    // type rule as D2 for prototypes).
    if node.child_by_field_name("body").is_none() {
        return;
    }

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, SymbolKind::Class, &name, scope.qname);
    let class_qname = qualified(scope.qname, &name);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Class,
            file_path,
            start_line,
            end_line,
            node.start_byte() as u32,
            node.end_byte() as u32,
            scope.qname,
        )
        .with_parent(scope.id)
        .with_docstring(extract_doc_comment(node, source)),
    );

    if let Some(bases) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "base_class_clause")
    {
        extract_base_classes(bases, source, file_path, &sym_id, start_line, edges);
    }

    if let Some(body) = node.child_by_field_name("body") {
        // `class` defaults to private members, `struct`/`union` to public.
        let default_vis = if node.kind() == "class_specifier" {
            Visibility::Private
        } else {
            Visibility::Public
        };
        extract_class_body(
            body,
            source,
            file_path,
            ParentScope::nested(&sym_id, &class_qname),
            default_vis,
            symbols,
            edges,
        );
    }
}

/// `: public Base, private Mixin` → one Inherits edge per named base.
fn extract_base_classes(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    for child in node.named_children(&mut node.walk()) {
        // access_specifier / `virtual` are siblings of the base type names.
        if child.kind() == "access_specifier" {
            continue;
        }
        let target = simple_type_name(child, source);
        if !target.is_empty() {
            edges.push(Edge::new(
                sym_id,
                target,
                EdgeKind::Inherits,
                file_path,
                line,
            ));
        }
    }
}

/// Walk a `field_declaration_list`. `access_specifier` is a flat sibling of the
/// members it governs (not a wrapper), so visibility is tracked statefully.
fn extract_class_body(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    default_vis: Visibility,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    crate::parse::guard_recursion!();
    let mut vis = default_vis;

    for child in node.named_children(&mut node.walk()) {
        match child.kind() {
            "access_specifier" => {
                vis = match node_text(child, source).trim_end_matches(':').trim() {
                    "private" => Visibility::Private,
                    "protected" => Visibility::Protected,
                    _ => Visibility::Public,
                };
            }
            // An inline method definition inside the class body.
            "function_definition" => {
                extract_member_function(child, source, file_path, scope, vis, symbols, edges);
            }
            // `field_declaration` covers both data members and method declarations;
            // a `declaration` here is a ctor/dtor declaration. Both are bodiless
            // when they declare a callable, so they emit no symbol (D2) — only a
            // data member does. A nested type is also wrapped in a
            // `field_declaration` (as its `type`), so unwrap that first.
            "field_declaration" => {
                if let Some(nested) = nested_type_definition(child) {
                    if nested.kind() == "enum_specifier" {
                        extract_enum(nested, source, file_path, scope, symbols);
                    } else {
                        extract_class_like(nested, source, file_path, scope, symbols, edges);
                    }
                    continue;
                }
                extract_field_declaration(child, source, file_path, scope, vis, symbols, edges);
            }
            "class_specifier" | "struct_specifier" | "union_specifier" => {
                extract_class_like(child, source, file_path, scope, symbols, edges);
            }
            "enum_specifier" => extract_enum(child, source, file_path, scope, symbols),
            "alias_declaration" => extract_alias(child, source, file_path, scope, symbols, edges),
            "type_definition" => extract_typedef(child, source, file_path, scope, symbols, edges),
            // A template member wraps the real declaration one level down.
            "template_declaration" => {
                for inner in child.named_children(&mut child.walk()) {
                    match inner.kind() {
                        "function_definition" => extract_member_function(
                            inner, source, file_path, scope, vis, symbols, edges,
                        ),
                        "class_specifier" | "struct_specifier" | "union_specifier" => {
                            extract_class_like(inner, source, file_path, scope, symbols, edges)
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// A nested type declared inside a class body arrives wrapped in a
/// `field_declaration` whose `type` is the specifier (`class Outer { class Inner {}; }`).
/// Returns that specifier when it actually defines a type (i.e. it has a body),
/// so a plain `Inner member_;` data field is left alone.
fn nested_type_definition(node: Node) -> Option<Node> {
    let ty = node.child_by_field_name("type")?;
    let defines_type = matches!(
        ty.kind(),
        "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier"
    ) && ty.child_by_field_name("body").is_some();
    defines_type.then_some(ty)
}

/// An inline `function_definition` inside a class body → a Method.
fn extract_member_function(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    vis: Visibility,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let declarator = match find_function_declarator(node) {
        Some(d) => d,
        None => return,
    };
    let name_node = match declarator.child_by_field_name("declarator") {
        Some(n) => n,
        None => return,
    };
    let (_, name) = split_qualified_name(name_node, source);
    if name.is_empty() {
        return;
    }

    let before = symbols.len();
    emit_callable(
        node,
        source,
        file_path,
        Callable {
            name: &name,
            kind: SymbolKind::Method,
            declarator,
        },
        scope,
        symbols,
        edges,
    );
    // The access-specifier state, not a modifier keyword, decides member visibility.
    if let Some(sym) = symbols.get_mut(before) {
        sym.visibility = vis;
    }
}

/// A `field_declaration`: a data member becomes a Variable; a bodiless method
/// declaration emits no symbol (D2) but still contributes its type references.
fn extract_field_declaration(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    vis: Visibility,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    // A function-pointer DATA member (`int (*login)(Service*);`) also parses as a
    // `function_declarator`, but its own declarator is a parenthesized pointer —
    // it is a field, not a method declaration, so fall through and emit a
    // Variable. (`.h` routes here, so C vtable structs depend on this.)
    if let Some(declarator) = find_function_declarator(node) {
        let is_fn_pointer_field = declarator
            .child_by_field_name("declarator")
            .is_some_and(|d| d.kind() == "parenthesized_declarator");
        if !is_fn_pointer_field {
            // Method declaration: no symbol (D2), but its parameter/return types
            // are real references from the enclosing class.
            if let Some(id) = scope.id {
                collect_param_and_return_refs(node, declarator, source, file_path, id, edges);
            }
            return;
        }
    }

    let name_node = match node.child_by_field_name("declarator") {
        Some(n) => n,
        None => return,
    };
    let name = innermost_declarator_name(name_node, source);
    if name.is_empty() {
        return;
    }

    let line = node.start_position().row as u32 + 1;
    let mut sym = Symbol::new(
        name,
        SymbolKind::Variable,
        file_path,
        line,
        node.end_position().row as u32 + 1,
        node.start_byte() as u32,
        node.end_byte() as u32,
        scope.qname,
    )
    .with_parent(scope.id);
    if vis != Visibility::Public {
        sym = sym.with_visibility(vis);
    }
    symbols.push(sym);

    if let (Some(id), Some(ty)) = (scope.id, node.child_by_field_name("type")) {
        emit_type_ref(ty, source, file_path, id, line, edges);
    }
}

// Identical to `innermost_declarator_name` in c.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// Bare name of a possibly-wrapped declarator (`*p`, `a[3]`, `(*fp)(int)`).
fn innermost_declarator_name(node: Node, source: &str) -> String {
    crate::parse::guard_recursion!(String::new());
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => {
            node_text(node, source).to_string()
        }
        _ => match node.child_by_field_name("declarator") {
            Some(inner) => innermost_declarator_name(inner, source),
            None => node
                .named_children(&mut node.walk())
                .map(|c| innermost_declarator_name(c, source))
                .find(|s| !s.is_empty())
                .unwrap_or_default(),
        },
    }
}

// ── Enums ──

fn extract_enum(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source).to_string(),
        None => return,
    };

    let start_line = node.start_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, SymbolKind::Enum, &name, scope.qname);
    let enum_qname = qualified(scope.qname, &name);

    symbols.push(
        Symbol::new(
            name,
            SymbolKind::Enum,
            file_path,
            start_line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            scope.qname,
        )
        .with_parent(scope.id)
        .with_docstring(extract_doc_comment(node, source)),
    );

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    for member in body.named_children(&mut body.walk()) {
        if member.kind() != "enumerator" {
            continue;
        }
        let Some(mn) = member.child_by_field_name("name") else {
            continue;
        };
        let m_line = member.start_position().row as u32 + 1;
        symbols.push(
            Symbol::new(
                node_text(mn, source),
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

// ── Type aliases ──

/// `typedef int MyInt;` / `typedef struct Point Point;` → a TypeAlias. A
/// `typedef struct { .. } Name;` also defines the struct, so recurse into the
/// type to pick that up too.
fn extract_typedef(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    if let Some(ty) = node.child_by_field_name("type") {
        if matches!(
            ty.kind(),
            "struct_specifier" | "union_specifier" | "enum_specifier"
        ) && ty.child_by_field_name("body").is_some()
        {
            extract_node(ty, source, file_path, scope, symbols, edges);
        }
    }

    let Some(decl) = node.child_by_field_name("declarator") else {
        return;
    };
    let name = innermost_declarator_name(decl, source);
    if name.is_empty() {
        return;
    }
    emit_alias(node, source, file_path, &name, scope, symbols, edges);
}

/// `using Alias = std::string;` → a TypeAlias.
fn extract_alias(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(n) = node.child_by_field_name("name") else {
        return;
    };
    let name = node_text(n, source).to_string();
    if name.is_empty() {
        return;
    }
    emit_alias(node, source, file_path, &name, scope, symbols, edges);
}

fn emit_alias(
    node: Node,
    source: &str,
    file_path: &str,
    name: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    let sym_id = symbol_id(file_path, SymbolKind::TypeAlias, name, scope.qname);
    symbols.push(
        Symbol::new(
            name,
            SymbolKind::TypeAlias,
            file_path,
            line,
            node.end_position().row as u32 + 1,
            node.start_byte() as u32,
            node.end_byte() as u32,
            scope.qname,
        )
        .with_parent(scope.id)
        .with_docstring(extract_doc_comment(node, source)),
    );
    if let Some(ty) = node.child_by_field_name("type") {
        emit_type_ref(ty, source, file_path, &sym_id, line, edges);
    }
}

// ── Includes ──

// Identical to `extract_include` in c.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// `#include "local.h"` → an Import symbol + Imports edge. A `<system>` include
/// is stdlib noise, so it is skipped.
fn extract_include(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let Some(path_node) = node.child_by_field_name("path") else {
        return;
    };
    if path_node.kind() == "system_lib_string" {
        return;
    }
    let raw = node_text(path_node, source);
    let path = raw.trim_matches(|c| c == '"' || c == '<' || c == '>');
    if path.is_empty() {
        return;
    }

    let line = node.start_position().row as u32 + 1;
    // Target the header's basename: the resolver matches edges by symbol name,
    // and an include path is relative to the includer, not the repo root.
    let target = last_segment(path, "/");
    // Derive the edge source from symbol_id, never a hand-built string: it
    // escapes `.` inside a segment, so a literal format! would not match the
    // symbol row and the edge's foreign key would fail.
    let sym_id = symbol_id(file_path, SymbolKind::Import, path, scope.qname);

    symbols.push(Symbol::new(
        path,
        SymbolKind::Import,
        file_path,
        line,
        line,
        node.start_byte() as u32,
        node.end_byte() as u32,
        scope.qname,
    ));
    edges.push(Edge::new(
        sym_id,
        target,
        EdgeKind::Imports,
        file_path,
        line,
    ));
}

// ── Macros ──

// Identical to `extract_macro` in c.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// `#define MAX 10` / `#define SQ(x) ((x)*(x))` → a Macro symbol. A function-like
/// macro's parameter list becomes its signature; an object macro gets none.
fn extract_macro(
    node: Node,
    source: &str,
    file_path: &str,
    scope: ParentScope,
    symbols: &mut Vec<Symbol>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = node_text(name_node, source).to_string();
    if name.is_empty() {
        return;
    }

    let line = node.start_position().row as u32 + 1;
    let mut sym = Symbol::new(
        name,
        SymbolKind::Macro,
        file_path,
        line,
        node.end_position().row as u32 + 1,
        node.start_byte() as u32,
        node.end_byte() as u32,
        scope.qname,
    )
    .with_parent(scope.id)
    .with_docstring(extract_doc_comment(node, source));
    if let Some(params) = node.child_by_field_name("parameters") {
        sym = sym.with_signature(Some(node_text(params, source).to_string()));
    }
    symbols.push(sym);
}

// ── Calls ──

// Identical to `walk_for_calls` in c.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// Collect `Calls` edges from a function body, without descending into nested
/// definitions (a lambda body's calls still belong to the enclosing function).
fn walk_for_calls(node: Node, source: &str, file_path: &str, sym_id: &str, edges: &mut Vec<Edge>) {
    crate::parse::guard_recursion!();
    if node.kind() == "call_expression" {
        if let Some(f) = node.child_by_field_name("function") {
            if let Some(target) = call_target_name(f, source) {
                edges.push(Edge::new(
                    sym_id,
                    target,
                    EdgeKind::Calls,
                    file_path,
                    node.start_position().row as u32 + 1,
                ));
            }
        }
    }
    for child in node.named_children(&mut node.walk()) {
        walk_for_calls(child, source, file_path, sym_id, edges);
    }
}

/// Target name for a call. Three receiver shapes, matching the resolver's
/// name-based matching: bare `f()` → `f`; `obj.m()` / `ptr->m()` → `obj.m`;
/// `Klass::m()` → `Klass::m`.
fn call_target_name(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node_text(node, source).to_string()),
        "field_expression" => {
            let field = node.child_by_field_name("field")?;
            let recv = node
                .child_by_field_name("argument")
                .map(|a| receiver_name(a, source))
                .unwrap_or_default();
            let field = node_text(field, source);
            Some(if recv.is_empty() {
                field.to_string()
            } else {
                format!("{recv}.{field}")
            })
        }
        "qualified_identifier" => {
            let scope = node.child_by_field_name("scope")?;
            let name = node.child_by_field_name("name")?;
            Some(format!(
                "{}::{}",
                node_text(scope, source),
                call_target_name(name, source)?
            ))
        }
        // `(*fp)(x)`, `this->m()` wrappers and template calls (`f<T>()`).
        "parenthesized_expression" | "pointer_expression" | "template_function" => node
            .named_child(0)
            .and_then(|c| call_target_name(c, source)),
        _ => None,
    }
}

/// Receiver text for `obj.m()`. Only a simple identifier makes a useful prefix;
/// a complex expression receiver (`a[i].m()`) yields none so the edge falls back
/// to the bare field name.
fn receiver_name(node: Node, source: &str) -> String {
    match node.kind() {
        "identifier" | "field_identifier" | "this" => node_text(node, source).to_string(),
        _ => String::new(),
    }
}

// ── Type references ──

// Identical to `collect_param_and_return_refs` in c.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// Parameter and return types of a callable → `References` edges.
fn collect_param_and_return_refs(
    node: Node,
    declarator: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    edges: &mut Vec<Edge>,
) {
    let line = node.start_position().row as u32 + 1;
    if let Some(ty) = node.child_by_field_name("type") {
        emit_type_ref(ty, source, file_path, sym_id, line, edges);
    }
    let Some(params) = declarator.child_by_field_name("parameters") else {
        return;
    };
    for p in params.named_children(&mut params.walk()) {
        if let Some(ty) = p.child_by_field_name("type") {
            emit_type_ref(ty, source, file_path, sym_id, line, edges);
        }
    }
}

// Identical to `emit_type_ref` in c.rs — the two C-family extractors are
// deliberately independent (one file per language), so fix bugs in both.
/// Emit a `References` edge for a named (non-builtin) type.
fn emit_type_ref(
    node: Node,
    source: &str,
    file_path: &str,
    sym_id: &str,
    line: u32,
    edges: &mut Vec<Edge>,
) {
    let name = simple_type_name(node, source);
    if !name.is_empty() && !is_builtin_type(&name) {
        edges.push(Edge::new(
            sym_id,
            name,
            EdgeKind::References,
            file_path,
            line,
        ));
    }
}

/// Simple (unqualified) type name, unwrapping templates (`vector<T>` → `vector`),
/// qualified names (`std::string` → `string`) and pointer/reference/const nesting.
fn simple_type_name(node: Node, source: &str) -> String {
    crate::parse::guard_recursion!(String::new());
    match node.kind() {
        "type_identifier" | "identifier" | "namespace_identifier" => {
            node_text(node, source).to_string()
        }
        "primitive_type" | "sized_type_specifier" | "placeholder_type_specifier" => String::new(),
        "qualified_identifier" => node
            .child_by_field_name("name")
            .map(|n| simple_type_name(n, source))
            .unwrap_or_default(),
        "template_type" => node
            .child_by_field_name("name")
            .map(|n| simple_type_name(n, source))
            .unwrap_or_default(),
        "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => node
            .child_by_field_name("name")
            .map(|n| simple_type_name(n, source))
            .unwrap_or_default(),
        _ => node
            .named_children(&mut node.walk())
            .map(|c| simple_type_name(c, source))
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
    }
}

/// Keyword/stdlib type names that carry no graph value as an edge target.
/// Deliberately limited to language keywords and the ubiquitous `std` aliases —
/// user and framework types must stay referenceable.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "void"
            | "bool"
            | "char"
            | "char8_t"
            | "char16_t"
            | "char32_t"
            | "wchar_t"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "signed"
            | "unsigned"
            | "auto"
            | "size_t"
            | "ssize_t"
            | "ptrdiff_t"
            | "intptr_t"
            | "uintptr_t"
            | "int8_t"
            | "int16_t"
            | "int32_t"
            | "int64_t"
            | "uint8_t"
            | "uint16_t"
            | "uint32_t"
            | "uint64_t"
            | "nullptr_t"
            | "string"
            | "string_view"
    )
}

// ── Visibility (free functions) ──

/// A `static` free function is file-local; everything else is externally visible.
fn declared_visibility(node: Node, source: &str) -> Visibility {
    let is_static = node
        .named_children(&mut node.walk())
        .any(|c| c.kind() == "storage_class_specifier" && node_text(c, source) == "static");
    if is_static {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

// ── Doc comments ──

/// Preceding `///` or `/** */` comment block, nearest-first then reversed.
fn extract_doc_comment(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut p = node.prev_sibling();
    while let Some(n) = p {
        if n.kind() != "comment" {
            break;
        }
        let text = node_text(n, source).trim();
        if let Some(stripped) = text.strip_prefix("///") {
            let clean = stripped.trim();
            if !clean.is_empty() {
                lines.push(clean.to_string());
            }
        } else if text.starts_with("/**") {
            for raw in text
                .trim_start_matches("/**")
                .trim_end_matches("*/")
                .lines()
            {
                let clean = raw.trim().trim_start_matches('*').trim();
                if !clean.is_empty() {
                    lines.push(clean.to_string());
                }
            }
        } else {
            // A plain `//` comment is not documentation; keep scanning past it
            // rather than truncating a `///` block that sits above it.
            p = n.prev_sibling();
            continue;
        }
        p = n.prev_sibling();
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
        let mut ext = CppExtractor::new();
        ext.extract(source, "test.cpp").unwrap()
    }

    fn extract_as(source: &str, path: &str) -> ExtractionResult {
        let mut ext = CppExtractor::new();
        ext.extract(source, path).unwrap()
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
        let result = extract("class Broken {");
        let _ = result.symbols.len();
    }

    #[test]
    fn test_free_function() {
        let result = extract("int add(int a, int b) { return a + b; }");
        let sym = result.symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(sym.kind, SymbolKind::Function);
        assert_eq!(sym.visibility, Visibility::Public);
    }

    #[test]
    fn test_static_function_is_private() {
        let result = extract("static int helper() { return 0; }");
        let sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(sym.visibility, Visibility::Private);
    }

    #[test]
    fn test_class() {
        let result = extract("class AuthService { };");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "AuthService")
            .unwrap();
        assert_eq!(sym.kind, SymbolKind::Class);
    }

    #[test]
    fn test_struct_maps_to_class() {
        let result = extract("struct Point { int x; int y; };");
        let sym = result.symbols.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(sym.kind, SymbolKind::Class);
    }

    #[test]
    fn test_union_maps_to_class() {
        let result = extract("union Value { int i; float f; };");
        let sym = result.symbols.iter().find(|s| s.name == "Value").unwrap();
        assert_eq!(sym.kind, SymbolKind::Class);
    }

    #[test]
    fn test_enum_with_members() {
        let result = extract("enum class TokenError { Expired, Invalid };");
        let e = result
            .symbols
            .iter()
            .find(|s| s.name == "TokenError")
            .unwrap();
        assert_eq!(e.kind, SymbolKind::Enum);
        let m = result.symbols.iter().find(|s| s.name == "Expired").unwrap();
        assert_eq!(m.kind, SymbolKind::EnumMember);
        assert!(m.id.ends_with("TokenError.Expired"));
    }

    #[test]
    fn test_typedef_is_type_alias() {
        let result = extract("typedef int MyInt;");
        let sym = result.symbols.iter().find(|s| s.name == "MyInt").unwrap();
        assert_eq!(sym.kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn test_using_alias_is_type_alias() {
        let result = extract("using Alias = std::string;");
        let sym = result.symbols.iter().find(|s| s.name == "Alias").unwrap();
        assert_eq!(sym.kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn test_inline_method_is_method() {
        let result = extract("class A { public: int m() { return 1; } };");
        let sym = result.symbols.iter().find(|s| s.name == "m").unwrap();
        assert_eq!(sym.kind, SymbolKind::Method);
        assert!(sym.id.ends_with("A.m"));
    }

    // Regression: a function-pointer data member parses as a `function_declarator`
    // and was skipped as if it were a method declaration. `.h` routes here, so C
    // vtable structs in headers were losing every member.
    #[test]
    fn test_function_pointer_member_is_variable() {
        let result = extract_as(
            "struct Service { int (*login)(struct Service*, const char*); };",
            "service.h",
        );
        let sym = result.symbols.iter().find(|s| s.name == "login").unwrap();
        assert_eq!(sym.kind, SymbolKind::Variable);
        assert!(sym.id.ends_with("Service.login"));
    }

    #[test]
    fn test_data_member_is_variable() {
        let result = extract("class A { private: int count_; };");
        let sym = result.symbols.iter().find(|s| s.name == "count_").unwrap();
        assert_eq!(sym.kind, SymbolKind::Variable);
        assert_eq!(sym.visibility, Visibility::Private);
    }

    #[test]
    fn test_class_members_default_to_private() {
        let result = extract("class A { int hidden; public: int shown; };");
        let hidden = result.symbols.iter().find(|s| s.name == "hidden").unwrap();
        let shown = result.symbols.iter().find(|s| s.name == "shown").unwrap();
        assert_eq!(hidden.visibility, Visibility::Private);
        assert_eq!(shown.visibility, Visibility::Public);
    }

    #[test]
    fn test_struct_members_default_to_public() {
        let result = extract("struct A { int shown; };");
        let sym = result.symbols.iter().find(|s| s.name == "shown").unwrap();
        assert_eq!(sym.visibility, Visibility::Public);
    }

    #[test]
    fn test_protected_access_specifier() {
        let result = extract("class A { protected: int mid; };");
        let sym = result.symbols.iter().find(|s| s.name == "mid").unwrap();
        assert_eq!(sym.visibility, Visibility::Protected);
    }

    // D2: a bodiless prototype must NOT emit a symbol, or two same-kind Function
    // candidates would make the resolver's UniqueGlobal tier ambiguous.
    #[test]
    fn test_prototype_emits_no_symbol() {
        let result = extract("int add(int a, int b);");
        assert!(!result.symbols.iter().any(|s| s.name == "add"));
    }

    #[test]
    fn test_definition_emits_symbol_where_prototype_does_not() {
        let result = extract("int add(int a, int b);\nint add(int a, int b) { return a + b; }");
        let defs: Vec<_> = result.symbols.iter().filter(|s| s.name == "add").collect();
        assert_eq!(defs.len(), 1, "exactly one symbol per function");
        assert_eq!(defs[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_in_class_method_declaration_emits_no_symbol() {
        let result = extract("class A { public: int declared_only(); };");
        assert!(!result.symbols.iter().any(|s| s.name == "declared_only"));
    }

    // D2 seam: an out-of-line definition attaches to its class as a Method.
    #[test]
    fn test_out_of_line_definition_is_method_on_class() {
        let result = extract("int SqlRepo::find(int id) { return id; }");
        let sym = result.symbols.iter().find(|s| s.name == "find").unwrap();
        assert_eq!(sym.kind, SymbolKind::Method);
        assert!(sym.id.ends_with("SqlRepo.find"));
    }

    #[test]
    fn test_out_of_line_namespaced_attaches_to_innermost_class() {
        let result = extract("int app::SqlRepo::find(int i) { return i; }");
        let sym = result.symbols.iter().find(|s| s.name == "find").unwrap();
        assert_eq!(sym.kind, SymbolKind::Method);
        assert!(sym.id.ends_with("SqlRepo.find"));
    }

    #[test]
    fn test_out_of_line_destructor() {
        let result = extract("A::~A() { }");
        assert!(result.symbols.iter().any(|s| s.name == "~A"));
    }

    // Regression: a prototype's return/parameter type specifiers are type USES,
    // not definitions. Descending into `declaration` emitted a phantom class for
    // every forward-declared or pointed-to type in a header.
    #[test]
    fn test_prototype_type_specifiers_emit_no_class_symbols() {
        let result = extract("struct Ret make(struct Param *p);");
        assert!(!result.symbols.iter().any(|s| s.name == "Ret"));
        assert!(!result.symbols.iter().any(|s| s.name == "Param"));
    }

    #[test]
    fn test_forward_declaration_emits_no_symbol() {
        let result = extract("class Fwd;\nclass Real { };");
        assert!(!result.symbols.iter().any(|s| s.name == "Fwd"));
        assert!(result.symbols.iter().any(|s| s.name == "Real"));
    }

    #[test]
    fn test_inheritance_edge() {
        let result = extract("class AuthService : public BaseService { };");
        assert!(edge_targets(&result, EdgeKind::Inherits).contains(&"BaseService".to_string()));
    }

    #[test]
    fn test_multiple_inheritance_edges() {
        let result = extract("class A : public B, private C { };");
        let inherits = edge_targets(&result, EdgeKind::Inherits);
        assert!(inherits.contains(&"B".to_string()));
        assert!(inherits.contains(&"C".to_string()));
    }

    #[test]
    fn test_bare_call_edge() {
        let result = extract("void f() { authenticate(); }");
        assert!(edge_targets(&result, EdgeKind::Calls).contains(&"authenticate".to_string()));
    }

    #[test]
    fn test_member_call_edge_dot() {
        let result = extract("void f() { repo.execute_query(); }");
        assert!(edge_targets(&result, EdgeKind::Calls).contains(&"repo.execute_query".to_string()));
    }

    #[test]
    fn test_member_call_edge_arrow() {
        let result = extract("void f() { repo->execute_query(); }");
        assert!(edge_targets(&result, EdgeKind::Calls).contains(&"repo.execute_query".to_string()));
    }

    #[test]
    fn test_qualified_call_edge() {
        let result = extract("void f() { Logger::get_logger(); }");
        assert!(edge_targets(&result, EdgeKind::Calls).contains(&"Logger::get_logger".to_string()));
    }

    #[test]
    fn test_local_include_emits_import_and_edge() {
        let result = extract("#include \"auth/service.h\"\n");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .unwrap();
        assert_eq!(sym.name, "auth/service.h");
        assert!(edge_targets(&result, EdgeKind::Imports).contains(&"service.h".to_string()));
    }

    // Regression: the Imports edge's source_id must be the Import symbol's real
    // `symbol_id` (which escapes `.` inside a segment), not a hand-built
    // "{file}:import:{path}" string — a mismatch makes the DB insert fail with
    // FOREIGN KEY constraint failed on any file with a local include.
    #[test]
    fn test_include_edge_source_matches_import_symbol_id() {
        let result = extract("#include \"auth/service.h\"\n");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .unwrap();
        let edge = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert_eq!(edge.source_id, sym.id);
    }

    #[test]
    fn test_system_include_is_skipped() {
        let result = extract("#include <string>\n");
        assert!(!result.symbols.iter().any(|s| s.kind == SymbolKind::Import));
        assert!(edge_targets(&result, EdgeKind::Imports).is_empty());
    }

    #[test]
    fn test_namespace_folds_into_qname_without_symbol() {
        let result = extract("namespace app { class Foo { }; }");
        assert!(!result.symbols.iter().any(|s| s.name == "app"));
        let foo = result.symbols.iter().find(|s| s.name == "Foo").unwrap();
        assert!(foo.id.ends_with("app.Foo"));
    }

    #[test]
    fn test_nested_namespace_qname() {
        let result = extract("namespace app { namespace db { class Conn { }; } }");
        let conn = result.symbols.iter().find(|s| s.name == "Conn").unwrap();
        assert!(conn.id.ends_with("app.db.Conn"));
    }

    #[test]
    fn test_nested_class_qname() {
        let result = extract("class Outer { public: class Inner { }; };");
        let inner = result.symbols.iter().find(|s| s.name == "Inner").unwrap();
        assert!(inner.id.ends_with("Outer.Inner"));
    }

    // A nested type is wrapped in a `field_declaration` (as its `type`), not a
    // direct child of the body — regression for silently dropping nested types.
    #[test]
    fn test_nested_enum_in_class_is_extracted() {
        let result = extract("class A { public: enum Mode { Fast, Slow }; };");
        let e = result.symbols.iter().find(|s| s.name == "Mode").unwrap();
        assert_eq!(e.kind, SymbolKind::Enum);
        assert!(e.id.ends_with("A.Mode"));
        assert!(result.symbols.iter().any(|s| s.name == "Fast"));
    }

    #[test]
    fn test_nested_struct_in_class_is_extracted() {
        let result = extract("class A { public: struct Row { int id; }; };");
        let row = result.symbols.iter().find(|s| s.name == "Row").unwrap();
        assert_eq!(row.kind, SymbolKind::Class);
        assert!(row.id.ends_with("A.Row"));
    }

    // A plain data member whose type is a class must stay a Variable, not be
    // mistaken for a nested type definition (it has no body).
    #[test]
    fn test_member_of_class_type_is_still_a_variable() {
        let result = extract("class A { private: Logger logger_; };");
        let sym = result.symbols.iter().find(|s| s.name == "logger_").unwrap();
        assert_eq!(sym.kind, SymbolKind::Variable);
    }

    #[test]
    fn test_line_doc_comment() {
        let result = extract("/// Validates a token.\nint validate_token() { return 1; }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "validate_token")
            .unwrap();
        assert_eq!(sym.docstring.as_deref(), Some("Validates a token."));
    }

    #[test]
    fn test_block_doc_comment() {
        let result = extract("/** Runs a query. */\nint execute_query() { return 1; }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "execute_query")
            .unwrap();
        assert_eq!(sym.docstring.as_deref(), Some("Runs a query."));
    }

    #[test]
    fn test_template_function_is_extracted() {
        let result = extract("template<typename T> T identity(T x) { return x; }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "identity")
            .unwrap();
        assert_eq!(sym.kind, SymbolKind::Function);
    }

    #[test]
    fn test_template_class_is_extracted() {
        let result =
            extract("template<typename T> class Box { public: T get() { return v; } T v; };");
        let cls = result.symbols.iter().find(|s| s.name == "Box").unwrap();
        assert_eq!(cls.kind, SymbolKind::Class);
        assert!(result.symbols.iter().any(|s| s.name == "get"));
    }

    #[test]
    fn test_parameter_type_reference_edge() {
        let result = extract("void f(DatabaseConnection conn) { }");
        assert!(
            edge_targets(&result, EdgeKind::References).contains(&"DatabaseConnection".to_string())
        );
    }

    #[test]
    fn test_builtin_parameter_types_are_not_referenced() {
        let result = extract("void f(int a, double b, char* c) { }");
        assert!(edge_targets(&result, EdgeKind::References).is_empty());
    }

    #[test]
    fn test_signature_includes_return_and_params() {
        let result = extract("int add(int a, int b) { return a + b; }");
        let sym = result.symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(sym.signature.as_deref(), Some("int (int a, int b)"));
    }

    // `.h` also covers Objective-C headers, which cartog does not support. The
    // contract is that they degrade to NOTHING rather than emitting garbage
    // symbols — an ObjC `@interface` matches no node kind this walker handles.
    #[test]
    fn test_objective_c_header_yields_no_symbols() {
        let result = extract_as(
            "#import <Foundation/Foundation.h>\n\n@interface Foo : NSObject\n@property (nonatomic, strong) NSString *name;\n- (void)doThing;\n+ (instancetype)fooWithName:(NSString *)name;\n@end\n",
            "Foo.h",
        );
        assert!(
            result.symbols.is_empty(),
            "ObjC header must yield no symbols, got: {:?}",
            result.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    // D5 seam: `.h` routes here (the C++ grammar is a superset), and plain-C
    // constructs in a header must still extract.
    #[test]
    fn test_c_header_constructs_extract_via_cpp_extractor() {
        let result = extract_as(
            "#ifndef H\n#define H\nstruct Point { int x; };\ntypedef struct Point Point;\nint add(int a, int b);\n#endif\n",
            "point.h",
        );
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Point" && s.kind == SymbolKind::Class));
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Point" && s.kind == SymbolKind::TypeAlias));
        // The prototype still emits no symbol (D2) even in a header.
        assert!(!result.symbols.iter().any(|s| s.name == "add"));
    }

    #[test]
    fn test_calls_inside_class_method_attach_to_method() {
        let result = extract("class A { public: int login() { return authenticate(); } };");
        let method = result.symbols.iter().find(|s| s.name == "login").unwrap();
        let call = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "authenticate")
            .unwrap();
        assert_eq!(call.source_id, method.id);
    }

    #[test]
    fn test_out_of_line_method_body_calls_attach_to_method() {
        let result = extract("int AuthService::login() { return validate_token(); }");
        let method = result.symbols.iter().find(|s| s.name == "login").unwrap();
        let call = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "validate_token")
            .unwrap();
        assert_eq!(call.source_id, method.id);
    }

    #[test]
    fn test_typedef_anonymous_struct_emits_both() {
        let result = extract("typedef struct Point { int x; } PointAlias;");
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Point" && s.kind == SymbolKind::Class));
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "PointAlias" && s.kind == SymbolKind::TypeAlias));
    }

    #[test]
    fn test_constructor_definition_out_of_line() {
        let result = extract("AuthService::AuthService() { init(); }");
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "AuthService" && s.kind == SymbolKind::Method)
            .unwrap();
        assert!(sym.id.ends_with("AuthService.AuthService"));
    }

    #[test]
    fn test_object_macro_is_extracted() {
        let result = extract("#define MAX 10\n");
        let sym = result.symbols.iter().find(|s| s.name == "MAX").unwrap();
        assert_eq!(sym.kind, SymbolKind::Macro);
    }

    #[test]
    fn test_function_like_macro_is_extracted() {
        let result = extract("#define SQ(x) ((x)*(x))\n");
        let sym = result.symbols.iter().find(|s| s.name == "SQ").unwrap();
        assert_eq!(sym.kind, SymbolKind::Macro);
        assert_eq!(sym.signature.as_deref(), Some("(x)"));
    }

    #[test]
    fn test_enumerator_is_enum_member() {
        let result = extract("enum TokenError { EXPIRED, INVALID };");
        let m = result.symbols.iter().find(|s| s.name == "EXPIRED").unwrap();
        assert_eq!(m.kind, SymbolKind::EnumMember);
    }

    #[test]
    fn test_enum_class_enumerator_is_enum_member() {
        let result = extract("enum class Status { Active, Inactive };");
        let m = result.symbols.iter().find(|s| s.name == "Active").unwrap();
        assert_eq!(m.kind, SymbolKind::EnumMember);
    }

    #[test]
    fn test_struct_field_is_still_variable() {
        let result = extract("struct Point { int x; };");
        let sym = result.symbols.iter().find(|s| s.name == "x").unwrap();
        assert_eq!(sym.kind, SymbolKind::Variable);
    }
}
