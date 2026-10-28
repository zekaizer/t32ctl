// SPDX-License-Identifier: Apache-2.0
//! Opt-in probes. `registry::select` refuses each one without its gate, and the gate carries
//! the typed input the probe needs, so these bodies may assume consent.

use std::{
    collections::BTreeMap,
    thread,
    time::{Duration, Instant},
};

use t32rcl::{AccessClass, CallBudget, ConnectError, Endpoint, ErrorClass, MemAddr, RemoteKind, Session, TargetState};
use t32stage::{
    Put, StagePath, Undo, VmRegion,
    practice::{cmd, fnc},
    transfer,
};

use super::{
    functions::command,
    hex,
    lab::{Case, Ending, Expect, Lab, LabSizes, Seen, Until, eval_text},
    nr_used_cons, ok, scratch_regions, soft, stage_root, workdir,
};
use crate::{
    cli::SourceLine,
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::{SkipReason, Value},
    stats::Stats,
};

/// The dialog case: `h` before the modal command, `z` only after it returned.
pub(crate) fn dlg() -> Case {
    let body = "@a\n@h\nDIALOG.OK \"t32probe: automatic test, this dialog closes itself\"\n@z\nENDDO";
    Case::new("dlg", body, Expect::End, Duration::from_secs(15))
}

/// A modal `DIALOG.OK` in a Lab script: the client must see state 2, send `T32_Stop`, and end
/// only its own script. Shows a window on the operator's screen.
pub fn dialog(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "dlg", LabSizes::default())?;
    let result = dialog_case(ctx, &mut lab);
    let closed = lab.close(ctx);
    result.and(closed)
}

fn dialog_case(ctx: &mut ProbeCtx<'_>, lab: &mut Lab) -> ProbeResult {
    ctx.measure("operator", Value::Text("check on the PowerView PC that the dialog window closed".into()));
    let Some(mut run) = lab.begin(ctx, &dlg())? else {
        return Err(ProbeFail::Check("the dialog case did not start".into()));
    };
    let state2 = lab.wait(ctx, &mut run, Until::Dialog)? == Seen::End(Ending::Dialog);
    let (mut stop, mut after) = ("-".to_string(), "-".to_string());
    if state2 {
        let (token, took) = lab.stop(ctx, &mut run)?;
        ctx.measure("stop_ms", Value::Seconds(took.as_secs_f64()));
        stop = token;
        after = lab.observe(ctx, &mut run, Duration::from_millis(300))?;
        lab.end(ctx, 3)?;
    }
    let rec = lab.finish(ctx, run)?;
    ctx.text("state2", if state2 { "seen" } else { "never" });
    ctx.text("stop", stop);
    ctx.text("after", after);
    ctx.text("stack_clean", if rec.depth == 0 { "yes" } else { "NO" });
    ctx.text("z", if rec.has("z") { "yes" } else { "no" });
    ctx.check(state2 && rec.depth == 0, || {
        format!(
            "dialog case ended {}, stack depth {}",
            rec.ending.as_ref().map_or("-".into(), |(e, _)| e.token()),
            rec.depth
        )
    })
}

/// The script client A runs while client B attaches.
pub(crate) fn mw() -> Case {
    Case::new("mw", "@a\n@w\nWAIT 5.s\n@z\nENDDO", Expect::End, Duration::from_secs(15))
}

fn ms(d: Duration) -> String {
    d.as_millis().to_string()
}

/// `err:<tag>` of a refused attach; `err:rcl-locked` when PowerView answered that the API is locked.
fn attach_error(e: &ConnectError) -> String {
    match e {
        ConnectError::Rejected { remote, .. } if remote.kind == RemoteKind::Locked => "err:rcl-locked".into(),
        other => format!("err:{}", other.tag()),
    }
}

/// A second connection B: attach while A's `DO`'d script waits, B's latency, `NrUsedCons` from
/// both; then attach while A blocks in a `WAIT` command.
pub fn multi_client(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "mc", LabSizes::default())?;
    let result = multi_client_cases(ctx, &mut lab);
    let closed = lab.close(ctx);
    result.and(closed)
}

fn multi_client_cases(ctx: &mut ProbeCtx<'_>, lab: &mut Lab) -> ProbeResult {
    let endpoint = ctx.env.endpoint.clone().unwrap_or_else(|| Endpoint::new(&ctx.env.cfg.hosts[0], ctx.env.cfg.port));
    let opts = ctx.env.connect_opts();
    let mut attach_script = "-".to_string();
    if let Some(mut run) = lab.begin(ctx, &mw())? {
        if lab.wait(ctx, &mut run, Until::Marker("w"))? == Seen::Reached {
            lab.pause(Duration::from_millis(500));
            let t = Instant::now();
            match Session::connect(endpoint.clone(), opts.clone()) {
                Ok(mut b) => {
                    attach_script = ms(t.elapsed());
                    let mut lat = Vec::new();
                    for _ in 0..10 {
                        let t = Instant::now();
                        if soft(b.eval("TRUE()"))?.is_none() {
                            break;
                        }
                        lat.push(t.elapsed());
                    }
                    let b_lat =
                        Stats::from_samples(&mut lat).map_or("-".into(), |s| format!("{}/{}", ms(s.p50), ms(s.max)));
                    ctx.text("b_lat", b_lat);
                    let count = |r: Option<i64>| r.map_or("?".to_string(), |n| n.to_string());
                    let a = count(soft(nr_used_cons(ctx.session))?);
                    let bn = count(soft(nr_used_cons(&mut b))?);
                    ctx.text("cons", format!("{a}/{bn}"));
                }
                Err(e) => attach_script = attach_error(&e),
            }
        }
        lab.wait(ctx, &mut run, Until::Done)?;
        lab.finish(ctx, run)?;
    }
    ctx.text("attach_script", attach_script.clone());

    let session = &mut *ctx.session;
    let (b, a) = thread::scope(|s| {
        let b = s.spawn(|| {
            thread::sleep(Duration::from_secs(1));
            let t = Instant::now();
            let r = Session::connect(endpoint, opts).map(drop);
            (t.elapsed(), r)
        });
        let idle = Duration::from_secs(10);
        let a = session.with_budget(CallBudget::idle(idle).with_total(idle + Duration::from_secs(10))).cmd("WAIT 3.s");
        (b.join(), a)
    });
    let (took, b) = b.map_err(|_| ProbeFail::Check("client B thread panicked".into()))?;
    let attach_cmdwait = match b {
        Ok(()) => ms(took),
        Err(e) => attach_error(&e),
    };
    ctx.measure("cmdwait_a", Value::Text(soft(a)?.map_or("refused".into(), |()| "ok".into())));
    ctx.text("attach_cmdwait", attach_cmdwait);
    ctx.check(attach_script != "-", || "client B never tried to attach while A's script ran".into())
}

/// Target state polling: every `POLL`, at most `GO_POLLS` after a run command (5 s), then `Break`.
const POLL: Duration = Duration::from_millis(10);
const GO_POLLS: u32 = 500;
/// Samples of the target state while `Go` runs before the probe's own `Break` (1 s).
const RUN_POLLS: u32 = 100;
/// Room for a `STOre ... Break` script read back through `VM:`.
const STORE_LEN: u32 = 64 * 1024;
const GO: &str = "Go";

/// A pause between target polls; none in tests.
pub(crate) fn nap(ctx: &ProbeCtx<'_>, d: Duration) {
    #[cfg(test)]
    let factor = ctx.env.lab_timing.pause;
    #[cfg(not(test))]
    let factor = {
        let _ = ctx;
        1.0
    };
    thread::sleep(d.mul_f64(factor));
}

pub(crate) fn unverified(name: &str) -> String {
    format!("unverified:{name}")
}

/// Whether this build offers a target command or function, asked once per name with
/// `PRACTICE.CoMmanD.AVAILable` / `PRACTICE.FUNCtion.AVAILable` (ide_func). A name it does not
/// offer is recorded as `unverified.<name>` and its step is skipped.
#[derive(Debug, Default)]
pub(crate) struct Verified(BTreeMap<String, bool>);

impl Verified {
    pub fn command(&mut self, ctx: &mut ProbeCtx<'_>, name: &str) -> Result<bool, ProbeFail> {
        self.ask(ctx, "CoMmanD", name)
    }

    pub fn function(&mut self, ctx: &mut ProbeCtx<'_>, name: &str) -> Result<bool, ProbeFail> {
        self.ask(ctx, "FUNCtion", name)
    }

    fn ask(&mut self, ctx: &mut ProbeCtx<'_>, kind: &str, name: &str) -> Result<bool, ProbeFail> {
        if let Some(&v) = self.0.get(name) {
            return Ok(v);
        }
        let expr = format!("PRACTICE.{kind}.AVAILable({name})");
        let v = soft(ctx.session.eval(&expr).and_then(|v| v.into_bool(&expr)))?.unwrap_or(false);
        if !v {
            ctx.measure(&format!("unverified.{name}"), Value::Text(unverified(name)));
        }
        self.0.insert(name.to_string(), v);
        Ok(v)
    }
}

/// Apply the newest `Revert` named `what` now; `clean` turns false when it did not apply.
pub(crate) fn revert(ctx: &mut ProbeCtx<'_>, what: &str, clean: &mut bool) {
    let o = ctx.ledger.revert_now(ctx.session, what);
    *clean &= o.is_clean();
    ctx.measure(&format!("revert.{what}"), Value::Text(o.label()));
}

pub(crate) fn halted(ctx: &mut ProbeCtx<'_>) -> Result<bool, ProbeFail> {
    Ok(matches!(ctx.session.target_state()?, TargetState::Halted | TargetState::Stopped))
}

/// Poll up to `polls` times until the target halts.
pub(crate) fn wait_halt(ctx: &mut ProbeCtx<'_>, polls: u32) -> Result<bool, ProbeFail> {
    for i in 0..polls {
        if halted(ctx)? {
            return Ok(true);
        }
        if i + 1 < polls {
            nap(ctx, POLL);
        }
    }
    Ok(false)
}

/// How a run command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ran {
    Refused,
    Halted(Duration),
    /// Still running after 5 s; the probe sent `Break`.
    Broken(Duration),
}

impl Ran {
    pub fn token(self) -> String {
        match self {
            Ran::Refused => "refused".into(),
            Ran::Halted(d) => format!("halted@{}", ms(d)),
            Ran::Broken(d) => format!("break@{}", ms(d)),
        }
    }
}

/// A run command, up to 5 s for the target to halt, then `Break`. The caller registered
/// `BreakTarget` before.
pub(crate) fn run_until_halt(ctx: &mut ProbeCtx<'_>, command: &str) -> Result<Ran, ProbeFail> {
    let t = ctx.session.now();
    if soft(ctx.session.cmd(command))?.is_none() {
        return Ok(Ran::Refused);
    }
    if wait_halt(ctx, GO_POLLS)? {
        return Ok(Ran::Halted(ctx.session.now().saturating_duration_since(t)));
    }
    soft(ctx.session.cmd(cmd::BREAK))?;
    wait_halt(ctx, GO_POLLS)?;
    Ok(Ran::Broken(ctx.session.now().saturating_duration_since(t)))
}

/// `Register(PC)` as PowerView prints it; `None` when unverified or refused.
pub(crate) fn pc(ctx: &mut ProbeCtx<'_>, v: &mut Verified) -> Result<Option<String>, ProbeFail> {
    if !v.function(ctx, "Register")? {
        return Ok(None);
    }
    eval_text(ctx, "Register(PC)")
}

fn pc_and_name(ctx: &mut ProbeCtx<'_>, v: &mut Verified) -> Result<String, ProbeFail> {
    let Some(pc) = pc(ctx, v)? else { return Ok("pc=?".into()) };
    let name = if v.function(ctx, "sYmbol.NAME")? {
        eval_text(ctx, &format!("sYmbol.NAME(P:{pc})"))?.unwrap_or_else(|| "?".into())
    } else {
        unverified("sYmbol.NAME")
    };
    Ok(format!("pc={pc} sym={name}"))
}

/// Lines of a `STOre ... Break` script without comments, for comparison.
fn breakpoint_lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(';')).map(str::to_string).collect()
}

/// `STOre "<dir>\<name>" Break` read back through `region`; `None` when PowerView refused.
fn store(ctx: &mut ProbeCtx<'_>, dir: &StagePath, region: &VmRegion, name: &str) -> Result<Option<String>, ProbeFail> {
    let p = dir.join(name)?;
    ctx.push(Undo::RemoveFile(p.clone()))?;
    if soft(ctx.session.cmd(&cmd::store_breakpoints(&p)))?.is_none() {
        return Ok(None);
    }
    let Some(size) = soft(transfer::file_size(ctx.session, &p))? else { return Ok(None) };
    if size > u64::from(region.len()) {
        return Err(ProbeFail::Check(format!("{p} holds {size} bytes, more than {STORE_LEN}")));
    }
    if size == 0 {
        return Ok(Some(String::new()));
    }
    let raw = soft(transfer::get_file(ctx.session, &p, region, size as u32))?;
    Ok(raw.map(|r| String::from_utf8_lossy(&r).into_owned()))
}

/// The run-control inputs and what the steps share.
struct Rc {
    dir: StagePath,
    region: VmRegion,
    v: Verified,
    /// Every `Revert` applied cleanly so far.
    clean: bool,
    base: Vec<String>,
}

impl Rc {
    fn store(&mut self, ctx: &mut ProbeCtx<'_>, name: &str) -> Result<Option<String>, ProbeFail> {
        store(ctx, &self.dir, &self.region, name)
    }

    /// `run_until_halt` once `Go` is verified: the `Ran` token or `unverified:Go`.
    fn run(&mut self, ctx: &mut ProbeCtx<'_>, command: &str) -> Result<String, ProbeFail> {
        if !self.v.command(ctx, GO)? {
            return Ok(unverified(GO));
        }
        Ok(run_until_halt(ctx, command)?.token())
    }

    fn same_as_base(&self, text: &Option<String>) -> bool {
        text.as_deref().map(breakpoint_lines).as_ref() == Some(&self.base)
    }
}

/// Run control on a halted target: breakpoint list stability, `Go`/`Break` and every core's PC,
/// `Go <symbol>`, steps and step-out candidates, an execution breakpoint, and with operator input
/// write breakpoints, a `/COUNT` breakpoint and a reverted RAM and register write. Every change
/// is reverted in memory right after it is measured. Changes the target state.
pub fn run_control(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let input = ctx.env.cfg.gates.run_control.clone().expect("gate opens only with a symbol");
    let symbol = input.symbol.clone();
    let state = ctx.session.target_state()?;
    if !matches!(state, TargetState::Halted | TargetState::Stopped) {
        return Err(ProbeFail::Skip(SkipReason::TargetNotUp { state: state.name().into() }));
    }
    if !ctx.session.eval(&fnc::symbol_exists(&symbol))?.into_bool("sYmbol.EXIST")? {
        return Err(ProbeFail::Skip(SkipReason::NotApplicable {
            why: format!("symbol {} not found", symbol.as_str()),
        }));
    }
    let root = stage_root(ctx)?;
    let dir = workdir(ctx, &root, "rc")?;
    let region = scratch_regions(ctx, &[STORE_LEN])?[0];
    let mut rc = Rc { dir, region, v: Verified::default(), clean: true, base: Vec::new() };
    let base = rc.store(ctx, "bp0.cmm")?.ok_or_else(|| ProbeFail::Check("STOre ... Break was refused".into()))?;
    rc.base = breakpoint_lines(&base);
    ctx.measure("breakpoints_before", Value::Text(rc.base.join(" | ")));
    let again = rc.store(ctx, "bp0b.cmm")?;
    ctx.text("store", if rc.same_as_base(&again) { "stable" } else { "diff" });

    ctx.push(Undo::BreakTarget)?;
    let go = go_break(ctx, &mut rc)?;
    ctx.text("go", go);
    let halt_all = cores(ctx, &mut rc)?;
    ctx.text("halt_all", halt_all);

    let reached = rc.run(ctx, &cmd::go(&symbol))?;
    ctx.measure("go_symbol", Value::Text(reached));
    let bp1 = rc.store(ctx, "bp1.cmm")?;
    ctx.text("tmp_bp", if rc.same_as_base(&bp1) { "clean" } else { "left" });

    let stepout = steps(ctx, &mut rc)?;
    ctx.text("stepout", stepout);
    let bp_impl = exec_breakpoint(ctx, &mut rc, &symbol)?;
    ctx.text("bp_impl", bp_impl);
    let wp_max = match &input.watch {
        Some(var) => write_breakpoints(ctx, &mut rc, var)?,
        None => "n/a".into(),
    };
    ctx.text("wp_max", wp_max);
    let count = match &input.loop_line {
        Some(line) => count_breakpoint(ctx, &mut rc, line)?,
        None => "n/a".into(),
    };
    ctx.text("count", count);
    let write = match input.scratch_ram {
        Some(a) => scratch_write(ctx, &mut rc, a)?,
        None => "n/a".into(),
    };
    ctx.text("write", write);

    let end = rc.store(ctx, "bp9.cmm")?;
    let unchanged = rc.same_as_base(&end);
    ctx.text("bp_unchanged", if unchanged { "yes" } else { "NO" });
    ctx.measure("reverts", Value::Text(ok(rc.clean).into()));
    ctx.check(unchanged && rc.clean, || {
        format!("breakpoint list unchanged: {unchanged}, every revert clean: {}", rc.clean)
    })
}

/// `Go`, the target state sampled for 1 s, then the probe's `Break`: milliseconds until halted.
fn go_break(ctx: &mut ProbeCtx<'_>, rc: &mut Rc) -> Result<String, ProbeFail> {
    if !rc.v.command(ctx, GO)? {
        return Ok(unverified(GO));
    }
    if soft(ctx.session.cmd(GO))?.is_none() {
        return Ok("refused".into());
    }
    let mut running = 0;
    for i in 0..RUN_POLLS {
        match ctx.session.target_state()? {
            TargetState::Running => running += 1,
            TargetState::Halted | TargetState::Stopped => {
                ctx.measure("go_running", Value::Text(format!("{running}/{}", i + 1)));
                return Ok("halted-early".into());
            }
            _ => {}
        }
        nap(ctx, POLL);
    }
    ctx.measure("go_running", Value::Text(format!("{running}/{RUN_POLLS}")));
    let t = ctx.session.now();
    if soft(ctx.session.cmd(cmd::BREAK))?.is_none() {
        return Ok("break-refused".into());
    }
    Ok(match wait_halt(ctx, GO_POLLS)? {
        true => ms(ctx.session.now().saturating_duration_since(t)),
        false => "nohalt".into(),
    })
}

/// Every core's PC after the `Break`, selecting each core and then the first one again.
fn cores(ctx: &mut ProbeCtx<'_>, rc: &mut Rc) -> Result<String, ProbeFail> {
    if !rc.v.function(ctx, "CORE.NUMBER")? {
        return Ok(unverified("CORE.NUMBER"));
    }
    let n = soft(ctx.session.eval("CORE.NUMBER()").and_then(|v| v.into_int("CORE.NUMBER()")))?;
    let Some(n) = n.filter(|n| *n > 1) else { return Ok("single".into()) };
    if !rc.v.command(ctx, "CORE.select")? {
        return Ok(unverified("CORE.select"));
    }
    let Some(c0) = eval_text(ctx, "CORE()")? else { return Ok("partial".into()) };
    let (mut answered, mut registered) = (0, false);
    for k in 0..n {
        if soft(ctx.session.cmd(&format!("CORE.select {k}")))?.is_none() {
            ctx.measure(&format!("core.{k}"), Value::Text("refused".into()));
            continue;
        }
        if !registered {
            ctx.push(Undo::Revert { what: "core".into(), commands: vec![format!("CORE.select {c0}")] })?;
            registered = true;
        }
        let pc = pc(ctx, &mut rc.v)?;
        answered += usize::from(pc.is_some());
        ctx.measure(&format!("core.{k}"), Value::Text(pc.unwrap_or_else(|| "refused".into())));
    }
    revert(ctx, "core", &mut rc.clean);
    Ok(if answered as i64 == n { "ok" } else { "partial" }.into())
}

/// Three steps, then the step-out candidates in order: the first that halts is `stepout`.
fn steps(ctx: &mut ProbeCtx<'_>, rc: &mut Rc) -> Result<String, ProbeFail> {
    for c in ["Step.Over", "Step", "Step.Asm"] {
        let r = if rc.v.command(ctx, c)? {
            let ran = run_until_halt(ctx, c)?;
            format!("{} {}", ran.token(), pc_and_name(ctx, &mut rc.v)?)
        } else {
            unverified(c)
        };
        ctx.measure(&format!("step.{c}"), Value::Text(r));
    }
    for c in ["Step.Out", "Go.Return", "Go.Up"] {
        if !rc.v.command(ctx, c)? {
            continue;
        }
        let ran = run_until_halt(ctx, c)?;
        let at = pc_and_name(ctx, &mut rc.v)?;
        ctx.measure(&format!("step.{c}"), Value::Text(format!("{} {at}", ran.token())));
        if matches!(ran, Ran::Halted(_)) {
            return Ok(c.into());
        }
    }
    Ok("none".into())
}

/// The symbol as a word of a breakpoint line.
fn names_symbol(line: &str, symbol: &str) -> bool {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '\\' | ':')))
        .any(|w| w.eq_ignore_ascii_case(symbol))
}

/// `Break.Set <symbol>` and how `STOre` writes it (`/SOFT`, `/Onchip`); skipped when the
/// operator's list already has a breakpoint there.
fn exec_breakpoint(ctx: &mut ProbeCtx<'_>, rc: &mut Rc, symbol: &t32stage::Symbol) -> Result<String, ProbeFail> {
    let s = symbol.as_str();
    if rc.base.iter().any(|l| names_symbol(l, s)) {
        ctx.measure("bp_exec", Value::Text("in the operator's list".into()));
        return Ok("?".into());
    }
    if !rc.v.command(ctx, "Break.Set")? {
        return Ok(unverified("Break.Set"));
    }
    if soft(ctx.session.cmd(&format!("Break.Set {s}")))?.is_none() {
        ctx.measure("bp_exec", Value::Text("refused".into()));
        return Ok("?".into());
    }
    let what = format!("bp {s}");
    ctx.push(Undo::Revert { what: what.clone(), commands: vec![format!("Break.Delete {s}")] })?;
    let text = rc.store(ctx, "bp2.cmm")?;
    revert(ctx, &what, &mut rc.clean);
    let lines = text.as_deref().map(breakpoint_lines).unwrap_or_default();
    ctx.measure("bp_exec", Value::Text(lines.join(" | ")));
    let up = lines.join("\n").to_ascii_uppercase();
    Ok(if up.contains("/SOFT") {
        "soft"
    } else if up.contains("/ONCHIP") {
        "onchip"
    } else {
        "?"
    }
    .into())
}

/// Up to five `/Write` breakpoints 4 bytes apart on `var`, a `Go`, all deleted again: how many
/// PowerView accepted.
fn write_breakpoints(ctx: &mut ProbeCtx<'_>, rc: &mut Rc, var: &t32stage::Symbol) -> Result<String, ProbeFail> {
    if !rc.v.command(ctx, "Break.Set")? {
        return Ok(unverified("Break.Set"));
    }
    if !rc.v.function(ctx, "Var.ADDRESS")? {
        return Ok(unverified("Var.ADDRESS"));
    }
    let mut set = 0;
    for k in 1..=5u32 {
        let at = format!("(Var.ADDRESS({})+{})", var.as_str(), 4 * (k - 1));
        match command(ctx, &format!("Break.Set {at} /Write"))? {
            Ok(()) => {
                ctx.push(Undo::Revert { what: format!("wp{k}"), commands: vec![format!("Break.Delete {at}")] })?;
                set = k;
            }
            Err(e) => {
                let why = format!("k={k} status={} {}", e.code, e.message.as_deref().unwrap_or(""));
                ctx.measure("wp_refused", Value::Text(why));
                break;
            }
        }
    }
    if set > 0 {
        let ran = rc.run(ctx, GO)?;
        let at = pc_and_name(ctx, &mut rc.v)?;
        ctx.measure("wp_go", Value::Text(format!("{ran} {at}")));
    }
    for k in (1..=set).rev() {
        revert(ctx, &format!("wp{k}"), &mut rc.clean);
    }
    Ok(set.to_string())
}

/// `Break.Set <file>\<line> /COUNT 3`, a `Go`, and whether `STOre` keeps the count.
fn count_breakpoint(ctx: &mut ProbeCtx<'_>, rc: &mut Rc, line: &SourceLine) -> Result<String, ProbeFail> {
    if !rc.v.command(ctx, "Break.Set")? {
        return Ok(unverified("Break.Set"));
    }
    let at = format!("{}\\{}", line.file, line.line);
    if soft(ctx.session.cmd(&format!("Break.Set {at} /COUNT 3")))?.is_none() {
        return Ok("refused".into());
    }
    ctx.push(Undo::Revert { what: "count".into(), commands: vec![format!("Break.Delete {at}")] })?;
    let ran = rc.run(ctx, GO)?;
    ctx.measure("count_go", Value::Text(ran));
    let text = rc.store(ctx, "bp3.cmm")?;
    revert(ctx, "count", &mut rc.clean);
    let lines = text.as_deref().map(breakpoint_lines).unwrap_or_default();
    let stored = lines.iter().find(|l| l.to_ascii_uppercase().contains("/COUNT"));
    ctx.measure("count_line", Value::Text(stored.cloned().unwrap_or_default()));
    Ok(if stored.is_some() { "stored" } else { "dropped" }.into())
}

/// A long written to `D:<a>` and restored, then X9 written and restored; `ok` when the RAM write
/// read back and the restore brought the old bytes back.
fn scratch_write(ctx: &mut ProbeCtx<'_>, rc: &mut Rc, a: u64) -> Result<String, ProbeFail> {
    if !rc.v.command(ctx, "Data.Set")? {
        return Ok(unverified("Data.Set"));
    }
    let at = MemAddr::new(AccessClass::parse("D")?, a);
    let Some(orig) = soft(ctx.session.mem_read(&at, 4))? else {
        ctx.measure("ram", Value::Text("unreadable".into()));
        return Ok("FAIL".into());
    };
    let le = u32::from_le_bytes([orig[0], orig[1], orig[2], orig[3]]);
    let set = |v: u32| format!("Data.Set D:0x{a:X} %Long %LE 0x{v:08X}");
    ctx.push(Undo::Revert { what: "ram".into(), commands: vec![set(le)] })?;
    let wrote = soft(ctx.session.cmd(&set(0x5A5A_A5A5)))?.is_some();
    let back = soft(ctx.session.mem_read(&at, 4))?;
    revert(ctx, "ram", &mut rc.clean);
    let restored = soft(ctx.session.mem_read(&at, 4))?;
    let shown = |b: &Option<Vec<u8>>| b.as_deref().map_or("-".into(), hex);
    ctx.measure(
        "ram",
        Value::Text(format!("orig={} wrote={wrote} back={} restored={}", hex(&orig), shown(&back), shown(&restored))),
    );
    let ok = wrote && back.as_deref() == Some(&[0xA5, 0xA5, 0x5A, 0x5A][..]) && restored.as_deref() == Some(&orig[..]);

    if rc.v.command(ctx, "Register.Set")? && rc.v.function(ctx, "Register")? {
        let x9 = match eval_text(ctx, "Register(X9)")? {
            Some(orig) => {
                ctx.push(Undo::Revert { what: "x9".into(), commands: vec![format!("Register.Set X9 {orig}")] })?;
                let wrote = soft(ctx.session.cmd("Register.Set X9 0x0123456789ABCDEF"))?.is_some();
                let back = eval_text(ctx, "Register(X9)")?;
                revert(ctx, "x9", &mut rc.clean);
                let restored = eval_text(ctx, "Register(X9)")?;
                format!("orig={orig} wrote={wrote} back={back:?} restored={restored:?}")
            }
            None => "refused".into(),
        };
        ctx.measure("x9", Value::Text(x9));
    }
    Ok(if ok { "ok" } else { "FAIL" }.into())
}

/// `Data.LOAD.Elf x.elf.gz /UNZIP /NoCODE /NoReg`, then the fallback through `VM:`. Symbols only:
/// target memory and registers are never written. The loaded symbols stay in PowerView.
pub fn elf_unzip(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let elf = ctx.env.cfg.gates.elf.clone().expect("gate opens only with an ELF");
    ctx.measure("elf_path", Value::Text(elf.path.display().to_string()));
    ctx.headline("elf", Value::Bytes(elf.bytes.len() as u64));
    let (stage, session) = ctx.stage()?;
    let gz = stage.upload(session, Put::KeepGz, &elf.bytes, "x.elf.gz")?;
    let direct = match session.cmd(&cmd::load_elf_symbols(&gz.path, true)) {
        Err(e) if matches!(e.class(), ErrorClass::ConnectionFatal | ErrorClass::Cancelled) => return Err(e.into()),
        other => other,
    };
    let plain = stage.upload(session, Put::Inflate, &elf.bytes, "x.elf")?;
    let fallback = soft(session.cmd(&cmd::load_elf_symbols(&plain.path, false)))?.is_some();
    let direct_ok = match direct {
        Ok(()) => true,
        Err(e) => {
            ctx.measure("direct_error", Value::Text(e.to_string()));
            let failure = ctx.feature_failure(t32rcl::Feature::ElfUnzip, e);
            if !matches!(failure, ProbeFail::Unsupported(_)) && !fallback {
                return Err(failure);
            }
            false
        }
    };
    ctx.text("direct", if direct_ok { "yes" } else { "no" });
    ctx.text("fallback", ok(fallback));
    if let Some(sym) = &elf.symbol {
        let found = soft(ctx.session.eval(&fnc::symbol_exists(sym)).and_then(|v| v.into_bool("sYmbol.EXIST")))?;
        ctx.text(
            "symbol",
            match found {
                Some(true) => "found",
                Some(false) => "missing",
                None => "?",
            },
        );
    }
    ctx.check(fallback, || "Data.LOAD.Elf of the inflated ELF failed".into())
}

#[cfg(test)]
#[path = "optin_tests.rs"]
mod optin_tests;
