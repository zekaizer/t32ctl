// SPDX-License-Identifier: Apache-2.0
//! fn-table: which commands and functions this build offers, and what PowerView returns for a
//! call it refuses. No script runs.

use std::time::{Duration, Instant};

use t32rcl::{CallBudget, ErrorClass, FuncValue, MacroName, RemoteError};
use t32stage::{RelPath, Undo, practice::cmd};

use super::{macro_prefix, scratch_regions, stage_root};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::Value,
    stats::Stats,
};

/// Commands asked with `PRACTICE.CoMmanD.AVAILable(<c>)` (ide_func).
pub(crate) const COMMANDS: &[&str] = &[
    "Break.Set",
    "Break.Delete",
    "Go",
    "Go.Return",
    "Go.Up",
    "Step",
    "Step.Over",
    "Step.Asm",
    "Step.Out",
    "Frame.view",
    "Var.Assign",
    "Register.Set",
    "CORE.select",
    "PER.view",
    "WinPrint",
    "PRinTer.OPEN",
    "STOre",
    "FLASHFILE",
    "Data.LOAD.Elf",
    "Data.Set",
    "SETUP.BAKfile",
    "PMACRO.EXPLICIT",
    "PMACRO.IMPLICITPRIVATE",
    "TAR",
    "ZIP",
    "AREA.Create",
    "CONTinue",
    "Eval.ENDDO",
];

/// Functions asked with `PRACTICE.FUNCtion.AVAILable(<f>)` (ide_func).
pub(crate) const FUNCTIONS: &[&str] = &[
    "Register",
    "CORE",
    "CORE.NUMBER",
    "SYStem.CPU",
    "SYStem.Up",
    "SYStem.MODE",
    "STATE.RUN",
    "STATE.TARGET",
    "sYmbol.EXIST",
    "sYmbol.BEGIN",
    "sYmbol.END",
    "sYmbol.SIZEOF",
    "sYmbol.FUNCTION",
    "sYmbol.NAME",
    "sYmbol.SECRANGE",
    "Var.VALUE",
    "Var.ADDRESS",
    "Var.SIZEOF",
    "Var.TYPEOF",
    "Var.STRing",
    "ADDRESS.OFFSET",
    "MMU.PHYSICAL",
    "ADDRESS.PHYSICAL",
    "Data.STRing",
    "Data.String",
    "OS.PresentPracticeDirectory",
    "OS.PPD",
    "OS.PPF",
    "OS.FILE.ABSPATH",
    "OS.FIRSTFILE",
    "OS.NEXTFILE",
    "PRINTER.FILENAME",
    "PRACTICE.ARG",
    "PRACTICE.ARG.SIZE",
    "ERROR.MESSAGE",
    "ERROR.CMDLINE",
    "ERROR.OCCURRED",
    "ERROR.ID",
    "ERROR.FIRSTID",
    "EVAL.STRing",
    "EVAL.TYPE",
    "AREA.LINE",
    "STRing.LENgth",
    "STRing.ESCapeQuotes",
    "FORMAT.Decimal",
    "FOUND",
    "PRACTICE.GLOBAL.VALUE",
    "PRACTICE.GLOBAL.EXIST",
    "PRACTICE.MACRO.VALUE",
    "InterCom.PORT",
];

/// Total `PRINT "x..."` lengths tried; the last is the client's own limit.
pub(crate) const CMD_LENGTHS: [usize; 3] = [1000, 2000, 2037];

/// Commands refused on purpose, by label.
pub(crate) const BAD_COMMANDS: [&str; 1] = ["T32PROBE_NOPE"];
/// Functions refused on purpose, by label.
pub(crate) const BAD_FUNCTIONS: [(&str, &str); 4] = [
    ("fnope", "T32PROBE.NOPE()"),
    ("size1", "OS.FILE.SIZE(1.)"),
    ("symexist", "sYmbol.EXIST(t32probe_nope)"),
    ("pc", "Register(PC)"),
];

/// Read 20 times each for the epoch and CPU reads; `None` is `GetState`.
pub(crate) const EPOCH: [Option<&str>; 7] = [
    None,
    Some("SYStem.Up()"),
    Some("SYStem.MODE()"),
    Some("CORE()"),
    Some("CORE.NUMBER()"),
    Some("Register(PC)"),
    Some("SYStem.CPU()"),
];
pub(crate) const EPOCH_N: usize = 20;

pub(crate) const PATHS: [&str; 7] = [
    "OS.PWD()",
    "OS.FILE.ABSPATH(\"\\x.cmm\")",
    "OS.FILE.ABSPATH(\"x.cmm\")",
    "OS.FILE.ABSPATH(\"~~~/x\")",
    "OS.PTD()",
    "InterCom.PORT()",
    "PRINTER.FILENAME()",
];

/// An answer, or PowerView's refusal of the call.
pub(crate) type Reply<T> = Result<T, RemoteError>;

/// A refusal becomes `Err(remote)`; any other error aborts the probe.
fn reply<T>(r: Result<T, t32rcl::Error>) -> Result<Reply<T>, ProbeFail> {
    match r {
        Ok(v) => Ok(Ok(v)),
        Err(e) => match e.remote() {
            Some(remote) => Ok(Err(remote.clone())),
            None => Err(e.into()),
        },
    }
}

pub(crate) fn func(ctx: &mut ProbeCtx<'_>, expr: &str) -> Result<Reply<FuncValue>, ProbeFail> {
    reply(ctx.session.eval(expr))
}

pub(crate) fn command(ctx: &mut ProbeCtx<'_>, text: &str) -> Result<Reply<()>, ProbeFail> {
    reply(ctx.session.cmd(text))
}

/// A value as PowerView prints it: `TRUE`/`FALSE`, text unquoted.
pub(crate) fn text_of(v: &FuncValue) -> String {
    match v {
        FuncValue::Bool(true) => "TRUE".into(),
        FuncValue::Bool(false) => "FALSE".into(),
        FuncValue::Text(t) => t.clone(),
        FuncValue::Empty => String::new(),
        other => other.to_string(),
    }
}

/// `text_of` the value, or `err<status>` for a refusal.
pub(crate) fn shown(r: &Reply<FuncValue>) -> String {
    r.as_ref().map_or_else(|e| format!("err{}", e.code), text_of)
}

/// Availability, `&` in functions, command length, the EVAL slot, refusals with their error ids,
/// epoch and CPU reads, path functions, and last an option PowerView may answer with a dialog.
/// Only a connection-fatal error fails it.
pub fn fn_table(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (yes, asked, true_names) = availability(ctx)?;
    ctx.text("avail", format!("{yes}/{asked}"));
    let amp = fn_amp(ctx)?;
    ctx.text("fn_amp", amp);
    let mut longest = 0;
    for total in CMD_LENGTHS {
        let r = command(ctx, &format!("PRINT \"{}\"", "x".repeat(total - 8)))?;
        ctx.measure(
            &format!("cmdlen.{total}"),
            Value::Text(r.as_ref().map_or_else(|e| format!("err{}", e.code), |()| "ok".into())),
        );
        if r.is_ok() {
            longest = total;
        }
    }
    ctx.text("cmdlen", longest.to_string());
    let eval = eval_slot(ctx)?;
    ctx.text("eval", eval);

    let region = scratch_regions(ctx, &[4096])?[0];
    let root = stage_root(ctx)?;
    let none = root.path(RelPath::new(&format!("probe-{}-none/none.bin", ctx.env.cfg.run_id))?);
    let mut errid = false;
    let mut calls: Vec<(String, bool, String)> =
        BAD_COMMANDS.iter().map(|c| ("nope".to_string(), true, c.to_string())).collect();
    calls.push(("load".into(), true, cmd::data_load_binary(&none, region.start(), false)));
    calls.extend(BAD_FUNCTIONS.iter().map(|(l, f)| (l.to_string(), false, f.to_string())));
    for (label, is_cmd, text) in &calls {
        for n in 1..=2 {
            let r = if *is_cmd { command(ctx, text)?.map(|()| None) } else { func(ctx, text)?.map(Some) };
            errid |= failure(ctx, &format!("{label}.{n}"), r)?;
        }
    }
    ctx.text("errid", if errid { "yes" } else { "no" });

    let (cpu, pc) = epoch(ctx)?;
    ctx.text("cpu", cpu);
    ctx.text("pc_down", pc);
    let bslash = paths(ctx)?;
    ctx.text("bslash", bslash);
    let has = |n: &str| true_names.contains(&n);
    ctx.text("pgv", if has("PRACTICE.GLOBAL.VALUE") { "yes" } else { "no" });
    ctx.text("list", if has("OS.FIRSTFILE") && has("OS.NEXTFILE") { "yes" } else { "no" });
    bogus_option(ctx, region.start())
}

/// Every name of `COMMANDS` and `FUNCTIONS`: how many are `TRUE`, how many were asked, which.
fn availability(ctx: &mut ProbeCtx<'_>) -> Result<(usize, usize, Vec<&'static str>), ProbeFail> {
    let mut true_names = Vec::new();
    let mut asked = 0;
    for (kind, names) in [("CoMmanD", COMMANDS), ("FUNCtion", FUNCTIONS)] {
        for &n in names {
            let r = func(ctx, &format!("PRACTICE.{kind}.AVAILable({n})"))?;
            let t = shown(&r);
            asked += 1;
            if t == "TRUE" {
                true_names.push(n);
            }
            ctx.measure(&format!("avail.{n}"), Value::Text(t));
        }
    }
    Ok((true_names.len(), asked, true_names))
}

/// `&name` inside a function argument sent over ExecuteFunction: `expanded` when
/// `STRing.LENgth` counts the value, `literal` when it counts the name.
fn fn_amp(ctx: &mut ProbeCtx<'_>) -> Result<&'static str, ProbeFail> {
    let q = macro_prefix(&ctx.env.cfg.run_id);
    let (fx, fy) = (MacroName::new(&format!("{q}_fx"))?, MacroName::new(&format!("{q}_fy"))?);
    ctx.push(Undo::ClearMacros(vec![fx.clone(), fy.clone()]))?;
    let set = reply(ctx.session.macro_set(&fx, "va"))?.is_ok();
    let len = |ctx: &mut ProbeCtx<'_>, m: &MacroName| func(ctx, &format!("STRing.LENgth(\"{}\")", m.amp()));
    let x = len(ctx, &fx)?;
    let global = command(ctx, &format!("GLOBAL {}", fy.amp()))?.is_ok();
    let assigned = command(ctx, &format!("{}=\"va\"", fy.amp()))?.is_ok();
    let y = len(ctx, &fy)?;
    ctx.measure(
        "fn_amp_raw",
        Value::Text(format!("exp_set={set} x={} global={global} assign={assigned} y={}", shown(&x), shown(&y))),
    );
    let class = |r: &Reply<FuncValue>, m: &MacroName| match r {
        Ok(FuncValue::Int(2)) => "expanded",
        Ok(FuncValue::Int(n)) if *n == m.amp().len() as i64 => "literal",
        _ => "other",
    };
    Ok(match (class(&x, &fx), class(&y, &fy)) {
        ("expanded", "expanded") => "expanded",
        ("literal", "literal") => "literal",
        _ => "mixed",
    })
}

/// The `EVAL` result slot after string, numeric and command calls: `keep`, the first call that
/// changed it, or `n/a` when `EVAL` itself did not fill it.
fn eval_slot(ctx: &mut ProbeCtx<'_>) -> Result<String, ProbeFail> {
    let _ = command(ctx, "EVAL \"t32q-\"+\"ok\"")?;
    let base = three_reads(ctx, "eval")?;
    if base.as_deref() != Ok("t32q-ok") {
        return Ok("n/a".into());
    }
    let mut token = None;
    for label in ["fnstr", "fnnum", "print"] {
        let _ = match label {
            "fnstr" => func(ctx, "OS.PTD()")?.map(drop),
            "fnnum" => func(ctx, "VERSION.BUILD()")?.map(drop),
            _ => command(ctx, "PRINT \"x\"")?,
        };
        if three_reads(ctx, label)? != base && token.is_none() {
            token = Some(format!("cleared-by:{label}"));
        }
    }
    Ok(token.unwrap_or_else(|| "keep".into()))
}

/// `T32_EvalGetString`, `EVAL.STRing()`, `EVAL.TYPE()`, recorded as `eval.<label>`; the first.
fn three_reads(ctx: &mut ProbeCtx<'_>, label: &str) -> Result<Reply<String>, ProbeFail> {
    let slot = reply(ctx.session.eval_last_string())?;
    let (s, t) = (func(ctx, "EVAL.STRing()")?, func(ctx, "EVAL.TYPE()")?);
    let slot_text = slot.as_ref().map_or_else(|e| format!("err{}", e.code), |v| format!("{v:?}"));
    ctx.measure(
        &format!("eval.{label}"),
        Value::Text(format!("slot={slot_text} string={} type={}", shown(&s), shown(&t))),
    );
    Ok(slot)
}

/// Records `err.<label>` and, after a refusal, `ERROR.ID`/`ERROR.FIRSTID` and `ERROR.RESet`.
/// `true` when `ERROR.ID` named the refusal.
fn failure(ctx: &mut ProbeCtx<'_>, label: &str, r: Reply<Option<FuncValue>>) -> Result<bool, ProbeFail> {
    let e = match r {
        Ok(v) => {
            let v = v.map_or_else(|| "ok".into(), |v| format!("ok {}", text_of(&v)));
            ctx.measure(&format!("err.{label}"), Value::Text(v));
            return Ok(false);
        }
        Err(e) => e,
    };
    let id = func(ctx, "ERROR.ID(\"PLAIN\")")?;
    let first = func(ctx, "ERROR.FIRSTID(\"PLAIN\")")?;
    let _ = command(ctx, cmd::ERROR_RESET)?;
    let text = format!(
        "{} st={} kind={} msg={:?} id={} first={}",
        e.op,
        e.code,
        e.kind,
        e.message.as_deref().unwrap_or(""),
        shown(&id),
        shown(&first)
    );
    ctx.measure(&format!("err.{label}"), Value::Text(text));
    Ok(id.is_ok_and(|v| !text_of(&v).is_empty()))
}

/// `EPOCH` reads, each `EPOCH_N` times: distinct answers in order and p50/max. The first
/// `SYStem.CPU()` and `Register(PC)` answers.
fn epoch(ctx: &mut ProbeCtx<'_>) -> Result<(String, String), ProbeFail> {
    let (mut cpu, mut pc) = (String::new(), String::new());
    for expr in EPOCH {
        let mut seen: Vec<String> = Vec::new();
        let mut times = Vec::with_capacity(EPOCH_N);
        for _ in 0..EPOCH_N {
            let t = Instant::now();
            let v = match expr {
                None => {
                    reply(ctx.session.target_state())?.map_or_else(|e| format!("err{}", e.code), |s| s.name().into())
                }
                Some(f) => shown(&func(ctx, f)?),
            };
            times.push(t.elapsed());
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
        let st = Stats::from_samples(&mut times).expect("EPOCH_N > 0");
        let ms = |d: Duration| format!("{:.2}", d.as_secs_f64() * 1e3);
        let name = expr.unwrap_or("GetState");
        ctx.measure(
            &format!("epoch.{name}"),
            Value::Text(format!("{} p50={}ms max={}ms", seen.join("|"), ms(st.p50), ms(st.max))),
        );
        match expr {
            Some("SYStem.CPU()") => cpu = seen[0].clone(),
            Some("Register(PC)") => pc = seen[0].clone(),
            _ => {}
        }
    }
    Ok((cpu, pc))
}

/// `PATHS` as given; where `OS.FILE.ABSPATH("\x.cmm")` lands: `pwd`, the drive `root`, `other`.
fn paths(ctx: &mut ProbeCtx<'_>) -> Result<&'static str, ProbeFail> {
    let mut got = Vec::new();
    for expr in PATHS {
        let r = func(ctx, expr)?;
        ctx.measure(&format!("path.{expr}"), Value::Text(shown(&r)));
        got.push(r.ok().map(|v| text_of(&v)));
    }
    Ok(bslash(got[0].as_deref(), got[1].as_deref()))
}

/// `pwd` when the leading-backslash path resolves below `OS.PWD()`, `root` for `X:\x.cmm`.
pub(crate) fn bslash(pwd: Option<&str>, abs: Option<&str>) -> &'static str {
    let Some(abs) = abs else { return "other" };
    let lower = abs.to_ascii_lowercase();
    let b = lower.as_bytes();
    if pwd.is_some_and(|p| !p.is_empty() && lower.starts_with(&p.to_ascii_lowercase())) {
        "pwd"
    } else if b.len() == 8 && b[0].is_ascii_alphabetic() && &lower[1..] == ":\\x.cmm" {
        "root"
    } else {
        "other"
    }
}

/// `Data.Set` with an option that does not exist, twice, each under a 3 s budget. A timeout is
/// recorded as `dialog?` and fails the probe; the executor reconnects.
fn bogus_option(ctx: &mut ProbeCtx<'_>, at: u32) -> ProbeResult {
    let line = format!("Data.Set VM:0x{at:08X} %Long 0x1 /T32PROBEBOGUS");
    let budget = CallBudget::idle(Duration::from_secs(3)).with_total(Duration::from_secs(3));
    for n in 1..=2 {
        let r = ctx.session.with_budget(budget).cmd(&line);
        match r {
            Err(e) if matches!(e.class(), ErrorClass::ConnectionFatal) => {
                ctx.measure(&format!("err.bogus.{n}"), Value::Text(format!("dialog? {e}")));
                ctx.text("bogus", "dialog?");
                return Err(e.into());
            }
            r => {
                let r = reply(r)?.map(|()| None);
                failure(ctx, &format!("bogus.{n}"), r)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "functions_tests.rs"]
mod tests;
