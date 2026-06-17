# cartog-core

Core types and utilities for the cartog code graph indexer.

## Overview

Foundation crate with no internal cartog dependencies. Defines the shared data model used by all other cartog crates: symbols, edges, and their metadata. Also provides pure utility functions like language detection.

## How it works

### Stable symbol IDs

Every symbol gets a deterministic ID built from its location in the code structure, not its position in the file:

```text
file_path:kind:qualified_name
```

The qualified name encodes the parent chain with `.` separators:

- Top-level function: `src/auth.py:function:validate`
- Method in class: `src/auth.py:method:TokenService.validate`
- Nested class: `src/auth.py:class:Outer.Inner`

This ID is **invariant to line movements** within a file — renaming or moving lines won't change it, only renaming the symbol or its parent chain will.

### Builder pattern

`Symbol::new()` returns a symbol with sensible defaults (`Public` visibility, not async, no docstring). Builder-style setters (`with_parent`, `with_signature`, `with_visibility`, `with_async`, `with_docstring`) allow incremental customization without a 15-argument constructor.

### Language detection

`detect_language()` maps file extensions to language names (e.g., `.py` → `"python"`, `.tsx` → `"tsx"`, `.md` → `"markdown"`). It's a pure function with no tree-sitter dependency, which allows lightweight crates like `cartog-watch` and `cartog-lsp` to filter files without pulling in grammar crates.

## Public API

| Export | Description |
|--------|-------------|
| `Symbol` | Code symbol with stable ID, kind, location, signature, visibility |
| `SymbolKind` | Function, Class, Method, Variable, Import, Interface, Enum, `TypeAlias`, Trait, Module, Document |
| `Edge` | Relationship between symbols (source → target) |
| `EdgeKind` | Calls, Imports, Inherits, References, Raises, Implements, `TypeOf` |
| `EdgeProvenance` | Which tier/source resolved a `target_id` (heuristic tiers + LSP outcomes) |
| `Visibility` | Public, Private, Protected |
| `FileInfo` | Indexed file metadata (path, hash, language, symbol count) |
| `ChangesResult` | Result of a git-changes query |
| `symbol_id()` | Build a stable symbol ID from components |
| `detect_language()` | Map file extension to language name (`None` for unrecognized extensions) |

## Crate dependencies

No internal cartog crates (this is the root of the dependency graph). External: `serde`, `anyhow`, `schemars`.
