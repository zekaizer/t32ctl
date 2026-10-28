// SPDX-License-Identifier: Apache-2.0
//! Opt-in probes. `registry::select` refuses each one without its gate, and the gate carries
//! the typed input the probe needs, so these bodies may assume consent.

use std::{
    thread,
    time::{Duration, Instant},
};

use t32rcl::{Endpoint, ErrorClass, Session, TargetState};
use t32stage::{
    Put, RunOutcome, Undo,
    practice::{cmd, fnc},
};

use super::{bundle_of, nr_used_cons, ok, run_entry, soft};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::{SkipReason, Value},
    stats::Stats,
};

/// A modal `DIALOG.OK`: the client must see state 2, send `T32_Stop`, and find the stack empty.
/// Shows a window on the operator's screen.
pub fn dialog(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let script = "DIALOG.OK \"t32probe: automatic test, this dialog closes itself\"\nENDDO\n";
    let bundle = bundle_of(&[("modal.cmm", script.as_bytes())])?;
    let (stage, session) = ctx.stage()?;
    let snap = stage.ensure_snapshot(session, &bundle)?;
    let t = Instant::now();
    let report = run_entry(stage, session, &snap, "modal.cmm", &[], Duration::from_secs(10))?;
    let depth = session.eval(fnc::STACK_DEPTH)?.into_int(fnc::STACK_DEPTH)?;
    ctx.measure("outcome", Value::Text(format!("{:?}", report.outcome)));
    let stopped = report.outcome == RunOutcome::DialogAborted;
    ctx.text("state2", if report.polls.dialog > 0 { "seen" } else { "never" });
    ctx.text("stop", ok(stopped));
    ctx.headline("after", Value::Seconds(t.elapsed().as_secs_f64()));
    ctx.text("stack_clean", if depth == 0 { "yes" } else { "NO" });
    ctx.check(stopped && depth == 0, || format!("dialog run ended {:?}, stack depth {depth}", report.outcome))
}

fn latency_series(b: &mut Session, until: Instant) -> Result<Vec<Duration>, ProbeFail> {
    let mut v = Vec::new();
    while Instant::now() < until {
        let t = Instant::now();
        b.eval("TRUE()")?;
        v.push(t.elapsed());
        thread::sleep(Duration::from_millis(100));
    }
    Ok(v)
}

/// A second connection B: `NrUsedCons` from both, B's latency idle and while A runs a long `DO`.
pub fn multi_client(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let endpoint = ctx.env.endpoint.clone().unwrap_or_else(|| Endpoint::new(&ctx.env.cfg.hosts[0], ctx.env.cfg.port));
    let mut b = Session::connect(endpoint, ctx.env.connect_opts()).map_err(|e| ProbeFail::Rcl(e.into()))?;
    let cons = soft(nr_used_cons(&mut b))?;
    ctx.headline("cons", cons.map_or(Value::Text("?".into()), |n| Value::Count(n.max(0) as u64)));
    let mut idle = latency_series(&mut b, Instant::now() + Duration::from_secs(1))?;
    let idle = Stats::from_samples(&mut idle).expect("at least one sample");
    ctx.text("b_idle", idle.compact());

    let bundle = bundle_of(&[("long.cmm", b"WAIT 3.s\nENDDO\n")])?;
    let (stage, session) = ctx.stage()?;
    let snap = stage.ensure_snapshot(session, &bundle)?;
    let (a_result, during) = thread::scope(|s| {
        let a = s.spawn(|| run_entry(stage, session, &snap, "long.cmm", &[], Duration::from_secs(20)));
        thread::sleep(Duration::from_millis(300));
        let during = latency_series(&mut b, Instant::now() + Duration::from_millis(2500));
        (a.join(), during)
    });
    let report = a_result.map_err(|_| ProbeFail::Check("client A thread panicked".into()))??;
    let mut during = during?;
    let during = Stats::from_samples(&mut during).expect("at least one sample");
    ctx.text("b_during", during.compact());
    let blocked = during.max > Duration::from_secs(2);
    ctx.text("b_blocked", if blocked { "yes" } else { "no" });
    ctx.measure("a_outcome", Value::Text(format!("{:?}", report.outcome)));
    ctx.check(report.outcome.is_success(), || format!("client A run ended {:?}", report.outcome))
}

/// Lines of a `STOre ... Break` script without comments, for comparison.
fn breakpoint_lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(';')).map(str::to_string).collect()
}

/// `Go <symbol>` with a timeout, `Break` if needed, and the breakpoint list unchanged.
/// Changes the target state.
pub fn run_control(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let symbol = ctx.env.cfg.gates.run_control.clone().expect("gate opens only with a symbol");
    let state = ctx.session.target_state()?;
    if !matches!(state, TargetState::Halted | TargetState::Stopped) {
        return Err(ProbeFail::Skip(SkipReason::TargetNotUp { state: state.name().into() }));
    }
    if !ctx.session.eval(&fnc::symbol_exists(&symbol))?.into_bool("sYmbol.EXIST")? {
        return Err(ProbeFail::Skip(SkipReason::NotApplicable {
            why: format!("symbol {} not found", symbol.as_str()),
        }));
    }
    let store = |ctx: &mut ProbeCtx<'_>, name: &str| -> Result<Vec<String>, ProbeFail> {
        let f = ctx.staged_file(name)?;
        let (stage, session) = ctx.stage()?;
        session.cmd(&cmd::store_breakpoints(&f))?;
        Ok(breakpoint_lines(&String::from_utf8_lossy(&stage.download(session, &f)?)))
    };
    let before = store(ctx, "bp0.cmm")?;
    ctx.push(Undo::BreakTarget)?;
    let t = Instant::now();
    ctx.session.cmd(&cmd::go(&symbol))?;
    let mut reached = false;
    while t.elapsed() < Duration::from_secs(5) {
        if matches!(ctx.session.target_state()?, TargetState::Halted | TargetState::Stopped) {
            reached = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !reached {
        ctx.session.cmd(cmd::BREAK)?;
    }
    ctx.text("reached", if reached { "yes" } else { "no-break" });
    ctx.headline("after", Value::Seconds(t.elapsed().as_secs_f64()));
    let after = store(ctx, "bp1.cmm")?;
    ctx.measure("breakpoints_before", Value::Text(before.join(" | ")));
    let unchanged = before == after;
    ctx.text("bp_unchanged", if unchanged { "yes" } else { "NO" });
    ctx.check(unchanged, || format!("breakpoint list changed: {before:?} -> {after:?}"))
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
