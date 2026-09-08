//! Seam tests for the `cartog-registry` extraction.
//!
//! `slot_for_db`, `default_state_dir`/`default_state_file`, and
//! `detect_live_serve_peer` moved out of the binary crate into
//! `cartog-registry`, and `cartog::state` re-exports them. Two invariants the
//! single-crate version held by construction now span a crate boundary:
//!
//! 1. The slot generated on one side of the boundary is the slot the PID-lock
//!    writer/validator accepts on the other, and equivalent paths still
//!    collapse onto one slot.
//! 2. `crate::state::*` still resolves for its in-crate call sites and for
//!    integration tests that reach it through the lib facade.

use std::path::Path;

/// `cartog::state::slot_for_db` must stay a re-export of the registry's
/// function, not a wrapper that could drift from it.
///
/// Comparing `state::slot_for_db(x)` to `registry::slot_for_db(x)` would be
/// vacuous while the shim is a `pub use`: both paths resolve to one `fn` item,
/// so it compares `f(x)` to `f(x)`. Assert function-pointer identity instead —
/// that is what fails the moment someone replaces the re-export with a
/// hand-written wrapper, which would compile and return equal values for these
/// inputs while being free to diverge on others.
#[test]
fn state_slot_for_db_is_the_registry_function_itself_not_a_wrapper() {
    let via_state: fn(&str, &Path) -> String = cartog::state::slot_for_db;
    let via_registry: fn(&str, &Path) -> String = cartog::registry::slot_for_db;
    assert!(
        std::ptr::fn_addr_eq(via_state, via_registry),
        "cartog::state::slot_for_db must re-export the registry's function, \
         not reimplement it — a divergent wrapper is the drift this guards"
    );
}

/// The moved hashing must still collapse logically-equivalent paths. On macOS
/// `/tmp` is a symlink to `/private/tmp`, which is the canonical instance of
/// this class; elsewhere the explicit symlink below covers it.
#[test]
fn equivalent_paths_still_collapse_onto_one_slot_across_the_boundary() {
    let real = tempfile::TempDir::new().unwrap();
    let db = real.path().join("cartog.db");
    std::fs::write(&db, b"").unwrap();

    let link_parent = tempfile::TempDir::new().unwrap();
    let link = link_parent.path().join("linked");
    #[cfg(unix)]
    std::os::unix::fs::symlink(real.path(), &link).unwrap();
    #[cfg(not(unix))]
    {
        // Skip on non-Unix: symlink creation needs admin on Windows, and
        // x86_64-pc-windows-msvc is a release target. Same guard as the
        // sibling tests in cartog-registry's own slot.rs.
        let _ = link;
        return;
    }

    assert_eq!(
        cartog::state::slot_for_db("serve", &db),
        cartog::state::slot_for_db("serve", &link.join("cartog.db")),
        "a symlinked parent must not change the slot"
    );
}

/// The generator and the `<slot>.pid` writer/validator now live in different
/// crates. A generated slot must still round-trip through `acquire` →
/// `find_active_locks` as the same string — otherwise a peer would hold a lock
/// nobody looks for.
#[test]
fn a_generated_slot_round_trips_through_the_pid_lock_writer() {
    let state_dir = tempfile::TempDir::new().unwrap();
    let db_dir = tempfile::TempDir::new().unwrap();
    let db_path = db_dir.path().join("cartog.db");

    let slot = cartog::state::slot_for_db("serve", &db_path);
    let _held = cartog::process_lock::ProcessLock::acquire(state_dir.path(), &slot)
        .expect("a generated slot must satisfy the lock crate's slot validation");

    let found = cartog::process_lock::find_active_locks(state_dir.path());
    assert_eq!(
        found.iter().map(|l| l.slot.as_str()).collect::<Vec<_>>(),
        vec![slot.as_str()]
    );
}

/// `detect_live_serve_peer` composes the moved `slot_for_db` with the lock
/// crate's scan. Assert the composition still finds a peer holding the slot the
/// generator produces — the seam the two halves meet at.
#[test]
fn detect_live_serve_peer_still_matches_a_generated_serve_slot() {
    let state_dir = tempfile::TempDir::new().unwrap();
    let db_dir = tempfile::TempDir::new().unwrap();
    let db_path = db_dir.path().join("cartog.db");

    let _held = cartog::process_lock::ProcessLock::acquire(
        state_dir.path(),
        &cartog::registry::slot_for_db("serve", &db_path),
    )
    .expect("acquire serve lock");

    let peer = cartog::state::detect_live_serve_peer(state_dir.path(), &db_path)
        .expect("the composed detector must see a peer on the generated slot");
    assert_eq!(peer.pid, std::process::id());
}

/// The shim keeps every `cartog::state::*` path resolving: the two moved
/// path helpers alongside `State`, which never moved.
#[test]
fn the_state_reexport_shim_keeps_every_moved_name_resolving() {
    // Naming all four items is itself the compile-time assertion. The runtime
    // checks confirm the re-exported helpers agree with the registry's own.
    assert_eq!(
        cartog::state::default_state_dir(),
        cartog::registry::default_state_dir()
    );
    assert_eq!(
        cartog::state::default_state_file(),
        cartog::registry::default_state_file()
    );
    // `State` stayed in the binary crate: the module must still carry both the
    // moved helpers and its own type.
    let s = cartog::state::State::default();
    assert!(s.pending_update.is_none(), "a default State arms no update");
}

/// `default_state_file` must stay inside `default_state_dir` after the move —
/// they now live in the same module but are reached through two re-exports.
#[test]
fn the_state_file_still_lives_inside_the_state_dir() {
    if let (Some(dir), Some(file)) = (
        cartog::state::default_state_dir(),
        cartog::state::default_state_file(),
    ) {
        assert_eq!(file.parent(), Some(dir.as_path()));
    }
}
