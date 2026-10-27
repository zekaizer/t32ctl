// SPDX-License-Identifier: Apache-2.0
//! The executor runs the selected probes in table order and owns every guarantee the operator
//! relies on, whatever a single probe does. Per probe:
//!
//! 1. Interrupted before start: `Interrupted`. A required probe did not pass: `Skipped(Prereq)`.
//! 2. Connection: a dead session gets one `try_revive`; failure skips every later probe.
//! 3. Deadline: the session gets `Cancel::with_deadline`, sharing the Ctrl-C flag.
//! 4. The probe runs inside `catch_unwind`; a panic is `Fail`.
//! 5. Cleanup: a fresh `Cancel` with its own deadline replaces the probe's (an expired deadline
//!    or Ctrl-C must not cancel the cleanup), a dead session is revived once and recorded, then
//!    the probe's `Stage` and ledger run.
//! 6. The observer persists the JSON before the next probe starts.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    time::{Duration, Instant},
};

use t32rcl::{Cancel, CancelReason, Error, Health, Session};
use t32stage::{CleanupReport, Journal, Ledger, StageError};

use crate::{
    ctx::{ProbeCtx, ProbeFail},
    env::Env,
    outcome::{ErrorRecord, Outcome, Revive, SkipReason, TraceSummary, Verdict, cleanup_records},
    registry::{ProbeDef, ProbeId},
};

/// Deadline of the cleanup after each probe, independent of the probe's own.
pub const CLEANUP_BUDGET: Duration = Duration::from_secs(30);

pub trait Observer {
    fn starting(&mut self, def: &ProbeDef);
    fn finished(&mut self, env: &Env, outcomes: &[Outcome], replay: &CleanupReport);
}

#[derive(Debug, Default)]
pub struct RunResult {
    pub outcomes: Vec<Outcome>,
    /// Journals left by earlier runs, replayed once the connection is up.
    pub replay: CleanupReport,
    pub interrupted: bool,
}

pub struct Executor<'a> {
    env: &'a mut Env,
    session: &'a mut Session,
    observer: &'a mut dyn Observer,
    result: RunResult,
    lost: Option<String>,
}

impl<'a> Executor<'a> {
    pub fn new(env: &'a mut Env, session: &'a mut Session, observer: &'a mut dyn Observer) -> Self {
        Self { env, session, observer, result: RunResult::default(), lost: None }
    }

    pub fn run(mut self, defs: &[&'static ProbeDef]) -> RunResult {
        for def in defs {
            self.observer.starting(def);
            let outcome = self.run_one(def);
            self.result.outcomes.push(outcome);
            self.observer.finished(self.env, &self.result.outcomes, &self.result.replay);
        }
        self.result.interrupted = self.env.cancel.reason(Instant::now()) == Some(CancelReason::Interrupt);
        self.result
    }

    fn run_one(&mut self, def: &'static ProbeDef) -> Outcome {
        let started = Instant::now();
        if self.env.cancel.reason(started) == Some(CancelReason::Interrupt) {
            return Outcome {
                skip: Some(SkipReason::NotApplicable { why: "interrupted before start".into() }),
                ..Outcome::new(def.id, Verdict::Interrupted)
            };
        }
        if let Some(reason) = self.unmet_prerequisite(def) {
            return Outcome::skipped(def.id, reason);
        }
        if def.needs_connection
            && let Some(reason) = self.ensure_connection()
        {
            return Outcome::skipped(def.id, reason);
        }

        let probe_cancel = self.env.cancel.with_deadline(started + self.env.deadline(def.deadline));
        self.session.set_cancel(probe_cancel.clone());
        self.session.take_trace();
        let journal_name = format!("{}-{}", self.env.cfg.run_id, def.id.name());
        let (ledger, journal_error) = match Journal::create(&self.env.cfg.state_dir, &journal_name) {
            Ok(j) => (Ledger::journaled(j, self.env.exists()), None),
            Err(e) => (Ledger::new(self.env.exists()), Some(e.to_string())),
        };

        let mut ctx = ProbeCtx::new(self.session, self.env, def.id, ledger);
        let result = catch_unwind(AssertUnwindSafe(|| (def.run)(&mut ctx))).map_err(panic_text);
        let parts = ctx.into_parts();
        let trace = TraceSummary::from(&self.session.take_trace());
        let cancelled = probe_cancel.reason(Instant::now());

        self.session.set_cancel(Cancel::new().with_deadline(Instant::now() + CLEANUP_BUDGET));
        let revive = (def.needs_connection && self.session.health() != Health::Live).then(|| self.revive());
        let mut cleanup = CleanupReport::default();
        if let Some(stage) = parts.stage {
            cleanup.extend(stage.close(self.session));
        }
        cleanup.extend(parts.ledger.run(self.session));
        drop(parts.lease);
        self.session.set_cancel(self.env.cancel.clone());

        let (mut verdict, mut error, skip) = verdict_of(result, cancelled);
        if let Some(r) = revive.as_ref().filter(|r| !r.ok)
            && verdict == Verdict::Pass
        {
            verdict = Verdict::Fail;
            let why = r.error.clone().unwrap_or_default();
            error = Some(ErrorRecord::new("revive", format!("connection not usable after the reconnect: {why}")));
        }
        self.env.learned = parts.learned;
        if def.id == ProbeId::Connect && verdict == Verdict::Pass {
            self.env.endpoint = Some(self.session.endpoint().clone());
            self.result.replay = t32stage::replay_leftovers(self.session, &self.env.cfg.state_dir, self.env.exists());
        }
        let mut measurements = parts.measurements;
        if let Some(e) = journal_error {
            measurements.push(crate::outcome::Measurement {
                key: "journal_error".into(),
                value: crate::outcome::Value::Text(e),
                headline: false,
            });
        }
        Outcome {
            probe: def.id,
            verdict,
            measurements,
            error,
            skip,
            cleanup: cleanup_records(&cleanup),
            revive,
            trace,
            elapsed: started.elapsed(),
        }
    }

    fn unmet_prerequisite(&self, def: &ProbeDef) -> Option<SkipReason> {
        def.requires.iter().find_map(|req| {
            let got = self.result.outcomes.iter().find(|o| o.probe == *req)?;
            match &got.skip {
                Some(root @ SkipReason::Prereq { .. }) => Some(root.clone()),
                _ => (got.verdict != Verdict::Pass)
                    .then(|| SkipReason::Prereq { probe: req.name().into(), verdict: got.verdict }),
            }
        })
    }

    /// `None` when usable. A failure is remembered, so later probes do not touch the network.
    fn ensure_connection(&mut self) -> Option<SkipReason> {
        if let Some(detail) = &self.lost {
            return Some(SkipReason::ConnectionLost { detail: detail.clone() });
        }
        match self.session.try_revive() {
            Ok(_) => None,
            Err(e) => {
                self.lost = Some(e.to_string());
                Some(SkipReason::ConnectionLost { detail: e.to_string() })
            }
        }
    }

    /// The single reconnect after a probe left the connection dead, plus the first call on the
    /// new connection: PowerView may still be busy with what the old one started.
    fn revive(&mut self) -> Revive {
        let t0 = Instant::now();
        match self.session.try_revive() {
            Ok(_) => {
                let connect = t0.elapsed();
                let t1 = Instant::now();
                let first = self.session.eval("TRUE()");
                Revive {
                    ok: first.is_ok(),
                    connect,
                    first_call: Some(t1.elapsed()),
                    error: first.err().map(|e| e.to_string()),
                }
            }
            Err(e) => {
                self.lost = Some(e.to_string());
                Revive { ok: false, connect: t0.elapsed(), first_call: None, error: Some(e.to_string()) }
            }
        }
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_string())
}

fn rcl_record(e: &Error) -> ErrorRecord {
    let hint = match e {
        Error::Connect(c) => Some(c.hint()),
        _ => None,
    };
    ErrorRecord::from_error(e, e.tag(), hint)
}

fn stage_record(e: &StageError) -> ErrorRecord {
    ErrorRecord::from_error(e, "stage", None)
}

/// Pure map from what the probe returned to a verdict.
///
/// | Probe returned | Cancel | Verdict |
/// |---|---|---|
/// | anything | interrupt | Interrupted |
/// | anything | deadline | Fail ("deadline exceeded") |
/// | `Ok` | none | Pass |
/// | `Check`, `Rcl`, `Stage`, panic | none | Fail |
/// | `Unsupported` | none | Unsupported |
/// | `Skip` | none | Skipped |
pub(crate) fn verdict_of(
    result: Result<Result<(), ProbeFail>, String>,
    cancelled: Option<CancelReason>,
) -> (Verdict, Option<ErrorRecord>, Option<SkipReason>) {
    let from_error = match &result {
        Ok(Err(ProbeFail::Rcl(Error::Cancelled { reason, .. }))) => Some(*reason),
        _ => None,
    };
    let error = match &result {
        Ok(Ok(())) | Ok(Err(ProbeFail::Skip(_))) => None,
        Ok(Err(ProbeFail::Rcl(e))) => Some(rcl_record(e)),
        Ok(Err(ProbeFail::Stage(e))) => Some(stage_record(e)),
        Ok(Err(ProbeFail::Check(m))) => Some(ErrorRecord::new("check", m.clone())),
        Ok(Err(ProbeFail::Unsupported(m))) => Some(ErrorRecord::new("unsupported", m.clone())),
        Err(panic) => Some(ErrorRecord::new("panic", format!("panic: {panic}"))),
    };
    match cancelled.or(from_error) {
        Some(CancelReason::Interrupt) => return (Verdict::Interrupted, error, None),
        Some(CancelReason::Deadline) => {
            let mut e = ErrorRecord::new("deadline", "probe deadline exceeded");
            if let Some(inner) = error {
                e.chain.push(inner.message);
            }
            return (Verdict::Fail, Some(e), None);
        }
        None => {}
    }
    match result {
        Ok(Ok(())) => (Verdict::Pass, None, None),
        Ok(Err(ProbeFail::Skip(r))) => (Verdict::Skipped, None, Some(r)),
        Ok(Err(ProbeFail::Unsupported(_))) => (Verdict::Unsupported, error, None),
        _ => (Verdict::Fail, error, None),
    }
}

#[cfg(test)]
#[path = "exec_tests.rs"]
pub(crate) mod tests;
