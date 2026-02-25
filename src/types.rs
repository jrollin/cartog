use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
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
}

impl Symbol {
    /// Create a new symbol, computing the ID from `file_path:name:start_line`.
    ///
    /// Optional fields (`signature`, `docstring`, `parent_id`) default to `None`,
    /// `visibility` defaults to `Public`, and `is_async` defaults to `false`.
    /// Use the builder-style setters to override.
    pub fn new(
        name: impl Into<String>,
        kind: SymbolKind,
        file_path: &str,
        start_line: u32,
        end_line: u32,
        start_byte: u32,
        end_byte: u32,
    ) -> Self {
        let name = name.into();
        let id = symbol_id(file_path, &name, start_line);
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
        }
    }

    /// Set the parent symbol ID.
    pub fn with_parent(mut self, parent_id: Option<&str>) -> Self {
        self.parent_id = parent_id.map(str::to_string);
        self
    }

    /// Set the function/method signature.
    pub fn with_signature(mut self, signature: Option<String>) -> Self {
        self.signature = signature;
        self
    }

    /// Set the visibility.
    pub fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Mark as async.
    pub fn with_async(mut self, is_async: bool) -> Self {
        self.is_async = is_async;
        self
    }

    /// Set the docstring.
    pub fn with_docstring(mut self, docstring: Option<String>) -> Self {
        self.docstring = docstring;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Variable,
    Import,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Method => "method",
            Self::Variable => "variable",
            Self::Import => "import",
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
            _ => Err(anyhow::anyhow!("unknown symbol kind: '{s}'")),
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Protected,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Protected => "protected",
        }
    }

    /// Parse a visibility string, defaulting to `Public` for unknown values.
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Edge {
    pub source_id: String,
    pub target_name: String,
    pub target_id: Option<String>,
    pub kind: EdgeKind,
    pub file_path: String,
    pub line: u32,
}

impl Edge {
    /// Create a new edge with `target_id` set to `None` (resolved later by `db.resolve_edges()`).
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Calls,
    Imports,
    Inherits,
    References,
    Raises,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Inherits => "inherits",
            Self::References => "references",
            Self::Raises => "raises",
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

/// Build a symbol ID from its components: `file_path:name:line`
pub fn symbol_id(file_path: &str, name: &str, line: u32) -> String {
    format!("{file_path}:{name}:{line}")
}
