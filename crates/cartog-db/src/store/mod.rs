//! The `Database` inherent impl, split into cohesive submodules. Each file adds
//! one `impl Database` block; the struct and free helpers live in `lib.rs`.

use super::*;

mod crud;
mod lifecycle;
mod lsp;
mod queries;
mod rag;
mod resolution;
