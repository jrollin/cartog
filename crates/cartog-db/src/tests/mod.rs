//! Unit tests for cartog-db, split by concern.

use crate::*;

pub(super) fn test_symbol(name: &str, kind: SymbolKind, file: &str, line: u32) -> Symbol {
    Symbol::new(name, kind, file, line, line + 5, 0, 100, None)
}

mod atomicity;
mod crud;
mod errors;
mod migration;
mod normalize;
mod rag;
mod resolution;
mod resolution_state;
mod traversal;
