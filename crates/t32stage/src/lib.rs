// SPDX-License-Identifier: Apache-2.0
//! `t32stage`: move files and scripts to a TRACE32 PowerView over RCL only, run them, and
//! clean up after them.
//!
//! `practice` is the only place PRACTICE command text is formatted. `Ledger` records cleanup
//! before every side effect and journals it on the WSL side.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod error;
mod ledger;
mod macros;
pub mod practice;
mod vm;

#[cfg(test)]
mod testutil;

pub use error::{ArgError, LeaseError, PathError, StageError, VmError};
pub use ledger::{
    CleanupItem, CleanupOutcome, CleanupReport, Journal, Ledger, Undo, default_state_dir, replay_leftovers,
};
pub use macros::Macro;
pub use practice::{ExistsFn, PvPath, RelPath, StagePath, StageRoot, Symbol};
pub use vm::{Prior, RestoreOutcome, SNAPSHOT_UNIT, VmGuard, VmLease, VmRegion, VmSnapshot, VmWindow};
