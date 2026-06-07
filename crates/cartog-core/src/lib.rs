//! Core types and utilities for the cartog code graph indexer.
//!
//! Defines the shared data model ([`Symbol`], [`Edge`], [`SymbolKind`], [`EdgeKind`])
//! used by all other cartog crates. Symbol IDs are deterministic and invariant to
//! line movements — built from `file_path:kind:qualified_name`.
//!
//! Also provides [`detect_language`] for mapping file extensions to language names
//! without pulling in tree-sitter grammar dependencies.
#![doc = ""]
#![doc = include_str!("../README.md")]

use std::borrow::Cow;
use std::path::Path;

use serde::Serialize;

mod provenance;
pub use provenance::EdgeProvenance;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Symbol {
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub parent_id: Option<String>,
    pub signature: Option<String>,
    pub visibility: Visibility,
    pub is_async: bool,
    pub docstring: Option<String>,
    pub in_degree: u32,
    pub content_hash: Option<String>,
    pub subtree_hash: Option<String>,
}

impl Symbol {
    /// Create a new symbol with a stable ID: `file_path:kind:qualified_name`.
    ///
    /// `parent_name` is the unqualified name chain of the parent symbol (e.g. `"Outer.Inner"`).
    /// It is used to build the stable ID and also stored as `parent_id` (the parent's full ID).
    ///
    /// Optional fields (`signature`, `docstring`) default to `None`,
    /// `visibility` defaults to `Public`, and `is_async` defaults to `false`.
    /// Use the builder-style setters to override.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        kind: SymbolKind,
        file_path: &str,
        start_line: u32,
        end_line: u32,
        start_byte: u32,
        end_byte: u32,
        parent_name: Option<&str>,
    ) -> Self {
        let name = name.into();
        let id = symbol_id(file_path, kind, &name, parent_name);
        Self {
            id,
            name,
            kind,
            file_path: file_path.to_string(),
            start_line,
            end_line,
            start_byte,
            end_byte,
            parent_id: None,
            signature: None,
            visibility: Visibility::Public,
            is_async: false,
            docstring: None,
            in_degree: 0,
            content_hash: None,
            subtree_hash: None,
        }
    }

    /// Set the parent symbol ID.
    #[must_use]
    pub fn with_parent(mut self, parent_id: Option<&str>) -> Self {
        self.parent_id = parent_id.map(str::to_string);
        self
    }

    /// Set the function/method signature.
    #[must_use]
    pub fn with_signature(mut self, signature: Option<String>) -> Self {
        self.signature = signature;
        self
    }

    /// Set the visibility.
    #[must_use]
    pub fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Mark as async.
    #[must_use]
    pub fn with_async(mut self, is_async: bool) -> Self {
        self.is_async = is_async;
        self
    }

    /// Set the docstring.
    #[must_use]
    pub fn with_docstring(mut self, docstring: Option<String>) -> Self {
        self.docstring = docstring;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Variable,
    Import,
    Interface,
    Enum,
    TypeAlias,
    Trait,
    Module,
    Document,
}

impl SymbolKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Method => "method",
            Self::Variable => "variable",
            Self::Import => "import",
            Self::Interface => "interface",
            Self::Enum => "enum",
            Self::TypeAlias => "type_alias",
            Self::Trait => "trait",
            Self::Module => "module",
            Self::Document => "document",
        }
    }
}

impl std::str::FromStr for SymbolKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "function" => Ok(Self::Function),
            "class" => Ok(Self::Class),
            "method" => Ok(Self::Method),
            "variable" => Ok(Self::Variable),
            "import" => Ok(Self::Import),
            "interface" => Ok(Self::Interface),
            "enum" => Ok(Self::Enum),
            "type_alias" => Ok(Self::TypeAlias),
            "trait" => Ok(Self::Trait),
            "module" => Ok(Self::Module),
            "document" => Ok(Self::Document),
            _ => Err(anyhow::anyhow!("unknown symbol kind: '{s}'")),
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Protected,
}

impl Visibility {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Protected => "protected",
        }
    }

    /// Parse a visibility string, defaulting to `Public` for unknown values.
    #[must_use]
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "private" => Self::Private,
            "protected" => Self::Protected,
            _ => Self::Public,
        }
    }
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Edge {
    pub source_id: String,
    pub target_name: String,
    pub target_id: Option<String>,
    pub kind: EdgeKind,
    pub file_path: String,
    pub line: u32,
    /// Which tier/source resolved `target_id`. `None` at extraction and for edges
    /// resolved before provenance tracking existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<EdgeProvenance>,
}

impl Edge {
    /// Create a new edge with `target_id` set to `None` (resolved later by `db.resolve_edges()`).
    #[must_use]
    pub fn new(
        source_id: impl Into<String>,
        target_name: impl Into<String>,
        kind: EdgeKind,
        file_path: &str,
        line: u32,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            target_name: target_name.into(),
            target_id: None,
            kind,
            file_path: file_path.to_string(),
            line,
            provenance: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Calls,
    Imports,
    Inherits,
    References,
    Raises,
    Implements,
    TypeOf,
}

impl EdgeKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Inherits => "inherits",
            Self::References => "references",
            Self::Raises => "raises",
            Self::Implements => "implements",
            Self::TypeOf => "type_of",
        }
    }
}

impl std::str::FromStr for EdgeKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "calls" => Ok(Self::Calls),
            "imports" => Ok(Self::Imports),
            "inherits" => Ok(Self::Inherits),
            "references" => Ok(Self::References),
            "raises" => Ok(Self::Raises),
            "implements" => Ok(Self::Implements),
            "type_of" => Ok(Self::TypeOf),
            _ => Err(anyhow::anyhow!("unknown edge kind: '{s}'")),
        }
    }
}

impl std::fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileInfo {
    pub path: String,
    pub last_modified: f64,
    pub hash: String,
    pub language: String,
    pub num_symbols: u32,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ChangesResult {
    pub changed_files: Vec<String>,
    pub symbols: Vec<Symbol>,
}

/// Percent-escape the ID separators (`%` first, then `.`/`:`) in a leaf-name
/// segment so a name containing `.`/`:` can't be confused with a structural `.`
/// join or a boundary `:`. Identity for any string free of `% . :` (every
/// ordinary identifier), so common-case IDs stay byte-for-byte unchanged; only
/// composite leaves (dotted imports, `.`/`:`-bearing markdown headings) differ.
fn escape_segment(s: &str) -> Cow<'_, str> {
    if !s.contains(['%', '.', ':']) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            '.' => out.push_str("%2E"),
            ':' => out.push_str("%3A"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

/// Build a stable symbol ID: `file_path:kind:qualified_name`
///
/// The qualified name encodes the parent chain using `.` separators:
/// - Top-level function: `src/auth.py:function:validate`
/// - Method in class:    `src/auth.py:method:TokenService.validate`
/// - Nested class:       `src/auth.py:class:Outer.Inner`
///
/// This ID is stable across line movements within a file. `kind` is typed so a
/// mistyped kind fails to compile rather than silently mis-forming the ID.
///
/// The leaf `name` is separator-escaped so distinct logical symbols never share
/// an ID (a collision is a silent DB overwrite). `parent_name` is emitted raw:
/// its `.` separators are structural (built by the extractors from already-bare
/// segments), so it carries no injected separators.
#[must_use]
pub fn symbol_id(
    file_path: &str,
    kind: SymbolKind,
    name: &str,
    parent_name: Option<&str>,
) -> String {
    let kind = kind.as_str();
    let name = escape_segment(name);
    match parent_name {
        Some(pn) => format!("{file_path}:{kind}:{pn}.{name}"),
        None => format!("{file_path}:{kind}:{name}"),
    }
}

/// Map file extension to language name.
#[must_use]
pub fn detect_language(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "py" | "pyi" => Some("python"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "rs" => Some("rust"),
        "go" => Some("go"),
        "rb" => Some("ruby"),
        "java" => Some("java"),
        "php" => Some("php"),
        "dart" => Some("dart"),
        "swift" => Some("swift"),
        "kt" | "kts" => Some("kotlin"),
        "md" => Some("markdown"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_top_level() {
        assert_eq!(
            symbol_id("src/auth.py", SymbolKind::Function, "validate", None),
            "src/auth.py:function:validate"
        );
    }

    #[test]
    fn stable_id_with_parent() {
        assert_eq!(
            symbol_id(
                "src/auth.py",
                SymbolKind::Method,
                "validate",
                Some("TokenService")
            ),
            "src/auth.py:method:TokenService.validate"
        );
    }

    #[test]
    fn stable_id_nested_parent() {
        assert_eq!(
            symbol_id(
                "src/auth.py",
                SymbolKind::Method,
                "do_work",
                Some("Outer.Inner")
            ),
            "src/auth.py:method:Outer.Inner.do_work"
        );
    }

    #[test]
    fn stable_id_invariant_to_line_changes() {
        let sym_at_line_10 = Symbol::new(
            "validate",
            SymbolKind::Function,
            "src/auth.py",
            10,
            20,
            100,
            500,
            None,
        );
        let sym_at_line_50 = Symbol::new(
            "validate",
            SymbolKind::Function,
            "src/auth.py",
            50,
            60,
            800,
            1200,
            None,
        );
        assert_eq!(sym_at_line_10.id, sym_at_line_50.id);
    }

    #[test]
    fn stable_id_differs_by_kind() {
        let func_id = symbol_id("f.py", SymbolKind::Function, "foo", None);
        let var_id = symbol_id("f.py", SymbolKind::Variable, "foo", None);
        assert_ne!(func_id, var_id);
    }

    #[test]
    fn symbol_id_matches_symbol_new_id_for_the_same_kind() {
        // Extractor-built edge-source ids must equal the symbol's own .id.
        let sym = Symbol::new(
            "validate",
            SymbolKind::Method,
            "src/auth.py",
            10,
            20,
            100,
            500,
            Some("TokenService"),
        );
        assert_eq!(
            sym.id,
            symbol_id(
                "src/auth.py",
                SymbolKind::Method,
                "validate",
                Some("TokenService")
            )
        );
    }

    use proptest::prelude::*;

    /// Strategy over every [`SymbolKind`] variant.
    fn any_kind() -> impl Strategy<Value = SymbolKind> {
        prop_oneof![
            Just(SymbolKind::Function),
            Just(SymbolKind::Class),
            Just(SymbolKind::Method),
            Just(SymbolKind::Variable),
            Just(SymbolKind::Import),
            Just(SymbolKind::Interface),
            Just(SymbolKind::Enum),
            Just(SymbolKind::TypeAlias),
            Just(SymbolKind::Trait),
            Just(SymbolKind::Module),
            Just(SymbolKind::Document),
        ]
    }

    /// Component string: mix of ordinary identifier chars and the `:`/`.`
    /// separators so the generator probes separator-injection collisions.
    fn component() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9_.:/]{0,8}").unwrap()
    }

    proptest! {
        /// Same logical tuple always yields the same ID.
        #[test]
        fn symbol_id_is_deterministic(
            file in component(),
            kind in any_kind(),
            name in component(),
            parent in proptest::option::of(component()),
        ) {
            let a = symbol_id(&file, kind, &name, parent.as_deref());
            let b = symbol_id(&file, kind, &name, parent.as_deref());
            prop_assert_eq!(a, b);
        }

        /// Distinct logical tuples must yield distinct IDs (a collision is a
        /// silent DB overwrite). Drives the realistic vector directly: method
        /// `name` of `parent` vs a top-level symbol named `parent.name`.
        #[test]
        fn symbol_id_is_injective(
            file in component(),
            kind in any_kind(),
            parent in "[A-Za-z0-9_]{1,6}",
            name in "[A-Za-z0-9_]{1,6}",
        ) {
            let id_a = symbol_id(&file, kind, &name, Some(&parent));
            let merged = format!("{parent}.{name}");
            let id_b = symbol_id(&file, kind, &merged, None);
            prop_assert_ne!(id_a, id_b);
        }
    }

    #[test]
    fn symbol_id_separates_dotted_leaf_from_parent_join() {
        // Regression for the proptest-found collision (silent DB overwrite).
        let method = symbol_id("f", SymbolKind::Method, "a", Some("0"));
        let dotted = symbol_id("f", SymbolKind::Method, "0.a", None);
        assert_eq!(method, "f:method:0.a");
        assert_eq!(dotted, "f:method:0%2Ea");
        assert_ne!(method, dotted);
    }

    #[test]
    fn symbol_id_is_identity_for_bare_identifiers() {
        // Ordinary names stay byte-identical (existing DBs + extractor assertions).
        assert_eq!(
            symbol_id(
                "src/auth.py",
                SymbolKind::Method,
                "validate",
                Some("Outer.Inner")
            ),
            "src/auth.py:method:Outer.Inner.validate"
        );
    }

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language(Path::new("src/main.py")), Some("python"));
        assert_eq!(detect_language(Path::new("lib.pyi")), Some("python"));
        assert_eq!(detect_language(Path::new("app.ts")), Some("typescript"));
        assert_eq!(detect_language(Path::new("App.tsx")), Some("tsx"));
        assert_eq!(detect_language(Path::new("index.js")), Some("javascript"));
        assert_eq!(detect_language(Path::new("util.mjs")), Some("javascript"));
        assert_eq!(detect_language(Path::new("main.rs")), Some("rust"));
        assert_eq!(detect_language(Path::new("server.go")), Some("go"));
        assert_eq!(detect_language(Path::new("app.rb")), Some("ruby"));
        assert_eq!(detect_language(Path::new("README.md")), Some("markdown"));
        assert_eq!(detect_language(Path::new("Makefile")), None);
        assert_eq!(detect_language(Path::new("Main.java")), Some("java"));
        assert_eq!(detect_language(Path::new("Service.php")), Some("php"));
        assert_eq!(detect_language(Path::new("main.dart")), Some("dart"));
        assert_eq!(detect_language(Path::new("App.swift")), Some("swift"));
        assert_eq!(detect_language(Path::new("Main.kt")), Some("kotlin"));
        assert_eq!(
            detect_language(Path::new("build.gradle.kts")),
            Some("kotlin")
        );
    }
}
