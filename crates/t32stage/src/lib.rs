// SPDX-License-Identifier: Apache-2.0
//! `t32stage`: move files and scripts to a TRACE32 PowerView over RCL only, run them, and
//! clean up after them.
//!
//! - [`Stage`]: open, ensure_snapshot, run, fetch_outputs, upload, download, close.
//! - [`Bundle`]: a validated file set that unpacks itself inside PowerView.
//! - [`practice`]: the only place PRACTICE command text is formatted.
//! - [`Ledger`]: cleanup registered before every side effect, journaled on the WSL side.
//! - [`transfer`]: single-file movement through `VM:`, and gzip.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod bundle;
mod error;
mod ledger;
mod macros;
pub mod practice;
mod runner;
mod stage;
mod tar;
pub mod transfer;
mod vm;

#[cfg(test)]
mod testutil;

pub use bundle::{Bundle, BundleLimits, Packed, SnapshotKey};
pub use error::{ArgError, BundleError, LeaseError, PathError, StageError, SymbolError, TarError, VmError};
pub use ledger::{
    CleanupItem, CleanupOutcome, CleanupReport, Journal, Ledger, Undo, default_state_dir, replay_leftovers,
};
pub use macros::Macro;
pub use practice::{ExistsFn, PvPath, RelPath, StagePath, StageRoot, Symbol};
pub use runner::{PollSchedule, PollStats, PracticeErrorInfo, RunOutcome, RunReport, RunRequest, runner_cmm};
pub use stage::{Outputs, Retention, Snapshot, Stage, StageConfig, StagedFile, UploadStats};
pub use tar::{TarDiag, TarEntry, diagnose as diagnose_tar, walk as walk_tar};
pub use transfer::Put;
pub use vm::{Prior, RestoreOutcome, SNAPSHOT_UNIT, VmGuard, VmLease, VmRegion, VmSnapshot, VmWindow};
