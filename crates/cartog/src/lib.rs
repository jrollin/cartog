#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

pub use cartog_core as types;
pub use cartog_db as db;
pub use cartog_indexer as indexer;
pub use cartog_languages as languages;
pub use cartog_rag as rag;
pub use cartog_watch as watch;

#[cfg(feature = "lsp")]
#[cfg_attr(docsrs, doc(cfg(feature = "lsp")))]
pub use cartog_lsp as lsp;

/// PID-file locks used by long-lived commands (`serve`, `watch`) and
/// consulted by `cartog self update` to detect concurrent peers.
///
/// Hidden from rustdoc: this is internal CLI plumbing re-exported only
/// so integration tests can reach it without each consumer crate
/// importing `cartog-process-lock` directly. Not a stable public API.
#[doc(hidden)]
pub use cartog_process_lock as process_lock;

/// Predicate gating the daily background update check.
///
/// Hidden from rustdoc: internal CLI plumbing exposed via the lib facade
/// only so integration tests can reach it. Not a stable public API.
#[doc(hidden)]
pub mod auto_check;

/// Persistent CLI state (last update check, last known latest version).
///
/// Hidden from rustdoc: internal CLI plumbing exposed via the lib facade
/// only so the auto-check thread (`auto_check::spawn_check`) and
/// integration tests can reach it. Not a stable public API.
#[doc(hidden)]
pub mod state;

/// RFC3339 ↔ Unix-seconds helpers used by `state.toml` writers and
/// `auto_check`. Internal — exposed via the lib facade only so the bin
/// target's `commands::self_cmd` can reach it without a duplicate copy.
#[doc(hidden)]
pub mod time_fmt;
