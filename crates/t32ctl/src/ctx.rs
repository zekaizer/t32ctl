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

use std::{collections::HashMap, io::Write, marker::PhantomData, path::Path, time::Duration};

use t32rcl::{
    AccessClass, Cancel, CancelReason, ConnectError, ConnectOpts, Endpoint, Error, FuncValue, Health, MemAddr,
    PracticeState, Session, TargetState,
};
use t32stage::{
    CleanupReport, ExistsFn, Ledger, OpenError, RegName, Stage, StageConfig, Undo, practice::fnc, replay_leftovers,
};

use crate::{
    error::{Context, CtlError, ErrorKind, from_connect, from_rcl, from_stage},
    grade::{Grade, Raw},
    notation::{Place, PracticeText, Size},
    profile::{
        Profile, RegionKind,
        policy::{Admitted, Piece, Segment, effective_memory},
    },
    report::{
        At, Body, CmdBody, CpuMatch, Header, Hex, Notice, Practice, PvStatus, Reg64, RegValue, RegsBody, TargetView,
    },
    script::{self, Prepared},
};

/// What a call does when the connected CPU is not the profile's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuCheck {
    /// `cpu-mismatch`: wrong board or wrong profile.
    Refuse,
    /// A notice; only `read status`, which shows both CPUs.
    Report,
    /// No check: a procedure that declares `debugger-config` may select the CPU.
    Skip,
}

/// One PowerView connection for the length of a call.
#[derive(Debug)]
pub struct Conn {
    session: Session,
    /// The candidate host that attached.
    host: String,
    /// `SYStem.CPU()` as the prelude read it; `None` while the target is down or unchecked.
    cpu: Option<String>,
    /// The call's cancel, restored after a bounded transfer.
    cancel: Cancel,
    notices: Vec<Notice>,
    /// How the staging layer opens when a leaf first needs it.
    stage_cfg: Option<StageConfig>,
    stage: Option<Stage>,
    /// What a failed `Stage::open` registered; cleanup runs it.
    open_cleanup: Option<Ledger>,
    /// The run token a scripted conversation expects.
    #[cfg(test)]
    stage_token: Option<String>,
}

/// What cleanup at the end of a call left.
#[derive(Debug, Default)]
pub struct Closed {
    /// Steps that ended clean.
    pub clean: usize,
    /// Our script could not be unwound: `residue`, which overrides exit 0 and 1.
    pub residue: Option<CtlError>,
}

impl Conn {
    /// Each host in order; the first that attaches is the connection. An interrupt stops at once.
    pub(crate) fn connect(hosts: &[String], port: u16, opts: &ConnectOpts) -> Result<Self, CtlError> {
        let mut failed: Vec<ConnectError> = Vec::new();
        for host in hosts {
            match Session::connect(Endpoint::new(host, port), opts.clone()) {
                Ok(session) => {
                    return Ok(Self {
                        session,
                        host: host.clone(),
                        cpu: None,
                        cancel: opts.cancel.clone(),
                        notices: Vec::new(),
                        stage_cfg: None,
                        stage: None,
                        open_cleanup: None,
                        #[cfg(test)]
                        stage_token: None,
                    });
                }
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
        if run == TargetState::Down || check == CpuCheck::Skip {
            return Ok(run);
        }
        let cpu = system_cpu(&mut self.session, Context::Stage)?;
        let want = &profile.target.cpu;
        if !cpu.eq_ignore_ascii_case(want) {
            let text = format!("connected CPU {cpu} is not the profile's {want}");
            match check {
                CpuCheck::Refuse => return Err(CtlError::new(ErrorKind::CpuMismatch, text)),
                CpuCheck::Report => self.notices.push(Notice::new(format!("{text}: every other command refuses"))),
                CpuCheck::Skip => {}
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

    /// How the stage opens if a leaf needs it.
    pub(crate) fn set_stage(&mut self, cfg: StageConfig) {
        self.stage_cfg = Some(cfg);
    }

    /// The run token of a scripted conversation.
    #[cfg(test)]
    pub(crate) fn set_stage_token(&mut self, token: Option<String>) {
        self.stage_token = token;
    }

    /// Run a prepared bundle, opening the stage first if no step of this call opened it.
    fn run_bundle(&mut self, run: Prepared) -> Result<Body, CtlError> {
        if self.stage.is_none() {
            let cfg = self.stage_cfg.clone().expect("a connecting call configures the stage");
            match Stage::open(&mut self.session, cfg) {
                #[cfg_attr(not(test), allow(unused_mut))]
                Ok(mut st) => {
                    #[cfg(test)]
                    if let Some(t) = &self.stage_token {
                        st.set_token(t.clone());
                    }
                    self.stage = Some(st);
                }
                Err(e) => {
                    let OpenError { error, cleanup } = *e;
                    self.open_cleanup = Some(cleanup);
                    return Err(from_stage(&error, "opening the stage"));
                }
            }
        }
        let stage = self.stage.as_mut().expect("the stage is open");
        script::execute(&mut self.session, stage, &self.cancel, run)
    }

    /// Run the stage's cleanup under a fresh 30 s deadline, not tied to Ctrl-C, reviving a dead
    /// link once. Residue becomes notices; our own script left on the stack is the call's error.
    pub(crate) fn close(&mut self) -> Closed {
        if self.stage.is_none() && self.open_cleanup.is_none() {
            return Closed::default();
        }
        let deadline = self.session.now() + CLEANUP;
        self.session.set_cancel(Cancel::new().with_deadline(deadline));
        let mut report = CleanupReport::default();
        if let Some(ledger) = self.open_cleanup.take() {
            report.extend(ledger.run(&mut self.session));
        }
        if let Some(stage) = self.stage.take() {
            report.extend(stage.close(&mut self.session));
        }
        self.notices.extend(residue_notices(&report));
        let unwind = report.residue().find(|i| i.what == Undo::UnwindPractice.describe());
        let residue = unwind.map(|i| {
            CtlError::new(
                ErrorKind::Residue,
                format!(
                    "the script t32ctl started could not be unwound ({}); in PowerView type: END (END also closes \
                     every dialog and OPEN file)",
                    i.outcome.label()
                ),
            )
        });
        Closed { clean: report.items.iter().filter(|i| i.outcome.is_clean()).count(), residue }
    }
}

/// The deadline of a call's cleanup.
const CLEANUP: Duration = Duration::from_secs(30);

/// A count of what replay cleaned, and one line per command a human must type for the rest.
fn replay_notices(report: &CleanupReport) -> Vec<Notice> {
    let mut v = Vec::new();
    let clean = report.items.iter().filter(|i| i.outcome.is_clean()).count();
    if clean > 0 {
        let steps = if clean == 1 { "step" } else { "steps" };
        v.push(Notice::new(format!("cleaned up after an interrupted call: {clean} {steps}")));
    }
    v.extend(residue_notices(report));
    v
}

/// One line per command a human must type for what cleanup could not do.
fn residue_notices(report: &CleanupReport) -> Vec<Notice> {
    let mut v = Vec::new();
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

/// The translation granule `read mem` assumes: no page is smaller.
const PAGE: u64 = 0x1000;
/// `read mem --to` writes in blocks of this size.
const BLOCK: u64 = 1 << 20;

impl MemPlan {
    /// The address object of a segment's read, `from` bytes in. ASSUMED(target-read): PowerView
    /// honors the access width of an `mmio` segment.
    fn read_addr(&self, seg: &Segment, mmio: bool, from: u64) -> MemAddr {
        let at = MemAddr::new(self.read_class.clone(), self.addr + seg.offset + from);
        if mmio { at.with_width(u16::from(self.width)) } else { at }
    }
}

/// Why a stream stopped early.
enum Halt {
    /// The bound passed after this many bytes were written.
    Deadline(u64),
    Failed(CtlError),
}

/// Every segment of `plan` into `sink`, `BLOCK` bytes per read; the bytes written.
fn stream(s: &mut Session, plan: &MemPlan, sink: &mut dyn Write) -> Result<u64, Halt> {
    let mut written = 0;
    for (seg, mmio) in &plan.segments {
        let mut from = 0;
        while from < seg.len {
            let n = (seg.len - from).min(BLOCK);
            let at = plan.read_addr(seg, *mmio, from);
            let bytes = match s.mem_read(&at, n as usize) {
                Ok(b) => b,
                Err(Error::Cancelled { reason: CancelReason::Deadline, .. }) => return Err(Halt::Deadline(written)),
                Err(e) => return Err(Halt::Failed(read_error(&e, &at))),
            };
            sink.write_all(&bytes).map_err(|e| {
                Halt::Failed(CtlError::new(ErrorKind::StateDir, format!("writing the output file: {e}")))
            })?;
            written += n;
            from += n;
        }
    }
    Ok(written)
}

fn read_error(e: &Error, at: &MemAddr) -> CtlError {
    from_rcl(e, Context::Read, &format!("read of {at}"))
}

/// `E<class>`, the access class for memory while the cores run.
fn running_class(class: &AccessClass) -> Result<AccessClass, CtlError> {
    if class.as_str().starts_with('E') {
        return Ok(class.clone());
    }
    AccessClass::parse(&format!("E{}", class.as_str())).map_err(|_| {
        let message = format!("access class {} has no E form for a running target", class.as_str());
        CtlError::new(ErrorKind::MemoryPolicy, message)
    })
}

/// The physical address of `class:page`. A refusal is `memory-policy`: without a translation the
/// map cannot be checked.
fn translate(s: &mut Session, class: &AccessClass, page: u64) -> Result<u64, CtlError> {
    let expr = fnc::physical(&MemAddr::new(class.clone(), page));
    let v = s.eval(&expr).map_err(|e| {
        let err = from_rcl(&e, Context::Read, &expr);
        if err.kind != ErrorKind::Refused {
            return err;
        }
        let message = format!("no physical address for {}:{page:#x}: {}", class.as_str(), err.message);
        CtlError::new(ErrorKind::MemoryPolicy, message)
    })?;
    let phys = match &v {
        FuncValue::Uint(n) => Some(*n),
        FuncValue::Text(t) | FuncValue::Raw { text: t, .. } => physical_text(t),
        _ => None,
    };
    phys.ok_or_else(|| CtlError::new(ErrorKind::PvError, format!("{expr}: {v} names no physical address")))
}

/// The hex number after the last `:` of an address value (`A:0x40107000`).
fn physical_text(t: &str) -> Option<u64> {
    let last = t.rsplit(':').next()?.trim();
    let digits = last.strip_prefix("0x").or_else(|| last.strip_prefix("0X"))?;
    u64::from_str_radix(digits, 16).ok()
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

    /// Run a procedure the grade check admitted at this grade. It runs whatever the target state:
    /// an `attach` procedure brings the target up.
    pub fn run_procedure(&mut self, adm: Admitted<G>, run: Prepared) -> Result<Body, CtlError> {
        let _admitted = adm;
        self.conn.run_bundle(run)
    }

    /// The host that attached.
    pub fn host(&self) -> &str {
        &self.conn.host
    }

    /// The bytes at `req`, after the memory map allowed every one of them.
    pub fn read_mem(&mut self, req: &MemRequest) -> Result<(MemPlan, Vec<u8>), CtlError> {
        let plan = self.plan_mem(req)?;
        let mut data = Vec::with_capacity(plan.len as usize);
        for (seg, mmio) in &plan.segments {
            let at = plan.read_addr(seg, *mmio, 0);
            let bytes = self.conn.session.mem_read(&at, seg.len as usize).map_err(|e| read_error(&e, &at))?;
            data.extend_from_slice(&bytes);
        }
        Ok((plan, data))
    }

    /// The bytes at `req` into `sink` in blocks, until done or until `bound` passes. The memory map
    /// is checked before the bound starts.
    pub fn read_mem_to(
        &mut self,
        req: &MemRequest,
        bound: Duration,
        sink: &mut dyn Write,
    ) -> Result<(MemPlan, Transfer), CtlError> {
        let plan = self.plan_mem(req)?;
        let base = self.conn.cancel.clone();
        let s = &mut self.conn.session;
        let t0 = s.now();
        s.set_cancel(base.with_deadline(t0 + bound));
        let streamed = stream(s, &plan, sink);
        s.set_cancel(base);
        let after_ms = u64::try_from((s.now() - t0).as_millis()).unwrap_or(u64::MAX);
        let transfer = match streamed {
            Ok(bytes) => Transfer::Done { bytes, after_ms },
            Err(Halt::Deadline(bytes)) => {
                Transfer::Stopped { bytes, bound_ms: u64::try_from(bound.as_millis()).unwrap_or(u64::MAX) }
            }
            Err(Halt::Failed(e)) => return Err(e),
        };
        Ok((plan, transfer))
    }

    /// Resolve the place, translate it page by page unless its class is `A`, and check every
    /// physical byte against the memory map before any memory request.
    fn plan_mem(&mut self, req: &MemRequest) -> Result<MemPlan, CtlError> {
        if self.run == TargetState::Down {
            return Err(CtlError::new(ErrorKind::TargetDown, "the target is down: memory needs an attached target"));
        }
        let running = self.run == TargetState::Running;
        let policy = effective_memory(self.profile);
        let (typed, addr) = match &req.place {
            Place::Addr { class, addr } => (class.as_ref(), *addr),
            Place::Reg { reg, offset } => {
                if running {
                    return Err(CtlError::new(
                        ErrorKind::NotHalted,
                        "the target is running: @reg places need a stopped target",
                    ));
                }
                let base = register(&mut self.conn.session, reg)?;
                let addr = base.checked_add_signed(*offset).ok_or_else(|| {
                    let message = format!("{}: the offset takes {reg} = {base:#x} outside 64 bits", req.place);
                    CtlError::new(ErrorKind::Notation, message)
                })?;
                (None, addr)
            }
        };
        let class = policy.class(typed)?;
        let mut pages = HashMap::new();
        let len = match req.len {
            Some(len) => len,
            None => {
                let first = self.pieces(&class, addr, 1, &mut pages)?[0].phys;
                u64::from(req.width.unwrap_or_else(|| policy.cell_width(first)))
            }
        };
        let pieces = self.pieces(&class, addr, len, &mut pages)?;
        let checked = policy.check(&pieces, running, req.width)?;
        // ASSUMED(target-read): `E` before the class reads memory while the cores run.
        let read_class = if running { running_class(&class)? } else { class.clone() };
        let regions = &policy.regions;
        Ok(MemPlan {
            default_class: class == policy.default_class,
            phys: (class.as_str() != "A").then(|| pieces.first().map(|p| p.phys)).flatten(),
            regions: checked.regions.iter().map(|&i| (regions[i].name.clone(), regions[i].kind)).collect(),
            segments: checked.segments.iter().map(|s| (*s, regions[s.region].kind == RegionKind::Mmio)).collect(),
            class,
            addr,
            len,
            width: checked.width,
            read_class,
        })
    }

    /// `len` bytes from `addr` as physical runs: one for class `A`, else one per translated 4 KiB
    /// page, merged where pages are physically contiguous. `pages` keeps translations across the
    /// calls of one plan.
    fn pieces(
        &mut self,
        class: &AccessClass,
        addr: u64,
        len: u64,
        pages: &mut HashMap<u64, u64>,
    ) -> Result<Vec<Piece>, CtlError> {
        let Some(end) = addr.checked_add(len) else {
            let message = format!("{addr:#x} + {}: past the end of the address space", Size(len));
            return Err(CtlError::new(ErrorKind::Notation, message));
        };
        if class.as_str() == "A" {
            return Ok(vec![Piece { offset: 0, phys: addr, len }]);
        }
        let mut out: Vec<Piece> = Vec::new();
        let mut at = addr;
        while at < end {
            let page = at & !(PAGE - 1);
            let next = page.saturating_add(PAGE).min(end);
            let phys_page = match pages.get(&page) {
                Some(p) => *p,
                None => {
                    let p = translate(&mut self.conn.session, class, page)?;
                    pages.insert(page, p);
                    p
                }
            };
            let phys = phys_page.checked_add(at - page).ok_or_else(|| {
                CtlError::new(ErrorKind::PvError, format!("{}:{page:#x} translates past 64 bits", class.as_str()))
            })?;
            let piece = Piece { offset: at - addr, phys, len: next - at };
            match out.last_mut() {
                Some(last) if last.phys + last.len == phys => last.len += piece.len,
                _ => out.push(piece),
            }
            at = next;
        }
        Ok(out)
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

/// What `read mem` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemRequest {
    pub place: Place,
    /// `None`: one cell at the access width.
    pub len: Option<u64>,
    pub width: Option<u8>,
}

/// A read the memory map allowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemPlan {
    pub class: AccessClass,
    /// The class is the profile's default class.
    pub default_class: bool,
    pub addr: u64,
    pub len: u64,
    pub width: u8,
    /// The physical address of the first byte, for a translated class.
    pub phys: Option<u64>,
    /// The regions the read touches, in order: name and kind.
    pub regions: Vec<(String, RegionKind)>,
    /// The class the reads use: `E` before it while the target runs.
    read_class: AccessClass,
    /// Each segment, and whether its region is `mmio`.
    segments: Vec<(Segment, bool)>,
}

/// How a bounded transfer ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Done {
        bytes: u64,
        after_ms: u64,
    },
    /// The bound passed; `bytes` were written before it.
    Stopped {
        bytes: u64,
        bound_ms: u64,
    },
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

    /// A script bundle from the repository, outside every policy.
    pub fn run_script(&mut self, run: Prepared) -> Result<Body, CtlError> {
        self.conn.run_bundle(run)
    }
}
