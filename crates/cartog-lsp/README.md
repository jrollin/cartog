# cartog-lsp

LSP-based edge resolution for the cartog code graph.

## Overview

Resolves edges that the heuristic resolver in `cartog-db` left unresolved, by querying real language servers (pyright, rust-analyzer, typescript-language-server, intelephense, etc.) for `textDocument/definition` responses. This is an optional precision feature, gated behind the `lsp` feature flag.

## How it works

### Resolution flow

1. Fetch all unresolved edges (`target_id IS NULL`) from the database
2. Group edges by language (detected from file extension)
3. Start an LSP server for each language found on `PATH`
4. For each file with unresolved edges:
   - Send `textDocument/didOpen` to the server
   - For each edge, find the target name's **column** in the source line
   - Send `textDocument/definition` request at that position
   - If the server returns a location, look up the symbol in the DB at that file+line
   - Update the edge's `target_id`
   - Send `textDocument/didClose`

### Column finding

Uses **word-boundary matching** to locate the target name in a source line. Checks `is_ascii_alphanumeric` / `_` boundaries on both sides to prevent matching inside longer identifiers (e.g., `validate` should not match inside `revalidate`).

Byte offsets are converted to **UTF-16 code units** as required by the LSP specification.

### Warm start

The `LspManager` can be shared across multiple indexing calls. Already-started servers stay alive between invocations, avoiding the cold-start penalty of LSP initialization.

## Public API

| Export | Description |
|--------|-------------|
| `lsp_resolve_edges()` | Main entry point — resolve unresolved edges via LSP |
| `manager::LspManager` | Manages running LSP server instances per language |
| `client::LspClient` | Minimal synchronous LSP client over stdio pipes |
| `servers::SERVERS` | Static registry of supported language servers |

## Crate dependencies

`cartog-core`, `cartog-db`
