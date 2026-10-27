// SPDX-License-Identifier: Apache-2.0
//! What a probe sees: the session, the environment, a ledger for its cleanup, a lazily opened
//! `Stage`, and a recorder whose measurements survive a probe that fails halfway.

use t32rcl::{Feature, RemoteKind, Session, Support};
use t32stage::{
    ExistsFn, LeaseError, Ledger, Retention, Stage, StageConfig, StageError, StagePath, Undo, VmLease, VmWindow,
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

pub struct ProbeCtx<'a> {
    pub session: &'a mut Session,
    pub env: &'a Env,
    pub probe: ProbeId,
    /// Register undos here before creating anything; the executor runs it on every exit path.
    pub ledger: Ledger,
    pub learned: Learned,
    stage: Option<Stage>,
    lease: Option<VmLease>,
    measurements: Vec<Measurement>,
    note: Option<String>,
}

/// What the executor takes back after the probe returned or panicked.
pub struct Parts {
    pub measurements: Vec<Measurement>,
    pub note: Option<String>,
    pub ledger: Ledger,
    pub stage: Option<Stage>,
    pub learned: Learned,
    /// Dropped after cleanup, so the restore runs under the lease.
    pub lease: Option<VmLease>,
}

impl<'a> ProbeCtx<'a> {
    pub fn new(session: &'a mut Session, env: &'a Env, probe: ProbeId, ledger: Ledger) -> Self {
        Self {
            session,
            env,
            probe,
            ledger,
            learned: env.learned,
            stage: None,
            lease: None,
            measurements: Vec::new(),
            note: None,
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

    pub fn note(&mut self, text: impl Into<String>) {
        self.note = Some(text.into());
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
        if self.stage.is_none() {
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
            self.stage = Some(Stage::open(self.session, cfg)?);
        }
        Ok((self.stage.as_mut().expect("opened above"), &mut *self.session))
    }

    /// `probe-<run id>/<rel>` below the stage root.
    pub fn path(&mut self, rel: &str) -> Result<StagePath, ProbeFail> {
        let (stage, _) = self.stage()?;
        Ok(stage.path(rel)?)
    }

    pub(crate) fn into_parts(self) -> Parts {
        Parts {
            measurements: self.measurements,
            note: self.note,
            ledger: self.ledger,
            stage: self.stage,
            learned: self.learned,
            lease: self.lease,
        }
    }
}
