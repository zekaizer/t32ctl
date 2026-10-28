// SPDX-License-Identifier: Apache-2.0
//! Opt-in probes. `registry::select` refuses each one without its gate, and the gate carries
//! the typed input the probe needs, so these bodies may assume consent.

use std::{
    thread,
    time::{Duration, Instant},
};

use t32rcl::{CallBudget, ConnectError, Endpoint, ErrorClass, RemoteKind, Session, TargetState};
use t32stage::{
    Put, Undo,
    practice::{cmd, fnc},
};

use super::{
    lab::{Case, Ending, Expect, Lab, LabSizes, Seen, Until},
    nr_used_cons, ok, soft,
};
use crate::{
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

#[cfg(test)]
#[path = "optin_tests.rs"]
mod optin_tests;
