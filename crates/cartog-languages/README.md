# cartog-languages

Tree-sitter language extractors for the cartog code graph.

## Overview

Parses source code using tree-sitter grammars and extracts symbols (functions, classes, methods, etc.) and edges (calls, imports, inherits, etc.). Each language has a dedicated extractor implementing the `Extractor` trait.

## How it works

### Extractor trait

```rust,ignore
pub trait Extractor: Send {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult>;
}
```

Takes `&mut self` so implementations can reuse their internal `tree_sitter::Parser` across files, avoiding per-file allocation overhead.

### Tree-sitter S-expression queries

Extractors use **declarative S-expression queries** (not cursor walking) to match AST patterns. Queries are compiled once in the extractor's `new()` constructor and reused on every `extract()` call via the `CachedQuery` helper.

Example (Python call extraction):

```scheme
(call function: [(identifier) (attribute)] @callee)
```

Named captures (`@callee`, `@exception_type`, etc.) identify the matched nodes for symbol/edge construction.

### Nested scope filtering

`is_inside_nested_scope()` walks up the AST from a node to a given root node, checking if any ancestor in between matches a set of scope kinds (e.g., `function_definition`, `class_definition`). This prevents extracting edges from nested function bodies as if they belong to the outer scope.

### Supported languages

**Code**: Python, TypeScript, TSX, JavaScript, Rust, Go, Ruby, Java, C#, PHP, Dart, Swift, Kotlin.

**Frontend SFCs**: Vue (`.vue`), Svelte (`.svelte`), Astro (`.astro`) — the `<script>` / frontmatter block is sliced out, parsed by the JS/TS extractor, and its byte/line offsets are remapped back to the full file.

**Frameworks**: JSX component usage (`<Counter/>`) emits a `Calls` edge in `.jsx`/`.tsx` (React) and inside SFC scripts — component composition becomes part of the call graph.

**Documents**: Markdown (`.md`) — chunked by heading for semantic search. Each heading section becomes a `Document` symbol. Large sections are sub-chunked at paragraph boundaries (~1500 bytes). Files without headings use fixed-size paragraph chunking.

A crate-internal `js_shared` module holds extraction logic shared between the JavaScript and TypeScript/TSX extractors (not part of the public API).

## Public API

| Export | Description |
|--------|-------------|
| `Extractor` | Trait for language-specific extraction |
| `ExtractionResult` | Symbols + edges extracted from a file |
| `get_extractor()` | Factory: language name → `Box<dyn Extractor>` |
| `detect_language()` | Re-export from `cartog-core` |
| `python`, `go`, `java`, `csharp`, `javascript`, `typescript`, `ruby`, `php`, `dart`, `swift`, `kotlin`, `rust_lang` | Per-language extractor modules (note Rust's module is `rust_lang`) |
| `sfc` | Vue/Svelte/Astro single-file-component extractors (`VueExtractor`, `SvelteExtractor`, `AstroExtractor`) |
| `markdown` | Markdown document extractor (heading-based chunking) |

## Crate dependencies

`cartog-core`
