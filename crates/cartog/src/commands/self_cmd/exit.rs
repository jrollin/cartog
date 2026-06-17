//! Exit codes for the `cartog self update` path. Mirrors the contract
//! documented on `cmd_self_update`.

pub(crate) const SUCCESS: i32 = 0;
pub(crate) const NETWORK_OR_PARSE_ERROR: i32 = 2;
pub(crate) const CARGO_INSTALL_REFUSED: i32 = 3;
pub(crate) const CHECKSUM_FAILED: i32 = 4;
pub(crate) const DISK_OR_PERMISSION_FAILED: i32 = 5;
pub(crate) const PEER_RUNNING: i32 = 6;
/// `--apply-pending` only: the new binary failed its smoke test and the
/// previous one was restored. Distinct from `5` (transient disk fault) so
/// the SessionEnd hook can treat it as terminal — the intent is cleared,
/// not retried. The plain `cartog self update` path keeps mapping a smoke
/// failure to `5` for backward compatibility.
pub(crate) const SMOKE_TEST_FAILED: i32 = 7;
