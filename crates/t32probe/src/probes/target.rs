// SPDX-License-Identifier: Apache-2.0
//! Opt-in probes that read a halted target or load code into it. Each target command and
//! function is asked with `PRACTICE.*.AVAILable` first; every setting they change is a
//! memory-only `Revert`, applied right after the step that measures it.

use std::time::Duration;

use t32rcl::{AccessClass, MemAddr, TargetState, Tuning};
use t32stage::{StagePath, Symbol, Undo, VmRegion, transfer};

use super::{
    lab::{eval_text, raw_note},
    optin::{Verified, revert, unverified},
    scratch_regions, soft, stage_root, workdir,
};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::{SkipReason, Value},
};

/// Registers `target-read` asks for: X0..X30, then these.
const NAMED_REGS: [&str; 9] =
    ["SP", "PC", "CPSR", "ESR_EL1", "FAR_EL1", "ELR_EL1", "SPSR_EL1", "SCTLR_EL1", "MPIDR_EL1"];
/// System registers among them: `spr=fn` when `Register()` answers one.
const SPR: [&str; 6] = ["ESR_EL1", "FAR_EL1", "ELR_EL1", "SPSR_EL1", "SCTLR_EL1", "MPIDR_EL1"];
/// One read at the largest chunk the client allows (`Tuning::MAX_CHUNK`).
const CHUNK: usize = Tuning::MAX_CHUNK;
/// Room for one printed file read back through `VM:`.
const PRINT_LEN: u32 = 256 * 1024;
const EPOCH_N: usize = 20;

fn ms(d: Duration) -> String {
    d.as_millis().to_string()
}

/// MB/s of `bytes` in `d`; `-` when no time passed.
fn rate(bytes: u64, d: Duration) -> String {
    if d.is_zero() { "-".into() } else { format!("{:.2}", bytes as f64 / 1e6 / d.as_secs_f64()) }
}

/// The address part of an address PowerView prints, `P:0x1000` or `0x1000`.
fn address(text: &str) -> Option<u64> {
    let t = text.rsplit(':').next()?.trim();
    let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))?;
    u64::from_str_radix(&hex.replace('_', ""), 16).ok()
}

/// Reads of a halted target and windows printed to a file; on a running target only the memory
/// range, the epoch reads and the printing. Nothing on the target changes; the printer file is
/// closed through a `Revert` registered right after `PRinTer.OPEN` succeeded.
pub fn target_read(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let input = ctx.env.cfg.gates.target_read.clone().expect("gate opens only with a symbol");
    let state = ctx.session.target_state()?;
    let stopped = matches!(state, TargetState::Halted | TargetState::Stopped);
    if !stopped && state != TargetState::Running {
        return Err(ProbeFail::Skip(SkipReason::TargetNotUp { state: state.name().into() }));
    }
    let root = stage_root(ctx)?;
    let dir = workdir(ctx, &root, "tr")?;
    let window = ctx.env.scratch_window();
    let range = input.read_range.filter(|(_, n)| window.carve(&[PRINT_LEN, *n]).is_ok());
    let sizes: Vec<u32> = [PRINT_LEN].into_iter().chain(range.map(|(_, n)| n)).collect();
    let regions = scratch_regions(ctx, &sizes)?;
    let mut v = Verified::default();
    let mut clean = true;
    let s = &input.symbol;
    let na = || "n/a".to_string();

    let (regs, spr, chunk, xlate, var) = if stopped {
        let (regs, spr) = registers(ctx, &mut v)?;
        (regs, spr, chunk_read(ctx, &mut v, s)?, translate(ctx, &mut v, input.va)?, var_reads(ctx, &mut v, s)?)
    } else {
        ctx.measure("state", Value::Text(state.name().into()));
        (na(), na(), na(), na(), na())
    };
    ctx.text("regs", regs);
    ctx.text("spr", spr);
    ctx.text("chunk", chunk);
    ctx.text("xlate", xlate);
    ctx.text("var", var);
    let mem = match input.read_range {
        Some((a, n)) => memory(ctx, &dir, a, n, regions.get(1))?,
        None => na(),
    };
    ctx.text("mem", mem);
    epoch(ctx, &mut v)?;
    let (print, frame) = print(ctx, &mut v, &dir, &regions[0], s, input.per_section.as_deref(), &mut clean)?;
    ctx.text("print", print);
    ctx.text("frame", frame);
    ctx.check(clean, || "closing the printer file did not apply".into())
}

/// `Register(<r>)` for every register of the list: `<answered>/<asked>`, and `fn` when a system
/// register answered.
fn registers(ctx: &mut ProbeCtx<'_>, v: &mut Verified) -> Result<(String, String), ProbeFail> {
    if !v.function(ctx, "Register")? {
        return Ok((unverified("Register"), unverified("Register")));
    }
    let names: Vec<String> =
        (0..=30).map(|k| format!("X{k}")).chain(NAMED_REGS.iter().map(|r| r.to_string())).collect();
    let (mut failed, mut spr) = (Vec::new(), false);
    for r in &names {
        match eval_text(ctx, &format!("Register({r})"))? {
            Some(_) => spr |= SPR.contains(&r.as_str()),
            None => failed.push(r.clone()),
        }
    }
    ctx.measure("regs_failed", Value::Text(failed.join(",")));
    Ok((format!("{}/{}", names.len() - failed.len(), names.len()), if spr { "fn" } else { "none" }.into()))
}

/// 64 bytes at `sYmbol.BEGIN(<s>)` in `D:`, then one read of `CHUNK` bytes: its milliseconds.
fn chunk_read(ctx: &mut ProbeCtx<'_>, v: &mut Verified, s: &Symbol) -> Result<String, ProbeFail> {
    if !v.function(ctx, "sYmbol.BEGIN")? {
        return Ok(unverified("sYmbol.BEGIN"));
    }
    let begin = eval_text(ctx, &format!("sYmbol.BEGIN({})", s.as_str()))?;
    ctx.measure("begin", Value::Text(begin.clone().unwrap_or_else(|| "refused".into())));
    let Some(a) = begin.as_deref().and_then(address) else { return Ok("?".into()) };
    let at = MemAddr::new(AccessClass::parse("D")?, a);
    let small = soft(ctx.session.mem_read(&at, 64))?;
    ctx.measure("d64", Value::Text(small.as_deref().map_or("refused".into(), super::hex)));
    let base = ctx.session.tuning();
    ctx.session.set_tuning(Tuning::new(CHUNK, base.write_chunk())?.with_macro_spelling(base.macro_spelling()));
    let t = ctx.session.now();
    let r = soft(ctx.session.mem_read(&at, CHUNK));
    let took = ctx.session.now().saturating_duration_since(t);
    ctx.session.set_tuning(base);
    Ok(match r? {
        Some(_) => ms(took),
        None => "refused".into(),
    })
}

/// `MMU.PHYSICAL` and `ADDRESS.PHYSICAL` of `--va` and of 0: the first function that answered.
fn translate(ctx: &mut ProbeCtx<'_>, v: &mut Verified, va: Option<u64>) -> Result<String, ProbeFail> {
    let Some(va) = va else { return Ok("n/a".into()) };
    let mut first = None;
    for f in ["MMU.PHYSICAL", "ADDRESS.PHYSICAL"] {
        if !v.function(ctx, f)? {
            continue;
        }
        let at = eval_text(ctx, &format!("{f}(D:0x{va:X})"))?;
        let zero = eval_text(ctx, &format!("{f}(D:0x0)"))?;
        ctx.measure(&format!("xlate.{f}"), Value::Text(format!("va={at:?} zero={zero:?}")));
        if at.is_some() && first.is_none() {
            first = Some(f);
        }
    }
    Ok(first.unwrap_or("none").into())
}

/// `Var.VALUE`, `Var.ADDRESS` and `Var.SIZEOF` of the symbol: `<answered>/3`.
fn var_reads(ctx: &mut ProbeCtx<'_>, v: &mut Verified, s: &Symbol) -> Result<String, ProbeFail> {
    let mut answered = 0;
    for f in ["Var.VALUE", "Var.ADDRESS", "Var.SIZEOF"] {
        let r = if v.function(ctx, f)? { eval_text(ctx, &format!("{f}({})", s.as_str()))? } else { None };
        answered += usize::from(r.is_some());
        ctx.measure(&format!("var.{f}"), Value::Text(r.unwrap_or_else(|| "refused".into())));
    }
    Ok(format!("{answered}/3"))
}

/// `--read-range` over RCL, then saved to a file under a data-sized budget and read back
/// through `VM:`: `<rcl MB/s>/<file MB/s>`.
fn memory(
    ctx: &mut ProbeCtx<'_>,
    dir: &StagePath,
    a: u64,
    n: u32,
    region: Option<&VmRegion>,
) -> Result<String, ProbeFail> {
    let at = MemAddr::new(AccessClass::parse("D")?, a);
    let t = ctx.session.now();
    let rcl = soft(ctx.session.mem_read(&at, n as usize))?;
    let rcl_time = ctx.session.now().saturating_duration_since(t);
    let Some(rcl) = rcl else { return Ok("refused/-".into()) };
    let rcl_rate = rate(n.into(), rcl_time);
    let Some(region) = region else {
        ctx.measure("mem_file", Value::Text("the range does not fit the scratch window".into()));
        return Ok(format!("{rcl_rate}/-"));
    };
    let p = dir.join("m.bin")?;
    ctx.push(Undo::RemoveFile(p.clone()))?;
    ctx.session.take_trace();
    let t = ctx.session.now();
    let budget = transfer::data_budget(ctx.session, n.into());
    let save = format!("Data.SAVE.Binary {} D:0x{a:X}++0x{:X}", p.quoted(), n - 1);
    let saved = soft(ctx.session.with_budget(budget).cmd(&save))?.is_some();
    let back = if saved { soft(transfer::get_file(ctx.session, &p, region, n))? } else { None };
    let took = ctx.session.now().saturating_duration_since(t);
    let ka = ctx.session.take_trace().keepalives_total;
    ctx.measure("mem_file", Value::Text(format!("ms={} ka={ka} same={}", ms(took), back.as_ref() == Some(&rcl))));
    Ok(match back {
        Some(_) => format!("{rcl_rate}/{}", rate(n.into(), took)),
        None => format!("{rcl_rate}/-"),
    })
}

/// `SYStem.CPU()`, `CORE()` and `Register(PC)`, 20 rounds: answers and distinct values.
fn epoch(ctx: &mut ProbeCtx<'_>, v: &mut Verified) -> Result<(), ProbeFail> {
    let mut reads: Vec<(&str, Vec<Option<String>>)> = Vec::new();
    for (f, expr) in [("SYStem.CPU", "SYStem.CPU()"), ("CORE", "CORE()"), ("Register", "Register(PC)")] {
        if v.function(ctx, f)? {
            reads.push((expr, Vec::new()));
        }
    }
    for _ in 0..EPOCH_N {
        for (expr, got) in reads.iter_mut() {
            got.push(eval_text(ctx, expr)?);
        }
    }
    for (expr, got) in reads {
        let mut distinct: Vec<&str> = got.iter().flatten().map(String::as_str).collect();
        distinct.sort_unstable();
        distinct.dedup();
        let answered = got.iter().flatten().count();
        let first = got.iter().flatten().next().cloned().unwrap_or_default();
        ctx.measure(
            &format!("epoch.{expr}"),
            Value::Text(format!("{answered}/{EPOCH_N} distinct={} first={first}", distinct.len())),
        );
    }
    Ok(())
}

/// Commands printed to `<dir>\<name>` between `PRinTer.OPEN` and `PRinTer.CLOSE`; `None` when
/// `PRinTer.OPEN` was refused, else the file's bytes when it exists and fits.
fn printed(
    ctx: &mut ProbeCtx<'_>,
    dir: &StagePath,
    region: &VmRegion,
    name: &str,
    commands: &[String],
    clean: &mut bool,
) -> Result<Option<Option<Vec<u8>>>, ProbeFail> {
    let p = dir.join(name)?;
    ctx.push(Undo::RemoveFile(p.clone()))?;
    if soft(ctx.session.cmd(&format!("PRinTer.OPEN {}", p.quoted())))?.is_none() {
        return Ok(None);
    }
    ctx.push(Undo::Revert { what: "printer".into(), commands: vec!["PRinTer.CLOSE".into()] })?;
    let open = eval_text(ctx, "PRINTER.FILENAME()")?;
    ctx.measure(&format!("prn_open.{name}"), Value::Text(open.unwrap_or_else(|| "refused".into())));
    let mut sent = Vec::new();
    for c in commands {
        sent.push(if soft(ctx.session.cmd(c))?.is_some() { "ok" } else { "refused" });
    }
    ctx.measure(&format!("printed.{name}"), Value::Text(sent.join(",")));
    revert(ctx, "printer", clean);
    let e = ctx.exists_fn();
    if !ctx.session.eval(&e.call(&p))?.into_bool(e.name())? {
        return Ok(Some(None));
    }
    let Some(size) = soft(transfer::file_size(ctx.session, &p))? else { return Ok(Some(None)) };
    ctx.measure(&format!("size.{name}"), Value::Bytes(size));
    if size == 0 || size > u64::from(region.len()) {
        return Ok(Some(None));
    }
    Ok(Some(soft(transfer::get_file(ctx.session, &p, region, size as u32))?))
}

/// Windows printed with `WinPrint` into files: `print` from the main file, `frame` from two
/// files of one `Frame.view` each. Last, because a refused `PRinTer.OPEN` ends it.
fn print(
    ctx: &mut ProbeCtx<'_>,
    v: &mut Verified,
    dir: &StagePath,
    region: &VmRegion,
    s: &Symbol,
    per_section: Option<&str>,
    clean: &mut bool,
) -> Result<(String, String), ProbeFail> {
    let na = || "n/a".to_string();
    for c in ["PRinTer.OPEN", "WinPrint"] {
        if !v.command(ctx, c)? {
            return Ok((unverified(c), na()));
        }
    }
    let before = eval_text(ctx, "PRINTER.FILENAME()")?;
    ctx.measure("prn_before", Value::Text(before.unwrap_or_else(|| "refused".into())));
    let mut main: Vec<String> = vec![
        "WinPrint.VERSION.SOFTWARE".into(),
        "WinPrint.PMACRO.list".into(),
        format!("WinPrint.List {}", s.as_str()),
    ];
    main.extend(per_section.map(|p| format!("WinPrint.PER.view {p}")));
    let Some(text) = printed(ctx, dir, region, "tr.lst", &main, clean)? else { return Ok(("refused".into(), na())) };
    let print = match &text {
        None => "nofile",
        Some(raw) if raw.contains(&0) || std::str::from_utf8(raw).is_err() => "binary",
        Some(_) => "text",
    };
    if let Some(raw) = &text {
        ctx.measure("print_raw", Value::Text(raw_note(raw)));
    }
    let frame_cmd = ["WinPrint.Frame.view /Locals /Caller".to_string()];
    let f1 = printed(ctx, dir, region, "tf1.lst", &frame_cmd, clean)?.flatten();
    let f2 = printed(ctx, dir, region, "tf2.lst", &frame_cmd, clean)?.flatten();
    let frame = match (f1, f2) {
        (Some(a), Some(b)) => if a == b { "stable" } else { "diff" }.to_string(),
        _ => na(),
    };
    Ok((print.into(), frame))
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod target_tests;
