// SPDX-License-Identifier: Apache-2.0
//! Probe bodies, grouped by what they exercise. The catalogue lives in `registry`.
//!
//! Rules every probe follows:
//! 1. Register cleanup in `ctx.ledger` (or let the `Stage` do it) before the side effect.
//! 2. Record measurements as soon as they exist.
//! 3. `Check` for a contradicted requirement, `Unsupported` only through
//!    `ctx.feature_failure`, `Skip` for a missing precondition.
//! 4. Never call `Session::try_revive`: the executor owns reconnect policy.
//!
//! PRACTICE text written here, outside `t32stage::practice`, is probe-only: it measures how
//! PowerView behaves and never becomes a Phase 1 code path.

pub mod optin;
pub mod scripts;
pub mod storage;
pub mod transport;

use std::time::Duration;

use t32rcl::{ErrorClass, Session};
use t32stage::{
    Bundle, BundleLimits, RelPath, RunReport, RunRequest, Snapshot, Stage, StageError, Undo, VmRegion, VmSnapshot,
};

use crate::ctx::{ProbeCtx, ProbeFail};

/// Deterministic bytes that differ per `seed` and are not compressible.
pub(crate) fn pattern(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (x >> 33) as u8
        })
        .collect()
}

/// Regions from the probe's scratch window, with the memory they cover captured and its
/// restore registered before anything is written.
pub(crate) fn scratch_regions(ctx: &mut ProbeCtx<'_>, sizes: &[u32]) -> Result<Vec<VmRegion>, ProbeFail> {
    let window = ctx.scratch()?;
    let regions = window.carve(sizes)?;
    let (first, last) = (regions[0], regions[regions.len() - 1]);
    let span = std::num::NonZeroU32::new(last.start() + last.len() - first.start()).expect("non-empty regions");
    let snap = VmSnapshot::capture(ctx.session, VmRegion::new(first.start(), span)?)?;
    ctx.measure("vm_occupied", crate::outcome::Value::Flag(snap.occupied()));
    ctx.measure("vm_uninit_units", crate::outcome::Value::Count(snap.uninitialized() as u64));
    ctx.push(Undo::RestoreVm(snap))?;
    Ok(regions)
}

pub(crate) fn bundle_of(files: &[(&str, &[u8])]) -> Result<Bundle, ProbeFail> {
    let mut b = Bundle::new(BundleLimits::default());
    for (path, bytes) in files {
        b.insert(RelPath::new(path)?, bytes.to_vec()).map_err(|e| ProbeFail::Stage(e.into()))?;
    }
    Ok(b)
}

pub(crate) fn run_entry(
    stage: &mut Stage,
    session: &mut Session,
    snap: &Snapshot,
    entry: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<RunReport, ProbeFail> {
    let args = args.iter().map(|a| a.to_string()).collect();
    let req = RunRequest { entry: RelPath::new(entry)?, args, timeout, collect_outputs: false };
    Ok(stage.run(session, snap, &req)?)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A refusal or a staging check that did not hold becomes `None`, a measured "no"; a
/// connection-fatal or cancelling error aborts the probe.
pub(crate) fn soft<T, E: Into<StageError>>(r: Result<T, E>) -> Result<Option<T>, ProbeFail> {
    match r.map_err(Into::into) {
        Ok(v) => Ok(Some(v)),
        Err(StageError::Rcl(e)) if matches!(e.class(), ErrorClass::ConnectionFatal | ErrorClass::Cancelled) => {
            Err(e.into())
        }
        Err(StageError::Journal { path, source }) => Err(ProbeFail::Stage(StageError::Journal { path, source })),
        Err(_) => Ok(None),
    }
}

/// `ok` or `FAIL`, the two-valued result token of most checks.
pub(crate) fn ok(cond: bool) -> &'static str {
    if cond { "ok" } else { "FAIL" }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod probe_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_are_deterministic_and_seed_dependent() {
        assert_eq!(pattern(1, 64), pattern(1, 64));
        assert_ne!(pattern(1, 64), pattern(2, 64));
        assert!(pattern(3, 4096).iter().filter(|&&b| b == 0).count() < 100);
    }
}
