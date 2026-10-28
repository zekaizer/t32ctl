// SPDX-License-Identifier: Apache-2.0
//! The connection of one call and the grade-typed context a leaf works through. The `Session`
//! is private to this module: a leaf reaches PowerView only through `Ctx<G>`, whose operations
//! live in impl blocks bounded by grade.
//!
//! A leaf cannot build its own context:
//!
//! ```compile_fail,E0624
//! use t32ctl::{ctx::{Conn, Ctx}, grade::Raw, profile::Profile};
//! fn forge<'c>(conn: &'c mut Conn, p: &'c Profile) -> Ctx<'c, Raw> {
//!     Ctx::new(conn, t32rcl::TargetState::Stopped, p)
//! }
//! ```
//!
//! PRACTICE text is for `raw` alone, so a `read` leaf cannot send a command and a `burn` leaf
//! cannot evaluate a function:
//!
//! ```compile_fail,E0599
//! use t32ctl::{ctx::Ctx, grade::Read, notation::PracticeText};
//! fn send(ctx: &mut Ctx<'_, Read>, text: &PracticeText) {
//!     let _ = ctx.cmd(text);
//! }
//! ```
//!
//! ```compile_fail,E0599
//! use t32ctl::{ctx::Ctx, grade::Burn, notation::PracticeText};
//! fn send(ctx: &mut Ctx<'_, Burn>, text: &PracticeText) {
//!     let _ = ctx.eval(text);
//! }
//! ```
//!
//! The same calls compile under `raw`:
//!
//! ```
//! use t32ctl::{ctx::Ctx, grade::Raw, notation::PracticeText};
//! fn send(ctx: &mut Ctx<'_, Raw>, text: &PracticeText) {
//!     let _ = ctx.cmd(text);
//!     let _ = ctx.eval(text);
//! }
//! ```

use std::{marker::PhantomData, path::Path};

use t32rcl::{ConnectError, ConnectOpts, Endpoint, FuncValue, Health, PracticeState, Session, TargetState};
use t32stage::{CleanupReport, ExistsFn, RegName, practice::fnc, replay_leftovers};

use crate::{
    error::{Context, CtlError, ErrorKind, from_connect, from_rcl},
    grade::{Grade, Raw},
    notation::PracticeText,
    profile::Profile,
    report::{At, CmdBody, CpuMatch, Header, Hex, Notice, Practice, PvStatus, Reg64, RegValue, RegsBody, TargetView},
};

/// What a call does when the connected CPU is not the profile's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuCheck {
    /// `cpu-mismatch`: wrong board or wrong profile.
    Refuse,
    /// A notice; only `read status`, which shows both CPUs.
    Report,
}

/// One PowerView connection for the length of a call.
#[derive(Debug)]
pub struct Conn {
    session: Session,
    /// The candidate host that attached.
    host: String,
    /// `SYStem.CPU()` as the prelude read it; `None` while the target is down.
    cpu: Option<String>,
    notices: Vec<Notice>,
}

impl Conn {
    /// Each host in order; the first that attaches is the connection. An interrupt stops at once.
    pub(crate) fn connect(hosts: &[String], port: u16, opts: &ConnectOpts) -> Result<Self, CtlError> {
        let mut failed: Vec<ConnectError> = Vec::new();
        for host in hosts {
            match Session::connect(Endpoint::new(host, port), opts.clone()) {
                Ok(session) => return Ok(Self { session, host: host.clone(), cpu: None, notices: Vec::new() }),
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

    /// Replay the journals dead calls left in `pv_dir`, read the run state, and unless the target
    /// is down compare `SYStem.CPU()` with the profile's CPU.
    pub(crate) fn prelude(
        &mut self,
        pv_dir: &Path,
        profile: &Profile,
        check: CpuCheck,
    ) -> Result<TargetState, CtlError> {
        let replayed = replay_leftovers(&mut self.session, pv_dir, ExistsFn::OsFileReadable);
        self.notices.extend(replay_notices(&replayed));
        let run = self.session.target_state().map_err(|e| from_rcl(&e, Context::Read, "T32_GetState"))?;
        if run == TargetState::Down {
            return Ok(run);
        }
        let cpu = system_cpu(&mut self.session, Context::Stage)?;
        let want = &profile.target.cpu;
        if !cpu.eq_ignore_ascii_case(want) {
            let text = format!("connected CPU {cpu} is not the profile's {want}");
            match check {
                CpuCheck::Refuse => return Err(CtlError::new(ErrorKind::CpuMismatch, text)),
                CpuCheck::Report => self.notices.push(Notice::new(format!("{text}: every other command refuses"))),
            }
        }
        self.cpu = Some(cpu);
        Ok(run)
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

fn system_cpu(s: &mut Session, context: Context) -> Result<String, CtlError> {
    s.eval(fnc::SYSTEM_CPU)
        .and_then(|v| v.into_text(fnc::SYSTEM_CPU))
        .map_err(|e| from_rcl(&e, context, fnc::SYSTEM_CPU))
}

/// `Register(<NAME>)` as an unsigned value; a refusal names the register.
fn register(s: &mut Session, name: &RegName) -> Result<u64, CtlError> {
    let expr = fnc::register(name);
    let v = s.eval(&expr).map_err(|e| from_rcl(&e, Context::Read, &expr))?;
    let shown = v.to_string();
    as_u64(v).ok_or_else(|| CtlError::new(ErrorKind::PvError, format!("{expr}: not a register value: {shown}")))
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
    profile: &'c Profile,
    _grade: PhantomData<G>,
}

impl<'c, G: Grade> Ctx<'c, G> {
    pub(crate) fn new(conn: &'c mut Conn, run: TargetState, profile: &'c Profile) -> Self {
        Self { conn, run, profile, _grade: PhantomData }
    }

    /// The run state the call found when it connected.
    pub fn run_state(&self) -> TargetState {
        self.run
    }

    /// The host that attached.
    pub fn host(&self) -> &str {
        &self.conn.host
    }

    /// Registers of PowerView's selected core, in the order named. A down or running target is
    /// refused before any `Register()`, which would fail with "target system down".
    pub fn read_regs(&mut self, names: &[RegName]) -> Result<RegsBody, CtlError> {
        self.require_stopped("registers")?;
        let s = &mut self.conn.session;
        let core = s.eval(fnc::CORE).and_then(|v| v.into_int(fnc::CORE)).ok();
        let mut regs = Vec::with_capacity(names.len());
        for name in names {
            let value = register(s, name)?;
            regs.push(RegValue { name: name.to_string(), value: Reg64(value) });
        }
        Ok(RegsBody { core, regs })
    }

    /// Exit 3 unless the run state the call found allows target access: down is `target-down`,
    /// running is `not-halted`.
    fn require_stopped(&self, what: &str) -> Result<(), CtlError> {
        match self.run {
            TargetState::Down => {
                Err(CtlError::new(ErrorKind::TargetDown, format!("the target is down: {what} need an attached target")))
            }
            TargetState::Running => {
                Err(CtlError::new(ErrorKind::NotHalted, format!("the target is running: {what} need a stopped target")))
            }
            _ => Ok(()),
        }
    }

    /// The target against the profile, and PowerView's build, RCL clients and PRACTICE state.
    /// While the target is down the prelude did not read the CPU, so this reads it for display.
    pub fn status(&mut self) -> Result<(TargetView, PvStatus), CtlError> {
        let t = &self.profile.target;
        let s = &mut self.conn.session;
        let (cpu, cpu_check) = match &self.conn.cpu {
            None => (system_cpu(s, Context::Read)?, CpuMatch::Skipped),
            Some(c) if c.eq_ignore_ascii_case(&t.cpu) => (c.clone(), CpuMatch::Match),
            Some(c) => (c.clone(), CpuMatch::Mismatch),
        };
        let cores = s
            .eval(fnc::CORE_NUMBER)
            .and_then(|v| v.into_int(fnc::CORE_NUMBER))
            .map_err(|e| from_rcl(&e, Context::Read, fnc::CORE_NUMBER))?;
        let target = TargetView {
            name: t.name.clone(),
            cpu,
            profile_cpu: t.cpu.clone(),
            cpu_check,
            cores,
            profile_cores: t.cores,
        };
        let build = s.build().map_err(|e| from_rcl(&e, Context::Read, "SOFTWARE.BUILD()"))?;
        let clients = nr_used_cons(s)?;
        let (practice, depth) = practice(s)?;
        Ok((target, PvStatus { build: build.build, base: build.base, clients, practice, depth }))
    }
}

/// The PRACTICE state and stack depth; an idle state with a non-empty stack is a halted script.
fn practice(s: &mut Session) -> Result<(Practice, i64), CtlError> {
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
    Ok((practice, depth))
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

/// PRACTICE text the caller typed: only a `raw` leaf gets these. The text goes out as typed, also
/// while the target is down; PowerView's answer decides.
impl Ctx<'_, Raw> {
    /// One command, then the PRACTICE state it left. A `DO` returns while its script runs, so a
    /// script still on the stack is a notice.
    pub fn cmd(&mut self, text: &PracticeText) -> Result<CmdBody, CtlError> {
        let s = &mut self.conn.session;
        s.cmd(text.as_str()).map_err(|e| {
            let err = from_rcl(&e, Context::Text, text.as_str());
            if err.kind != ErrorKind::NoAnswer {
                return err;
            }
            // A blocking command (`WAIT`) holds PowerView past the call budget.
            err.with_details(vec![
                "PowerView may still run the command, and it accepts no new connection until the command ends".into(),
            ])
        })?;
        let (practice, depth) = practice(s)?;
        if depth > 0 {
            self.conn.notices.push(Notice::new(format!(
                "a PRACTICE script is on the stack after this command (depth {depth}, {practice}); proc and raw \
                 script calls wait for it"
            )));
        }
        Ok(CmdBody { practice, depth })
    }

    /// One function.
    pub fn eval(&mut self, text: &PracticeText) -> Result<FuncValue, CtlError> {
        self.conn.session.eval(text.as_str()).map_err(|e| from_rcl(&e, Context::Text, text.as_str()))
    }
}
