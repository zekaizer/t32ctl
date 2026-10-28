// SPDX-License-Identifier: Apache-2.0
//! What a probe sees: the session, the environment, a ledger for its cleanup, a lazily opened
//! `Stage`, and a recorder whose measurements survive a probe that fails halfway.

use std::time::Instant;

use t32rcl::{Feature, RemoteKind, Session, Support};
use t32stage::{
    CleanupReport, ExistsFn, LeaseError, Ledger, OpenError, Retention, Stage, StageConfig, StageError, StagePath, Undo,
    VmLease, VmWindow,
};

use crate::{
    env::{Env, Learned},
    outcome::{Measurement, SkipReason, Value},
    registry::ProbeId,
};

/// Why a probe stopped before its end. `?` converts library errors.
#[derive(Debug)]
pub enum ProbeFail {
    Rcl(t32rcl::Error),
    Stage(StageError),
    /// The measurement contradicts a requirement. The message names expected and actual.
    Check(String),
    Skip(SkipReason),
    Unsupported(String),
}

impl From<t32rcl::Error> for ProbeFail {
    fn from(e: t32rcl::Error) -> Self {
        ProbeFail::Rcl(e)
    }
}

impl From<StageError> for ProbeFail {
    fn from(e: StageError) -> Self {
        match e {
            StageError::Rcl(e) => ProbeFail::Rcl(e),
            StageError::Busy { depth } => ProbeFail::Skip(SkipReason::PowerViewBusy { depth }),
            StageError::Lease(LeaseError::Busy { holder }) => ProbeFail::Skip(SkipReason::VmLeaseBusy { holder }),
            other => ProbeFail::Stage(other),
        }
    }
}

macro_rules! via_stage {
    ($($t:ty),+) => {$(
        impl From<$t> for ProbeFail {
            fn from(e: $t) -> Self {
                StageError::from(e).into()
            }
        }
    )+};
}

via_stage!(t32stage::VmError, t32stage::PathError, t32stage::LeaseError, t32stage::TarError, t32rcl::LocalError);

pub type ProbeResult = Result<(), ProbeFail>;

/// The probe's `Stage`. A failed open keeps the cleanup it registered for the executor, which
/// runs it under the cleanup `Cancel`; the open is not retried.
#[derive(Debug)]
pub enum StageSlot {
    Unopened,
    Open(Box<Stage>),
    Failed(Ledger),
}

impl StageSlot {
    /// Bytes its pending `VM:` restores write back.
    pub fn restore_bytes(&self) -> u64 {
        match self {
            StageSlot::Unopened => 0,
            StageSlot::Open(stage) => crate::exec::restore_bytes(stage.pending()),
            StageSlot::Failed(ledger) => crate::exec::restore_bytes(ledger.pending()),
        }
    }

    /// Run whatever the stage registered.
    pub fn close(self, session: &mut Session) -> CleanupReport {
        match self {
            StageSlot::Unopened => CleanupReport::default(),
            StageSlot::Open(stage) => stage.close(session),
            StageSlot::Failed(ledger) => ledger.run(session),
        }
    }
}

pub struct ProbeCtx<'a> {
    pub session: &'a mut Session,
    pub env: &'a Env,
    pub probe: ProbeId,
    /// When the executor cancels the probe, on the session clock.
    pub deadline: Instant,
    /// Register undos here before creating anything; the executor runs it on every exit path.
    pub ledger: Ledger,
    pub learned: Learned,
    stage: StageSlot,
    lease: Option<VmLease>,
    measurements: Vec<Measurement>,
}

/// What the executor takes back after the probe returned or panicked.
pub struct Parts {
    pub measurements: Vec<Measurement>,
    pub ledger: Ledger,
    pub stage: StageSlot,
    pub learned: Learned,
    /// Dropped after cleanup, so the restore runs under the lease.
    pub lease: Option<VmLease>,
}

impl<'a> ProbeCtx<'a> {
    pub fn new(session: &'a mut Session, env: &'a Env, probe: ProbeId, ledger: Ledger, deadline: Instant) -> Self {
        Self {
            session,
            env,
            probe,
            deadline,
            ledger,
            learned: env.learned,
            stage: StageSlot::Unopened,
            lease: None,
            measurements: Vec::new(),
        }
    }

    /// JSON only.
    pub fn measure(&mut self, key: &str, value: Value) {
        self.measurements.push(Measurement { key: key.to_string(), value, headline: false });
    }

    /// Also on the one-line report, in call order.
    pub fn headline(&mut self, key: &str, value: Value) {
        self.measurements.push(Measurement { key: key.to_string(), value, headline: true });
    }

    pub fn text(&mut self, key: &str, value: impl Into<String>) {
        self.headline(key, Value::Text(value.into()));
    }

    pub fn check(&self, cond: bool, what: impl FnOnce() -> String) -> Result<(), ProbeFail> {
        if cond { Ok(()) } else { Err(ProbeFail::Check(what())) }
    }

    pub fn push(&mut self, undo: Undo) -> Result<(), ProbeFail> {
        Ok(self.ledger.push(undo)?)
    }

    /// What the build number says about `f`: `Present` is reliable, `Unknown` means "try it".
    pub fn support(&mut self, f: Feature) -> Support {
        self.session.build().map_or(Support::Unknown, |b| b.support(f))
    }

    /// A failed call for a feature: `Unsupported` when the build gate did not promise it and
    /// PowerView refused it as an unknown command or function; otherwise the error stands.
    pub fn feature_failure(&mut self, f: Feature, e: t32rcl::Error) -> ProbeFail {
        let refused =
            e.remote().is_some_and(|r| matches!(r.kind, RemoteKind::CommandFailed | RemoteKind::FunctionFailed));
        if refused && self.support(f) == Support::Unknown {
            ProbeFail::Unsupported(format!("{}: {e}", f.info().id))
        } else {
            ProbeFail::Rcl(e)
        }
    }

    pub fn exists_fn(&self) -> ExistsFn {
        self.learned.exists.unwrap_or_default()
    }

    /// The probe's own `VM:` area (first half of the window), leased for the probe's duration.
    pub fn scratch(&mut self) -> Result<VmWindow, ProbeFail> {
        let w = self.env.scratch_window();
        if self.lease.is_none() {
            self.lease =
                Some(VmLease::acquire(&self.env.cfg.state_dir, w, &format!("t32probe {}", self.env.cfg.run_id))?);
        }
        Ok(w)
    }

    /// A `Stage` on `probe-<run id>` in the second half of the window, snapshotting `VM:` it
    /// touches and removing everything it creates. Closed by the executor.
    pub fn stage(&mut self) -> Result<(&mut Stage, &mut Session), ProbeFail> {
        if matches!(self.stage, StageSlot::Unopened) {
            let cfg = StageConfig {
                window: self.env.stage_window(),
                guard: t32stage::VmGuard::SnapshotRestore,
                state_dir: self.env.cfg.state_dir.clone(),
                root: self.env.cfg.stage_root.clone(),
                subdir: Some(t32stage::RelPath::new(&format!("probe-{}", self.env.cfg.run_id))?),
                retention: Retention::Transient,
                exists: self.exists_fn(),
                poll: t32stage::PollSchedule::DEFAULT,
                holder: format!("t32probe {}", self.env.cfg.run_id),
                journal_name: format!("{}-{}-stage", self.env.cfg.run_id, self.probe.name()),
            };
            self.stage = match Stage::open(self.session, cfg) {
                Ok(stage) => StageSlot::Open(Box::new(stage)),
                Err(failed) => {
                    let OpenError { error, cleanup } = *failed;
                    self.stage = StageSlot::Failed(cleanup);
                    return Err(error.into());
                }
            };
        }
        match &mut self.stage {
            StageSlot::Open(stage) => Ok((&mut **stage, &mut *self.session)),
            _ => Err(ProbeFail::Check("the stage failed to open earlier in this probe".into())),
        }
    }

    /// `probe-<run id>/<rel>` below the stage root, its removal registered with the stage so it
    /// unwinds before the stage's directories.
    pub fn staged_file(&mut self, rel: &str) -> Result<StagePath, ProbeFail> {
        let (stage, _) = self.stage()?;
        let p = stage.path(rel)?;
        stage.register(Undo::RemoveFile(p.clone()))?;
        Ok(p)
    }

    pub(crate) fn into_parts(self) -> Parts {
        Parts {
            measurements: self.measurements,
            ledger: self.ledger,
            stage: self.stage,
            learned: self.learned,
            lease: self.lease,
        }
    }
}
