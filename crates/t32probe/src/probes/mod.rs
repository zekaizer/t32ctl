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

pub mod lab;
pub mod optin;
pub mod scriptlab;
pub mod scripts;
pub mod storage;
pub mod transport;

use std::time::Duration;

use t32rcl::{CallBudget, ErrorClass, Session};
use t32stage::{
    Bundle, BundleLimits, RelPath, RunReport, RunRequest, Snapshot, Stage, StageError, StagePath, StageRoot, Undo,
    VmRegion, VmSnapshot,
    practice::{cmd, fnc},
    transfer,
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

/// Connected Remote API clients, from the first spelling in `fnc::NR_USED_CONS` PowerView does
/// not refuse. When every spelling is refused, the last refusal.
pub(crate) fn nr_used_cons(session: &mut Session) -> Result<i64, t32rcl::Error> {
    let mut refused = None;
    for expr in fnc::NR_USED_CONS {
        match session.eval(expr).and_then(|v| v.into_int(expr)) {
            Err(e) if e.remote().is_some() => refused = Some(e),
            answered => return answered,
        }
    }
    Err(refused.expect("NR_USED_CONS is not empty"))
}

/// The tempdir rule: `--stage-root`, else `OS.PTD()\t32ctl`.
pub(crate) fn stage_root(ctx: &mut ProbeCtx<'_>) -> Result<StageRoot, ProbeFail> {
    if let Some(p) = &ctx.env.cfg.stage_root {
        return Ok(StageRoot::new(p.clone()));
    }
    let ptd = ctx.session.eval(fnc::PTD)?.into_text(fnc::PTD)?;
    StageRoot::from_ptd(&ptd).map_err(|e| ProbeFail::Check(format!("OS.PTD() gives no stage root: {e}")))
}

/// `<root>\probe-<run>-<tag>`, its removal registered before `MKDIR`. The root is created if
/// missing and never removed. An existing directory is refused: it holds an earlier run's files.
pub(crate) fn workdir(ctx: &mut ProbeCtx<'_>, root: &StageRoot, tag: &str) -> Result<StagePath, ProbeFail> {
    transfer::ensure_root(ctx.session, root)?;
    let dir = root.path(RelPath::new(&format!("probe-{}-{tag}", ctx.env.cfg.run_id))?);
    let expr = fnc::dir_exists(&dir);
    if ctx.session.eval(&expr)?.into_bool(&expr)? {
        return Err(ProbeFail::Check(format!("work directory already exists: {dir}")));
    }
    ctx.push(Undo::RemoveDir(dir.clone()))?;
    ctx.session.cmd(&cmd::mkdir(&dir))?;
    if !ctx.session.eval(&expr)?.into_bool(&expr)? {
        return Err(ProbeFail::Check(format!("MKDIR did not create {dir}")));
    }
    Ok(dir)
}

/// File names in a directory, not recursive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Listing {
    /// Names after the last `\` or `/`; `first_raw` is the first answer as PowerView gave it.
    Names {
        names: Vec<String>,
        first_raw: String,
    },
    Unavailable(String),
}

const LIST_MAX: usize = 1000;

/// `OS.FIRSTFILE("<dir>\*")`, then `OS.NEXTFILE()` until the empty string (ide_func), at most
/// `LIST_MAX` names, each call under a 3 s budget.
pub(crate) fn list_dir(session: &mut Session, dir: &StagePath) -> Result<Listing, ProbeFail> {
    let mut s = session.with_budget(CallBudget::idle(Duration::from_secs(3)).with_total(Duration::from_secs(3)));
    let first = format!("OS.FIRSTFILE(\"{}\\*\")", dir.text());
    let mut ask = |expr: &str| soft(s.eval(expr).and_then(|v| v.into_text(expr)));
    let Some(mut name) = ask(&first)? else { return Ok(Listing::Unavailable(format!("{first} refused"))) };
    let first_raw = name.clone();
    let mut names = Vec::new();
    while !name.is_empty() && names.len() < LIST_MAX {
        names.push(name.rsplit(['\\', '/']).next().unwrap_or_default().to_string());
        let Some(next) = ask("OS.NEXTFILE()")? else {
            return Ok(Listing::Unavailable("OS.NEXTFILE() refused".into()));
        };
        name = next;
    }
    Ok(Listing::Names { names, first_raw })
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
