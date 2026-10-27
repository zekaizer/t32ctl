// SPDX-License-Identifier: Apache-2.0
//! Running a script inside PowerView and knowing, without guessing, how it ended.
//!
//! `GetPracticeState` reads 0 for a script that halted at an error or at `STOP` although it is
//! still on the PRACTICE stack (t32mcp t32api.rs `practice_running`), so pyrcl's stack-depth
//! wait (`rcl.py cmm()`) cannot tell "failed" from "finished". Completion is proven by a token
//! instead: the client sets `&t32ctl_token` to a fresh value, the runner copies it into
//! `&t32ctl_done` as the last statement of both its success and its error path, and the client
//! compares.
//!
//! ```text
//! client                                   PowerView
//! require PRACTICE idle and stack depth 0
//! set t32ctl_token, _entry, _args, _done; bind t32ctl_snap, _out
//! ERROR.RESet; DO "<snap>\.t32ctl\runner.cmm"
//! loop GetPracticeState  ------------->    1 running | 2 dialog | 0 idle
//!   2 -> T32_Stop, unwind                             DialogAborted
//!   0 -> t32ctl_done == token ?  Finished: read rc, msg, ret
//!        else depth > 0          Halted: read ERROR.*, unwind
//!        else after start grace  Vanished
//! ```

use std::time::{Duration, Instant};

use t32rcl::PracticeState;

use crate::{macros::Macro, practice::RelPath};

/// Part of every bundle key through `runner_cmm()`.
pub const RUNNER_VERSION: u32 = 1;

/// PRACTICE text of `.t32ctl/runner.cmm`.
///
/// ASSUMED(script-error): `ON ERROR JUMPTO` in the runner catches errors raised inside nested
/// `DO` files and unwinds them; `ERROR.MESSAGE()` is assignable. ASSUMED(bundle): `ENTRY %LINE`
/// after `DO` yields the callee's `ENDDO` value. Syntax per practice_ref (`DO`, `ENTRY`, `ON`).
pub fn runner_cmm() -> String {
    let m = |x: Macro| x.amp();
    format!(
        "; t32ctl runner v{RUNNER_VERSION}\n\
         {done}=\"\"\n{rc}=\"\"\n{msg}=\"\"\n{ret}=\"\"\n\
         ON ERROR JUMPTO t32ctl_failed\n\
         DO \"{snap}\\{entry}\" {args}\n\
         ENTRY %LINE {ret}\n\
         ON ERROR inherit\n\
         {rc}=\"0\"\n{done}=\"{token}\"\n\
         ENDDO\n\
         t32ctl_failed:\n\
         ON ERROR inherit\n\
         {rc}=\"1\"\n{msg}=ERROR.MESSAGE()\n{done}=\"{token}\"\n\
         ENDDO\n",
        done = m(Macro::Done),
        rc = m(Macro::Rc),
        msg = m(Macro::Msg),
        ret = m(Macro::Ret),
        snap = m(Macro::Snap),
        entry = m(Macro::Entry),
        args = m(Macro::Args),
        token = m(Macro::Token),
    )
}

#[derive(Clone, Debug)]
pub struct RunRequest {
    pub entry: RelPath,
    pub args: Vec<String>,
    pub timeout: Duration,
    /// Create a per-run output directory (`&t32ctl_out`) the entry script may write into.
    pub collect_outputs: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollSchedule {
    pub initial: Duration,
    pub max: Duration,
    /// How long "idle, no token, empty stack" may last after the `DO` before it is `Vanished`.
    pub start_grace: Duration,
}

impl PollSchedule {
    pub const DEFAULT: PollSchedule = PollSchedule {
        initial: Duration::from_millis(10),
        max: Duration::from_millis(250),
        start_grace: Duration::from_millis(500),
    };

    /// Doubling, capped at `max`.
    pub fn next(&self, prev: Duration) -> Duration {
        (prev * 2).clamp(self.initial, self.max)
    }
}

impl Default for PollSchedule {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollStats {
    pub samples: u32,
    pub running: u32,
    pub dialog: u32,
    /// Idle samples after the first running sample and before completion. Non-zero during a
    /// nested `DO` chain means `GetPracticeState` dipped to 0 between scripts.
    pub idle_after_running: u32,
    pub first_running_after: Option<Duration>,
}

/// PRACTICE error details after a halt, read with `EVAL` + `EvalGetString`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PracticeErrorInfo {
    pub message: String,
    pub cmdline: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunOutcome {
    /// The runner reached its last statement. `rc != 0`: the runner caught a PRACTICE error.
    Finished { rc: i32, message: String, ret: String },
    /// Idle with scripts on the stack and no token: an uncaught error or `STOP`. Unwound.
    Halted { error: Option<PracticeErrorInfo> },
    /// State 2 seen; `T32_Stop` sent; unwound.
    DialogAborted,
    /// `RunRequest::timeout` elapsed; `T32_Stop` sent; unwound.
    TimedOut { last_state: PracticeState },
    /// Idle, empty stack, no token: the runner never reached its end.
    Vanished,
}

impl RunOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, RunOutcome::Finished { rc: 0, .. })
    }
}

#[derive(Clone, Debug)]
pub struct RunReport {
    pub token: String,
    pub outcome: RunOutcome,
    pub elapsed: Duration,
    pub polls: PollStats,
    pub out: Option<crate::practice::StagePath>,
}

/// What to do after a `GetPracticeState` sample.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Next {
    PollAfter(Duration),
    /// Send `T32_Stop`, then unwind.
    Stop,
    /// Idle: read `t32ctl_done` and the stack depth, then call `on_idle_detail`.
    NeedIdleDetail,
    TimedOut,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IdleVerdict {
    Finished,
    Halted,
    Vanished,
    PollAfter(Duration),
}

/// Pure state machine over `GetPracticeState` samples; every method takes `now`.
#[derive(Debug)]
pub(crate) struct ScriptWatch {
    timeout: Duration,
    schedule: PollSchedule,
    started: Instant,
    interval: Duration,
    pub stats: PollStats,
    idle_since: Option<Instant>,
    halted_seen: bool,
}

impl ScriptWatch {
    pub fn new(timeout: Duration, schedule: PollSchedule, now: Instant) -> Self {
        Self {
            timeout,
            schedule,
            started: now,
            interval: schedule.initial,
            stats: PollStats::default(),
            idle_since: None,
            halted_seen: false,
        }
    }

    pub fn on_state(&mut self, now: Instant, state: PracticeState) -> Next {
        self.stats.samples += 1;
        if now.saturating_duration_since(self.started) >= self.timeout {
            return Next::TimedOut;
        }
        match state {
            PracticeState::Dialog => {
                self.stats.dialog += 1;
                Next::Stop
            }
            PracticeState::Idle => {
                if self.stats.running > 0 {
                    self.stats.idle_after_running += 1;
                }
                Next::NeedIdleDetail
            }
            PracticeState::Running | PracticeState::Other(_) => {
                self.stats.running += 1;
                self.stats.first_running_after.get_or_insert(now.saturating_duration_since(self.started));
                self.idle_since = None;
                self.halted_seen = false;
                let wait = self.interval;
                self.interval = self.schedule.next(self.interval);
                Next::PollAfter(wait)
            }
        }
    }

    /// `token_done`: `t32ctl_done` equals the token. `depth`: PRACTICE stack depth now.
    /// Halted needs two consecutive idle samples with scripts on the stack, so a single dip
    /// between two nested `DO` files is not mistaken for a halt.
    pub fn on_idle_detail(&mut self, now: Instant, token_done: bool, depth: i64) -> IdleVerdict {
        if token_done {
            return IdleVerdict::Finished;
        }
        self.interval = self.schedule.initial;
        if depth > 0 {
            self.idle_since = None;
            if self.halted_seen {
                return IdleVerdict::Halted;
            }
            self.halted_seen = true;
            return IdleVerdict::PollAfter(self.schedule.initial);
        }
        self.halted_seen = false;
        let since = *self.idle_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= self.schedule.start_grace {
            IdleVerdict::Vanished
        } else {
            IdleVerdict::PollAfter(self.schedule.initial)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn schedule_doubles_then_caps() {
        let s = PollSchedule::DEFAULT;
        assert_eq!(s.next(ms(10)), ms(20));
        assert_eq!(s.next(ms(200)), ms(250));
        assert_eq!(s.next(ms(250)), ms(250));
    }

    fn watch() -> (ScriptWatch, Instant) {
        let t0 = Instant::now();
        (ScriptWatch::new(Duration::from_secs(5), PollSchedule::DEFAULT, t0), t0)
    }

    #[test]
    fn running_then_idle_with_the_token_finishes_and_counts_dips() {
        let (mut w, t0) = watch();
        assert_eq!(w.on_state(t0, PracticeState::Running), Next::PollAfter(ms(10)));
        assert_eq!(w.on_state(t0 + ms(10), PracticeState::Running), Next::PollAfter(ms(20)));
        assert_eq!(w.on_state(t0 + ms(30), PracticeState::Idle), Next::NeedIdleDetail);
        assert_eq!(w.on_idle_detail(t0 + ms(31), false, 2), IdleVerdict::PollAfter(ms(10)));
        assert_eq!(w.on_state(t0 + ms(41), PracticeState::Running), Next::PollAfter(ms(10)));
        assert_eq!(w.on_state(t0 + ms(51), PracticeState::Idle), Next::NeedIdleDetail);
        assert_eq!(w.on_idle_detail(t0 + ms(52), true, 0), IdleVerdict::Finished);
        assert_eq!(w.stats.running, 3);
        assert_eq!(w.stats.idle_after_running, 2);
        assert_eq!(w.stats.first_running_after, Some(Duration::ZERO));
    }

    #[test]
    fn a_dialog_is_stopped_and_a_timeout_is_reported() {
        let (mut w, t0) = watch();
        assert_eq!(w.on_state(t0, PracticeState::Dialog), Next::Stop);
        assert_eq!(w.stats.dialog, 1);
        let (mut w, t0) = watch();
        assert_eq!(w.on_state(t0 + Duration::from_secs(5), PracticeState::Running), Next::TimedOut);
    }

    #[test]
    fn idle_with_scripts_on_the_stack_twice_is_halted() {
        let (mut w, t0) = watch();
        assert_eq!(w.on_state(t0, PracticeState::Idle), Next::NeedIdleDetail);
        assert_eq!(w.on_idle_detail(t0, false, 1), IdleVerdict::PollAfter(ms(10)));
        assert_eq!(w.on_state(t0 + ms(10), PracticeState::Idle), Next::NeedIdleDetail);
        assert_eq!(w.on_idle_detail(t0 + ms(10), false, 1), IdleVerdict::Halted);
    }

    #[test]
    fn idle_at_an_empty_stack_waits_out_the_grace_then_vanishes() {
        let (mut w, t0) = watch();
        assert_eq!(w.on_state(t0, PracticeState::Idle), Next::NeedIdleDetail);
        assert_eq!(w.on_idle_detail(t0, false, 0), IdleVerdict::PollAfter(ms(10)));
        assert_eq!(w.on_state(t0 + ms(400), PracticeState::Idle), Next::NeedIdleDetail);
        assert_eq!(w.on_idle_detail(t0 + ms(400), false, 0), IdleVerdict::PollAfter(ms(10)));
        assert_eq!(w.on_idle_detail(t0 + ms(500), false, 0), IdleVerdict::Vanished);
    }

    #[test]
    fn the_runner_text_ends_both_paths_with_the_token() {
        let text = runner_cmm();
        let token_lines: Vec<_> =
            text.lines().enumerate().filter(|(_, l)| *l == "&t32ctl_done=\"&t32ctl_token\"").collect();
        assert_eq!(token_lines.len(), 2);
        for (i, _) in token_lines {
            assert_eq!(text.lines().nth(i + 1), Some("ENDDO"));
        }
        assert!(text.lines().any(|l| l == "t32ctl_failed:"), "labels start in column 1");
    }
}
