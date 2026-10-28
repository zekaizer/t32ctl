// SPDX-License-Identifier: Apache-2.0
//! The connection of one call and the grade-typed context a leaf works through. The `Session`
//! is private to this module: a leaf reaches PowerView only through `Ctx<G>`, whose operations
//! live in impl blocks bounded by grade.
//!
//! A leaf cannot build its own context:
//!
//! ```compile_fail,E0624
//! fn forge(conn: &mut t32ctl::ctx::Conn) -> t32ctl::ctx::Ctx<'_, t32ctl::grade::Raw> {
//!     t32ctl::ctx::Ctx::new(conn, t32rcl::TargetState::Stopped)
//! }
//! ```

use std::{marker::PhantomData, path::Path};

use t32rcl::{ConnectError, ConnectOpts, Endpoint, FuncValue, Health, PracticeState, Session, TargetState};
use t32stage::{CleanupReport, ExistsFn, practice::fnc, replay_leftovers};

use crate::{
    error::{Context, CtlError, ErrorKind, from_connect, from_rcl},
    grade::Grade,
    report::{At, Header, Hex, Notice, Practice, PvStatus},
};

/// One PowerView connection for the length of a call.
#[derive(Debug)]
pub struct Conn {
    session: Session,
    /// The candidate host that attached.
    host: String,
    notices: Vec<Notice>,
}

impl Conn {
    /// Each host in order; the first that attaches is the connection. An interrupt stops at once.
    pub(crate) fn connect(hosts: &[String], port: u16, opts: &ConnectOpts) -> Result<Self, CtlError> {
        let mut failed: Vec<ConnectError> = Vec::new();
        for host in hosts {
            match Session::connect(Endpoint::new(host, port), opts.clone()) {
                Ok(session) => return Ok(Self { session, host: host.clone(), notices: Vec::new() }),
                Err(e @ ConnectError::Cancelled { .. }) => return Err(from_connect(&e)),
                Err(e) => failed.push(e),
            }
        }
        let Some(first) = failed.first() else {
            return Err(CtlError::new(ErrorKind::Unreachable, "no PowerView host to try"));
        };
        // A candidate that accepted TCP and stayed silent is a PowerView: report that one.
        let telling = failed.iter().find(|e| matches!(e, ConnectError::NoAnswer { .. })).unwrap_or(first);
        let mut err = from_connect(telling);
        if failed.len() > 1 {
            let tried: Vec<String> = failed.iter().map(|e| format!("{} ({})", e.endpoint(), e.tag())).collect();
            err.details.push(format!("tried {}", tried.join(", ")));
        }
        Err(err)
    }

    /// Replay the journals dead calls left in `pv_dir`, then read the run state.
    pub(crate) fn prelude(&mut self, pv_dir: &Path) -> Result<TargetState, CtlError> {
        let replayed = replay_leftovers(&mut self.session, pv_dir, ExistsFn::OsFileReadable);
        self.notices.extend(replay_notices(&replayed));
        self.session.target_state().map_err(|e| from_rcl(&e, Context::Read, "T32_GetState"))
    }

    /// Where the target is now, best effort: a refused or failed read shortens or drops it.
    pub(crate) fn header(&mut self) -> Option<Header> {
        if self.session.health() != Health::Live {
            return None;
        }
        Some(match self.session.target_state().ok()? {
            TargetState::Down => Header::Down,
            TargetState::Stopped => {
                let core = self.session.eval(fnc::CORE).and_then(|v| v.into_int(fnc::CORE)).ok();
                let pc = t32stage::RegName::new("pc").expect("pc is a register name");
                let at = self.session.eval(&fnc::register(&pc)).ok().and_then(as_u64).map(|pc| At { pc: Hex(pc) });
                Header::Stopped { core, at }
            }
            TargetState::Running => {
                let cores = self.session.eval(fnc::CORE_NUMBER).and_then(|v| v.into_int(fnc::CORE_NUMBER)).ok();
                Header::Running { cores }
            }
            TargetState::Halted => Header::Other { state: 1 },
            TargetState::Other(state) => Header::Other { state },
        })
    }

    pub(crate) fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }
}

/// A count of what replay cleaned, and one line per command a human must type for the rest.
fn replay_notices(report: &CleanupReport) -> Vec<Notice> {
    let mut v = Vec::new();
    let clean = report.items.iter().filter(|i| i.outcome.is_clean()).count();
    if clean > 0 {
        let steps = if clean == 1 { "step" } else { "steps" };
        v.push(Notice::new(format!("cleaned up after an interrupted call: {clean} {steps}")));
    }
    for item in report.residue() {
        match &item.manual {
            Some(commands) => v.extend(commands.lines().map(|c| {
                let end = if c == t32stage::practice::cmd::END {
                    " (END also closes every dialog and OPEN file)"
                } else {
                    ""
                };
                Notice::new(format!("residue: {}; in PowerView type: {c}{end}", item.what))
            })),
            None => v.push(Notice::new(format!("residue: {}: {}", item.what, item.outcome.label()))),
        }
    }
    v
}

/// An address in any of the integer forms PowerView answers with.
fn as_u64(v: FuncValue) -> Option<u64> {
    match v {
        FuncValue::Uint(n) => Some(n),
        FuncValue::Int(n) => u64::try_from(n).ok(),
        _ => None,
    }
}

/// What a leaf under grade `G` may do to PowerView.
#[derive(Debug)]
pub struct Ctx<'c, G: Grade> {
    conn: &'c mut Conn,
    run: TargetState,
    _grade: PhantomData<G>,
}

impl<'c, G: Grade> Ctx<'c, G> {
    pub(crate) fn new(conn: &'c mut Conn, run: TargetState) -> Self {
        Self { conn, run, _grade: PhantomData }
    }

    /// The run state the call found when it connected.
    pub fn run_state(&self) -> TargetState {
        self.run
    }

    /// The host that attached.
    pub fn host(&self) -> &str {
        &self.conn.host
    }

    /// PowerView's build, RCL clients and PRACTICE state.
    pub fn status(&mut self) -> Result<PvStatus, CtlError> {
        let s = &mut self.conn.session;
        let build = s.build().map_err(|e| from_rcl(&e, Context::Read, "SOFTWARE.BUILD()"))?;
        let clients = nr_used_cons(s)?;
        let state = s.practice_state().map_err(|e| from_rcl(&e, Context::Read, "T32_GetPracticeState"))?;
        let depth = s
            .eval(fnc::STACK_DEPTH)
            .and_then(|v| v.into_int(fnc::STACK_DEPTH))
            .map_err(|e| from_rcl(&e, Context::Read, fnc::STACK_DEPTH))?;
        let practice = match state {
            PracticeState::Idle if depth > 0 => Practice::Halted,
            PracticeState::Idle => Practice::Idle,
            PracticeState::Running => Practice::Running,
            PracticeState::Dialog => Practice::Dialog,
            PracticeState::Other(n) => Practice::Other(n),
        };
        Ok(PvStatus { build: build.build, base: build.base, clients, practice, depth })
    }
}

/// The RCL client count; `None` when PowerView refuses every spelling of the function.
fn nr_used_cons(s: &mut Session) -> Result<Option<i64>, CtlError> {
    for expr in fnc::NR_USED_CONS {
        match s.eval(expr).and_then(|v| v.into_int(expr)) {
            Ok(n) => return Ok(Some(n)),
            Err(e) if e.remote().is_some() => {}
            Err(e) => return Err(from_rcl(&e, Context::Read, expr)),
        }
    }
    Ok(None)
}
