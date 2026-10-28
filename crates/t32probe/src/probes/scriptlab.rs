// SPDX-License-Identifier: Apache-2.0
//! Probes on Lab scripts: script-lab, script-halt.

use std::time::Duration;

use t32rcl::{CallBudget, FuncValue, MacroName, MacroSpelling, PracticeState};
use t32stage::{
    CleanupOutcome, Undo,
    practice::{cmd, fnc},
    transfer,
};

use super::{
    hex,
    lab::{
        Answer, Case, CaseRecord, Ending, Expect, Lab, LabSizes, Seen, Slot, Until, raw_note, stack_depth, stop_token,
        vm_slot,
    },
    pattern, soft,
};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    env::ScriptOut,
    outcome::{Value, key},
    stats::Stats,
};

const MIB: u32 = 1 << 20;
/// `{L}` of script-lab and script-halt.
const LARGE: u32 = 8 * MIB;

fn case(id: impl Into<String>, body: &str, expect: Expect, secs: u64) -> Case {
    Case::new(id, body, expect, Duration::from_secs(secs))
}

pub(crate) fn o0() -> Case {
    case("o0", O0, Expect::End, 10)
}
pub(crate) fn c1() -> Case {
    case("c1", C1, Expect::HaltAfter("z"), 10)
}
pub(crate) fn c2() -> Case {
    case("c2", C2, Expect::HaltAfter("z"), 10).outputs(&["c2-w.txt"]).oracle_buffer()
}
pub(crate) fn w300(k: u32) -> Case {
    case(format!("w300-{k:02}"), W300, Expect::End, 5)
}
pub(crate) fn nest(k: u32) -> Case {
    case(format!("nest-{k:02}"), NEST, Expect::End, 5)
}
pub(crate) fn w3() -> Case {
    case("w3", W3, Expect::End, 10)
}
pub(crate) fn looped() -> Case {
    case("loop", LOOP, Expect::End, 30)
}

const O0: &str =
    "@a\n@ask nodir OS.DIR(\"{S}\") == FALSE()\n@ask size OS.FILE.SIZE(\"{S}\\o0-a.bin\") == 0x00000001\n@z\nENDDO";
const C1: &str = "@a\n@z\nData.Set VM:{R0} \"t32q-vs-{run}\" 0\n@v\nENDDO";
const C2: &str =
    "@a\n@z\nOPEN #8 \"{S}\\c2-w.txt\" /Create\nWRITE #8 \"t32q-wr-{run}\"\nWRITE #8 \"a b,c\"\nCLOSE #8\n@v\nENDDO";
const W300: &str = "@a\nWAIT 300.ms\n@z\nENDDO";
const NEST: &str = "@a\nDO \"{S}\\c50.cmm\"\nDO \"{S}\\c50.cmm\"\nDO \"{S}\\c50.cmm\"\n@z\nENDDO";
pub(crate) const C50: &str = "WAIT 50.ms\nENDDO\n";
const W3: &str = "@a\n@w\nWAIT 3.s\n@z\nENDDO";
pub(crate) fn big() -> Case {
    case("big", BIG, Expect::End, 30).outputs(&["big.bin"])
}

const LOOP: &str = "@a\nPRIVATE &i\n&i=0.\n@w\nRePeaT 50000.\n(\n  &i=&i+1.\n)\n@ask n &i == 50000.\n@z\nENDDO";

const BIG: &str = "@a\n@w\nData.SAVE.Binary \"{S}\\big.bin\" VM:{L}++0x007FFFFF\n@z\nENDDO";

/// The file-existence oracle, then the value channels, DO timing, PRACTICE state while scripts
/// run and RCL during `WAIT`, a loop and a large `SAVE`. Only the oracle decides the verdict.
pub fn script_lab(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "lab", LabSizes { large: Some(LARGE), extra: Vec::new() })?;
    let result = script_lab_cases(ctx, &mut lab);
    let closed = lab.close(ctx);
    result.and(closed)
}

/// `Data.Set` from the command line: a string with its NUL, then a little-endian long
/// (general_ref_d `Data.Set`).
fn cmdset(ctx: &mut ProbeCtx<'_>, lab: &Lab) -> Result<&'static str, ProbeFail> {
    let tries = [
        ("Data.Set VM:{R0} \"t32q\" 0", 0, &b"t32q\0"[..]),
        ("Data.Set VM:{R1} %Long %LE 0x11223344", 1, &[0x44, 0x33, 0x22, 0x11][..]),
    ];
    let mut token = "ok";
    for (cmd, slot, want) in tries {
        if soft(ctx.session.cmd(&lab.fill(cmd)?))?.is_none() {
            return Ok("refused");
        }
        let got = soft(ctx.session.mem_read(&lab.r_slot(slot).addr(), want.len()))?;
        if got.as_deref() != Some(want) {
            token = "bytes";
        }
    }
    Ok(token)
}

fn script_lab_cases(ctx: &mut ProbeCtx<'_>, lab: &mut Lab) -> ProbeResult {
    let cmdset = cmdset(ctx, lab)?;
    let o0 = lab.run(ctx, &o0())?;
    let oracle = o0.has("a") && o0.has("z") && o0.answer("nodir") == Answer::No && o0.answer("size") == Answer::Yes;
    ctx.text("oracle", if oracle { "fx" } else { "FAIL" });
    ctx.text("cmdset", cmdset);
    if !oracle {
        if !lab.cut.is_empty() {
            return Err(ProbeFail::Check(format!("cut by the probe deadline: {}", lab.cut.join(","))));
        }
        let m: Vec<&str> = o0.markers.iter().map(String::as_str).collect();
        let ans: Vec<String> = o0.answers.iter().map(|(k, a)| format!("{k}:{}", a.token())).collect();
        return Err(ProbeFail::Check(format!(
            "the file-existence oracle failed: m={} ans={}",
            m.join(","),
            ans.join(" ")
        )));
    }

    lab.clear_results(ctx)?;
    let c1 = lab.run(ctx, &c1())?;
    let r0 = if c1.ran() { soft(ctx.session.mem_read(&lab.r_slot(0).addr(), 0x100))? } else { None };
    let vmset = match r0.as_deref().map(vm_slot) {
        _ if !c1.ran() => "-",
        Some(Slot::Text(t)) if t == lab.fill("t32q-vs-{run}")? => "ok",
        _ if c1.halted() => "halt",
        _ => "bytes",
    };
    let c2 = lab.run(ctx, &c2())?;
    let raw = if c2.ran() { lab.file_slot(ctx, "c2-w.txt")?.1 } else { Vec::new() };
    ctx.measure("write_raw", Value::Text(raw_note(&raw)));
    let first = String::from_utf8_lossy(&raw).lines().next().map(str::to_string);
    let write = match first {
        _ if !c2.ran() => "-",
        Some(l) if l == lab.fill("t32q-wr-{run}")? => {
            if raw.windows(2).any(|w| w == b"\r\n") {
                "crlf"
            } else {
                "lf"
            }
        }
        _ if c2.halted() => "halt",
        _ => "bytes",
    };
    let out = match (vmset, write) {
        ("ok", _) => ScriptOut::VmSet,
        (_, "crlf" | "lf") => ScriptOut::Write,
        _ => ScriptOut::Off,
    };
    ctx.learned.script_out = Some(out);
    lab.out = out;
    ctx.text("vmset", vmset);
    ctx.text("write", write);
    ctx.text(key::SCRIPT_OUT, out.tag());

    let mut plain = Vec::new();
    for k in 1..=20 {
        plain.push(lab.run(ctx, &w300(k))?);
    }
    lab.stage_file(ctx, "c50.cmm", C50)?;
    for k in 1..=20 {
        plain.push(lab.run(ctx, &nest(k))?);
    }

    let e1 = MacroName::new(&format!("{}_e1", lab.q()))?;
    ctx.push(Undo::ClearMacros(vec![e1.clone()]))?;
    let e1_set = soft(ctx.session.macro_set(&e1, &lab.fill("e1-{run}")?))?.is_some();
    ctx.measure("globals_left", Value::Count(e1_set.into()));
    let w3 = during(ctx, lab, &w3(), &e1)?;
    let lp = during(ctx, lab, &looped(), &e1)?;
    let big = match lab.l {
        Some(l) => {
            ctx.session.mem_write(&l.addr(), &pattern(u64::from(LARGE), LARGE as usize))?;
            during(ctx, lab, &big(), &e1)?
        }
        None => None,
    };

    let mut dos: Vec<Duration> = lab.records.iter().filter_map(|r| r.do_time).collect();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let do_token = Stats::from_samples(&mut dos).map_or("-".into(), |s| format!("{:.1}/{:.1}ms", ms(s.p50), ms(s.max)));
    ctx.text("do", do_token);
    let blocks = w3.as_ref().and_then(|d| d.rec.do_time).map(|t| t >= Duration::from_millis(2500));
    ctx.text("blocks", blocks.map_or("-", |b| if b { "yes" } else { "no" }));
    let pre0 = lab.records.iter().map(|r| r.pre0).max().unwrap_or(0);
    ctx.headline("pre0", Value::Count(pre0.into()));
    let first1 = lab.records.iter().filter_map(|r| r.first1).max();
    ctx.text("first1", first1.map_or("-".into(), |d| format!("{:.0}ms", ms(d))));
    let dip_max = plain.iter().map(|r| r.dip_max).max().unwrap_or_default();
    let dip_runs = plain.iter().filter(|r| r.dip_max >= Duration::from_millis(10)).count();
    ctx.text("dip", format!("{:.0}ms/{dip_runs}", ms(dip_max)));
    let wait_state = match w3.as_ref().map(|d| d.states.as_slice()) {
        None | Some([]) => "-",
        Some(v) if v.iter().all(|&s| s == PracticeState::Running) => "running",
        Some(v) if v.iter().all(|&s| s == PracticeState::Idle) => "idle",
        Some(_) => "mixed",
    };
    ctx.text("wait_state", wait_state);
    for (k, d) in [("rcl_wait", &w3), ("rcl_loop", &lp), ("rcl_save", &big)] {
        rcl_tokens(ctx, k, d.as_ref());
    }
    let cmd_during = match w3.as_ref().map(|d| d.commands.as_slice()) {
        None | Some([]) => "-",
        Some(v) if v.iter().any(|(ok, _)| !ok) => "refused",
        Some(v) if v.iter().any(|(_, t)| *t >= Duration::from_secs(1)) => "waited",
        Some(_) => "ok",
    };
    ctx.text("cmd_during", cmd_during);
    let w3_path = lab.path("w3.cmm")?.text();
    let ppf = match w3.as_ref().and_then(|d| d.ppf.as_deref()) {
        None => "-",
        Some("") => "empty",
        Some(p) if p.eq_ignore_ascii_case(&w3_path) => "ok",
        Some(_) => "other",
    };
    ctx.text("ppf", ppf);
    ctx.measure("ppf_text", Value::Text(format!("{:?}", w3.as_ref().and_then(|d| d.ppf.clone()))));
    let loop_token = match lp.as_ref().map(|d| &d.rec) {
        None => "-".to_string(),
        Some(r) if r.ended() && r.answer("n") == Answer::Yes => "ok".into(),
        Some(r) if r.halted() => "halt".into(),
        Some(r) => format!("n:{}", r.answer("n").token()),
    };
    ctx.text("loop", loop_token);
    Ok(())
}

/// What a script measured with RCL rounds ran into.
struct During {
    rec: CaseRecord,
    /// Call kind and round trip.
    calls: Vec<(&'static str, Duration)>,
    /// `GetPracticeState` between `w` and `z`.
    states: Vec<PracticeState>,
    /// `ExecuteCommand` while the script runs: accepted, round trip.
    commands: Vec<(bool, Duration)>,
    ppf: Option<String>,
}

/// Run `case`; once `w` exists, a round of RCL calls every `timing.round` until `z`, each timed.
/// `None` when the case was not staged or was cut.
fn during(ctx: &mut ProbeCtx<'_>, lab: &mut Lab, case: &Case, e1: &MacroName) -> Result<Option<During>, ProbeFail> {
    let staged = match lab.stage(ctx, case)? {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    let mut run = match lab.start(ctx, staged)? {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    let (mut calls, mut states, mut commands, mut ppf) = (Vec::new(), Vec::new(), Vec::new(), None);
    let r15 = lab.fill("Data.Set VM:{R15} \"x\" 0")?;
    let budget = CallBudget::idle(Duration::from_secs(10)).with_total(Duration::from_secs(15));
    macro_rules! timed {
        ($kind:expr, $call:expr) => {{
            let t = ctx.session.now();
            let v = $call;
            calls.push(($kind, ctx.session.now().saturating_duration_since(t)));
            v
        }};
    }
    if lab.wait(ctx, &mut run, Until::Marker("w"))? == Seen::Reached {
        loop {
            let now = ctx.session.now();
            let state = timed!("state", ctx.session.practice_state()?);
            let depth = timed!("depth", stack_depth(ctx)?);
            timed!("true", soft(ctx.session.eval("TRUE()"))?);
            let p = timed!("ppf", soft(ctx.session.eval("OS.PPF()"))?);
            timed!("exp", soft(ctx.session.macro_get(e1))?);
            timed!("mem", soft(ctx.session.mem_read(&lab.r_slot(0).addr(), 16))?);
            let t = ctx.session.now();
            let accepted = soft(ctx.session.with_budget(budget).cmd(&r15))?.is_some();
            let took = ctx.session.now().saturating_duration_since(t);
            calls.push(("cmd", took));
            commands.push((accepted, took));
            if ppf.is_none() {
                ppf = p.map(|v| match v {
                    FuncValue::Text(t) => t,
                    FuncValue::Empty => String::new(),
                    other => other.to_string(),
                });
            }
            let z = lab.poll_marker(ctx, &mut run, "z")?;
            if !z {
                states.push(state);
            }
            if z || run.watch.sample(now, Until::Done, state, depth).is_some() {
                lab.wait(ctx, &mut run, Until::Done)?;
                break;
            }
            std::thread::sleep(lab.timing.round);
        }
    }
    let rec = lab.finish(ctx, run)?;
    Ok(Some(During { rec, calls, states, commands, ppf }))
}

pub(crate) fn hs_stop() -> Case {
    case("hs-stop", "@a\n@h\nSTOP\n@z\nENDDO", Expect::HaltAfter("h"), 10)
}
pub(crate) fn hs_err() -> Case {
    case("hs-err", "@a\n@h\nT32PROBE_NO_SUCH_COMMAND\n@z\nENDDO", Expect::HaltAfter("h"), 10)
}
pub(crate) fn hs_wait() -> Case {
    case("hs-wait", "@a\n@w\nWAIT 5.s\n@z\nENDDO", Expect::End, 15)
}
pub(crate) fn hs_unwind() -> Case {
    case("hs-unwind", "@a\n@w\nWAIT 3.s\n@z\nENDDO", Expect::End, 15)
}
pub(crate) fn hs_save() -> Case {
    let body = "@a\n@w\nData.SAVE.Binary \"{S}\\hs-save.bin\" VM:{L}++0x007FFFFF\n@z\nENDDO";
    case("hs-save", body, Expect::End, 30).outputs(&["hs-save.bin"])
}
pub(crate) fn hs_open() -> Case {
    let body = "@a\nOPEN #1 \"{S}\\hs-open.txt\" /Create\nWRITE #1 \"x\"\n@h\nSTOP\nCLOSE #1\n@z\nENDDO";
    case("hs-open", body, Expect::HaltAfter("h"), 10).outputs(&["hs-open.txt"])
}
pub(crate) fn hs_glob() -> Case {
    let body = "@a\nGLOBAL &{q}_hg\n&{q}_hg=\"hg-{run}\"\n@h\nSTOP\n@z\nENDDO";
    case("hs-glob", body, Expect::HaltAfter("h"), 10)
}
pub(crate) fn hs_read() -> Case {
    let body = "@a\n@ask kept \"&{q}_hg\" == \"hg-{run}\"\n@ask empty \"&{q}_hg\" == \"\"\n@z\n@put 0 \"[&{q}_hg]\"\n@v\nENDDO";
    case("hs-read", body, Expect::End, 10)
}

/// How scripts stop and what is left after `END`: `STOP`, an uncaught error, `T32_Stop` during a
/// `WAIT` and during a long `SAVE`, unwinding, an open file and a global. Passes when every case
/// left an empty stack.
pub fn script_halt(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "halt", LabSizes { large: Some(LARGE), extra: Vec::new() })?;
    let result = script_halt_cases(ctx, &mut lab);
    let closed = lab.close(ctx);
    result.and(closed)
}

fn script_halt_cases(ctx: &mut ProbeCtx<'_>, lab: &mut Lab) -> ProbeResult {
    let dash = || "-".to_string();
    let mut finished: Vec<CaseRecord> = Vec::new();

    let mut stop = dash();
    if let Some(mut run) = lab.begin(ctx, &hs_stop())? {
        let seen = lab.wait(ctx, &mut run, Until::Done)?;
        stop = match seen {
            Seen::End(Ending::Halted { .. }) => lab.observe(ctx, &mut run, Duration::from_secs(1))?,
            _ => run.watch.last().unwrap_or_else(dash),
        };
        finished.push(lab.finish(ctx, run)?);
    }
    ctx.text("stop", stop);

    let (mut err, mut errmsg, mut errid, mut reset) = (dash(), "none", "none".to_string(), dash());
    if let Some(mut run) = lab.begin(ctx, &hs_err())? {
        let seen = lab.wait(ctx, &mut run, Until::Done)?;
        err = run.watch.last().unwrap_or_else(dash);
        if let Some(d) = run.diags.first() {
            let full = |v: Option<&str>| v.is_some_and(|t| !t.is_empty());
            errmsg = match (full(d.function(fnc::ERROR_MESSAGE)), full(d.evaluated(fnc::ERROR_MESSAGE))) {
                (true, true) => "both",
                (true, false) => "fn",
                (false, true) => "eval",
                (false, false) => "none",
            };
            errid = d.function(ERROR_ID).filter(|t| !t.is_empty()).unwrap_or("none").to_string();
        }
        if matches!(seen, Seen::End(Ending::Halted { .. })) {
            soft(ctx.session.cmd(cmd::ERROR_RESET))?;
            let after = lab.diag(ctx, "hs-err.reset")?;
            reset = if after.function(fnc::ERROR_MESSAGE) == Some("") { "clears" } else { "keeps" }.to_string();
        }
        finished.push(lab.finish(ctx, run)?);
    }
    ctx.text("err", err);
    ctx.text("errmsg", errmsg);
    ctx.text("errid", errid);
    ctx.text("reset", reset);

    let (mut t32stop, mut wait_cut, mut end1) = (dash(), dash(), dash());
    if let Some(mut run) = lab.begin(ctx, &hs_wait())? {
        if lab.wait(ctx, &mut run, Until::Marker("w"))? == Seen::Reached {
            lab.pause(Duration::from_millis(500));
            let (token, took) = lab.stop(ctx, &mut run)?;
            ctx.measure("t32stop_ms", Value::Seconds(took.as_secs_f64()));
            t32stop = token;
            let after = lab.observe(ctx, &mut run, Duration::from_millis(300))?;
            ctx.measure("after_stop", Value::Text(after));
            end1 = match lab.end(ctx, 1)? {
                0 => "ok".to_string(),
                n => format!("depth{n}"),
            };
        }
        let rec = lab.finish(ctx, run)?;
        wait_cut = if rec.has("z") { "no" } else { "yes" }.to_string();
        finished.push(rec);
    }
    let stop_idle = match stack_depth(ctx)? {
        0 => stop_token(ctx.session.stop_practice())?,
        _ => dash(),
    };
    ctx.text("t32stop", t32stop);
    ctx.text("stop_idle", stop_idle);
    ctx.text("wait_cut", wait_cut);
    ctx.text("end1", end1);

    let mut unwind = dash();
    if let Some(mut run) = lab.begin(ctx, &hs_unwind())? {
        if lab.wait(ctx, &mut run, Until::Marker("w"))? == Seen::Reached {
            unwind = match soft(t32stage::unwind_practice(ctx.session))? {
                Some(CleanupOutcome::Done) => "done".to_string(),
                Some(other) => other.label(),
                None => "refused".to_string(),
            };
            let depth = stack_depth(ctx)?;
            ctx.measure("unwind_depth", Value::Text(depth.to_string()));
        }
        finished.push(lab.finish(ctx, run)?);
    }
    ctx.text("unwind", unwind);

    let mut stop_save = "n/a".to_string();
    if let Some(l) = lab.l {
        stop_save = dash();
        ctx.session.mem_write(&l.addr(), &pattern(u64::from(LARGE) + 1, LARGE as usize))?;
        if let Some(mut run) = lab.begin(ctx, &hs_save())? {
            let mut took = None;
            if lab.wait(ctx, &mut run, Until::Marker("w"))? == Seen::Reached {
                lab.pause(Duration::from_millis(50));
                let (token, t) = lab.stop(ctx, &mut run)?;
                ctx.measure("stop_save_status", Value::Text(token));
                took = Some(t);
                lab.wait(ctx, &mut run, Until::Done)?;
            }
            let rec = lab.finish(ctx, run)?;
            let size = soft(transfer::file_size(ctx.session, &lab.path("hs-save.bin")?))?;
            ctx.measure("stop_save_size", Value::Text(format!("{size:?}")));
            stop_save = match (rec.has("z"), size, took) {
                (true, _, _) => "after".into(),
                (false, s, Some(_)) if s != Some(u64::from(LARGE)) => "cut".into(),
                (false, _, Some(t)) => format!("{}ms", t.as_millis()),
                (false, _, None) => dash(),
            };
            finished.push(rec);
        }
    }
    ctx.text("stop_save", stop_save);

    let mut open_closed = dash();
    let rec = lab.run(ctx, &hs_open())?;
    if rec.ran() {
        let txt = lab.path("hs-open.txt")?;
        let removed = soft(ctx.session.cmd(&cmd::rm(&txt)))?.is_some();
        let exists = ctx.exists_fn();
        let gone = !ctx.session.eval(&exists.call(&txt))?.into_bool(exists.name())?;
        open_closed = if removed && gone { "yes" } else { "no" }.to_string();
        finished.push(rec);
    }
    ctx.text("open_closed", open_closed);

    let hg = MacroName::new(&format!("{}_hg", lab.q()))?;
    ctx.push(Undo::ClearMacros(vec![hg.clone()]))?;
    let glob = lab.run(ctx, &hs_glob())?;
    let read = lab.run(ctx, &hs_read())?;
    let base = ctx.session.tuning();
    for spelling in [MacroSpelling::Bare, MacroSpelling::Ampersand] {
        ctx.session.set_tuning(base.with_macro_spelling(spelling));
        let got = soft(ctx.session.macro_get_payload(&hg))?;
        ctx.measure(&format!("glob_exp_{}", spelling.tag()), Value::Text(got.map_or("refused".into(), |p| hex(&p))));
    }
    ctx.session.set_tuning(base);
    ctx.measure("globals_left", Value::Count(glob.has("h").into()));
    let glob_end = match (read.answer("kept"), read.answer("empty")) {
        _ if !read.ran() => "-",
        (Answer::Yes, _) => "kept",
        (_, Answer::Yes) => "empty",
        _ => "lost",
    };
    ctx.text("glob_end", glob_end);
    finished.extend([glob, read].into_iter().filter(CaseRecord::ran));

    match finished.iter().find(|r| r.depth != 0) {
        Some(r) => Err(ProbeFail::Check(format!("END left depth {} after {}", r.depth, r.id))),
        None => Ok(()),
    }
}

const ERROR_ID: &str = "ERROR.ID(\"PLAIN\")";

/// `<key>=<p50>/<max>ms` over every call kind, and each kind on its own in the JSON.
fn rcl_tokens(ctx: &mut ProbeCtx<'_>, key: &str, d: Option<&During>) {
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let mut all: Vec<Duration> = d.map(|d| d.calls.iter().map(|(_, t)| *t).collect()).unwrap_or_default();
    let token = Stats::from_samples(&mut all).map_or("-".into(), |s| format!("{:.2}/{:.2}ms", ms(s.p50), ms(s.max)));
    ctx.text(key, token);
    for kind in ["state", "depth", "true", "ppf", "exp", "mem", "cmd"] {
        let mut v: Vec<Duration> =
            d.map(|d| d.calls.iter().filter(|(k, _)| *k == kind).map(|(_, t)| *t).collect()).unwrap_or_default();
        if let Some(s) = Stats::from_samples(&mut v) {
            ctx.measure(&format!("{key}.{kind}"), Value::Text(s.compact()));
        }
    }
}

#[cfg(test)]
#[path = "scriptlab_tests.rs"]
pub(crate) mod scriptlab_tests;
