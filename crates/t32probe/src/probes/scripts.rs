// SPDX-License-Identifier: Apache-2.0
//! Probes about PRACTICE scripts: macro, bundle, script-error.

use std::time::{Duration, Instant};

use t32rcl::{MacroName, MacroSpelling};
use t32stage::{RunOutcome, Undo, practice::fnc, transfer};

use super::{bundle_of, exp_as, hex, macro_prefix, ok, pattern, run_entry, soft};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::{Value, key},
};

fn name(s: &str) -> MacroName {
    MacroName::new(s).expect("constant macro name")
}

/// EXP 2 global macros: the wire spelling, whether `GLOBAL` must come first, redeclaration,
/// value size, and characters that survive. Then, never deciding the verdict: whether the two
/// spellings name one macro, empty and undeclared names, command-line assignment, and values over
/// 4 KiB.
pub fn macro_probe(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (a, b) = (name("t32ctl_probe_a"), name("t32ctl_probe_b"));
    ctx.push(Undo::ClearMacros(vec![a.clone(), b.clone()]))?;
    let base = ctx.session.tuning();
    let try_spellings = |ctx: &mut ProbeCtx<'_>| -> Result<Option<MacroSpelling>, ProbeFail> {
        for s in [MacroSpelling::Bare, MacroSpelling::Ampersand] {
            ctx.session.set_tuning(base.with_macro_spelling(s));
            let set = soft(ctx.session.macro_set(&a, "v1"))?.is_some();
            let got = if set { soft(ctx.session.macro_get(&a))? } else { None };
            ctx.measure(&format!("spelling_{}", s.tag()), Value::Text(format!("set={set} get={got:?}")));
            if got.as_deref() == Some("v1") {
                return Ok(Some(s));
            }
        }
        Ok(None)
    };
    let (spelling, declare) = match try_spellings(ctx)? {
        Some(s) => (Some(s), "not-needed"),
        None => {
            ctx.session.cmd(&format!("GLOBAL {} {}", a.amp(), b.amp()))?;
            (try_spellings(ctx)?, "needed")
        }
    };
    let Some(spelling) = spelling else {
        return Err(ProbeFail::Check("EXP 2 set/get round trip fails with both name spellings".into()));
    };
    ctx.session.set_tuning(base.with_macro_spelling(spelling));
    ctx.learned.macro_spelling = Some(spelling);
    ctx.text(key::SPELLING, spelling.tag());
    ctx.text("declare", declare);

    ctx.session.macro_set(&a, "keep")?;
    ctx.session.cmd(&format!("GLOBAL {}", a.amp()))?;
    let kept = ctx.session.macro_get(&a)? == "keep";
    ctx.text("redeclare", if kept { "keeps" } else { "resets" });

    let mut max_value = 0;
    for len in [1usize, 255, 1024, 4096, 16384, 60000] {
        let v: String = pattern(len as u64, len).iter().map(|b| char::from(b'a' + b % 26)).collect();
        if soft(ctx.session.macro_set(&b, &v))?.is_none() || soft(ctx.session.macro_get(&b))?.as_deref() != Some(&v) {
            break;
        }
        max_value = len;
    }
    ctx.headline("max_value", Value::Bytes(max_value as u64));

    let mut bad = String::new();
    for c in [' ', '"', '&', '%', ';', '\\', '\'', '$', '(', ')', ',', '\t', 'ü', '한'] {
        let v = format!("x{c}y");
        let round =
            soft(ctx.session.macro_set(&b, &v))?.is_some() && soft(ctx.session.macro_get(&b))?.as_deref() == Some(&v);
        if !round {
            bad.push(c);
        }
    }
    ctx.measure("charset_bad_full", Value::Text(bad.clone()));
    ctx.text("charset_bad", if bad.is_empty() { "none".to_string() } else { bad.escape_default().to_string() });
    macro_names(ctx)
}

/// The steps after the round trip, under the learned spelling unless named.
fn macro_names(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    use MacroSpelling::{Ampersand as Amp, Bare};
    let run = ctx.env.cfg.run_id.clone();
    let q = macro_prefix(&run);
    let n = |x: &str| MacroName::new(&format!("{q}_{x}"));
    let [x1, x2, x3, x4, x5, z, g, big] = ["x1", "x2", "x3", "x4", "x5", "z", "g", "big"].map(n);
    let [x1, x2, x3, x4, x5, z, g, big] = [x1?, x2?, x3?, x4?, x5?, z?, g?, big?];
    let never = n("never")?;
    ctx.push(Undo::ClearMacros(vec![
        x1.clone(),
        x2.clone(),
        x3.clone(),
        x4.clone(),
        x5.clone(),
        z.clone(),
        g.clone(),
        big.clone(),
    ]))?;
    let learned = ctx.session.tuning().macro_spelling();
    let get = |ctx: &mut ProbeCtx<'_>, m: &MacroName, s| exp_as(ctx, s, |x| x.macro_get(m));
    let shown = |r: &Result<String, String>| r.as_ref().map_or_else(Clone::clone, |v| format!("{v:?}"));

    let (bare_v, amp_v) = (format!("bare-{run}"), format!("amp-{run}"));
    let set1 = exp_as(ctx, Bare, |x| x.macro_set(&x1, &bare_v))?;
    let from_bare = get(ctx, &x1, Amp)?;
    let set2 = exp_as(ctx, Amp, |x| x.macro_set(&x2, &amp_v))?;
    let (from_amp, amp_amp) = (get(ctx, &x2, Bare)?, get(ctx, &x2, Amp)?);
    ctx.measure(
        "xspell_raw",
        Value::Text(format!(
            "set_bare={} get_amp={} set_amp={} get_bare={} get_amp={}",
            set1.is_ok(),
            shown(&from_bare),
            set2.is_ok(),
            shown(&from_amp),
            shown(&amp_amp)
        )),
    );
    let xspell = match (from_bare.as_deref() == Ok(&*bare_v), from_amp.as_deref() == Ok(&*amp_v)) {
        (true, true) => "same",
        (true, false) => "bare-only",
        (false, true) => "amp-only",
        (false, false) => "split",
    };
    ctx.text("xspell", xspell);

    let empty = exp_as(ctx, Bare, |x| x.macro_set(&z, ""))?;
    let payload = exp_as(ctx, Bare, |x| x.macro_get_payload(&z))?;
    ctx.measure("empty_payload", Value::Text(payload.map_or_else(|e| e, |p| hex(&p))));
    ctx.text("empty", empty.map_or_else(|e| e, |()| "ok".into()));

    let mut never_token = String::new();
    for s in [Bare, Amp] {
        let r = get(ctx, &never, s)?;
        ctx.measure(&format!("never_{}", s.tag()), Value::Text(shown(&r)));
        if s == learned {
            never_token = match r {
                Ok(v) if v.is_empty() => "empty".into(),
                Ok(_) => "value".into(),
                Err(e) => e,
            };
        }
    }
    ctx.text("never", never_token);

    soft(ctx.session.cmd(&format!("GLOBAL {}", g.amp())))?;
    for s in [Bare, Amp] {
        let p = exp_as(ctx, s, |x| x.macro_get_payload(&g))?;
        ctx.measure(&format!("declared_{}", s.tag()), Value::Text(p.map_or_else(|e| e, |p| hex(&p))));
    }

    let cmd_v = format!("cmd-{run}");
    soft(ctx.session.cmd(&format!("GLOBAL {}", x3.amp())))?;
    let assigned = soft(ctx.session.cmd(&format!("{}=\"{cmd_v}\"", x3.amp())))?.is_some();
    let (b, a) = (get(ctx, &x3, Bare)?, get(ctx, &x3, Amp)?);
    ctx.measure("cmdasgn_raw", Value::Text(format!("cmd={assigned} bare={} amp={}", shown(&b), shown(&a))));
    let cmdasgn = match (b.as_deref() == Ok(&*cmd_v), a.as_deref() == Ok(&*cmd_v)) {
        (true, true) => "both",
        (true, false) => "bare",
        (false, true) => "amp",
        (false, false) => "none",
    };
    ctx.text("cmdasgn", cmdasgn);
    soft(ctx.session.cmd(&format!("{}=\"cmd4-{run}\"", x4.amp())))?;
    let undeclared = get(ctx, &x4, learned)?;
    ctx.measure("cmdasgn_undeclared", Value::Text(shown(&undeclared)));
    soft(ctx.session.cmd(&format!("GLOBAL {}", x5.amp())))?;
    soft(ctx.session.cmd(&format!("{}=\"a;b\"", x5.amp())))?;
    let semi = match get(ctx, &x5, learned)? {
        Ok(v) if v == "a;b" => "kept",
        Ok(v) if v == "a" => "cut",
        _ => "none",
    };
    ctx.text("semi", semi);

    let mut worst = "kept";
    for len in [4096usize, 4097, 8192] {
        let v: String = pattern(len as u64, len).iter().map(|b| char::from(b'a' + b % 26)).collect();
        let set = exp_as(ctx, learned, |x| x.macro_set(&big, &v))?;
        let got = if set.is_ok() { Some(get(ctx, &big, learned)?) } else { None };
        let (glen, prefix) = match &got {
            Some(Ok(g)) => (g.len().to_string(), v.starts_with(g.as_str())),
            Some(Err(e)) => (e.clone(), false),
            None => ("-".into(), false),
        };
        ctx.measure(&format!("big.{len}"), Value::Text(format!("set={} len={glen} prefix={prefix}", set.is_ok())));
        let class = match &got {
            None => "refused",
            Some(Ok(g)) if *g == v => "kept",
            Some(_) => "truncated",
        };
        if len > 4096 && (class == "refused" || (class == "truncated" && worst == "kept")) {
            worst = class;
        }
    }
    ctx.text("big", worst);
    Ok(())
}

/// Returns what `b.cmm` returned, so the runner's result shows how far the chain got.
/// ASSUMED(bundle): `ENTRY %LINE` after `DO` yields the callee's `ENDDO` value, as in the runner.
const MAIN: &str = "GLOBAL &t32ctl_probe_main\n&t32ctl_probe_main=\"main-ran\"\nLOCAL &r\n\
                    DO \"~~~~/sub/b.cmm\"\nENTRY %LINE &r\nENDDO &r\n";
/// `OS.PresentPracticeDirectory()+"/..."` is the function form ide_ref documents (`Eval.GOSUB`);
/// `~~~~` is documented only in `DO`/`GOSUB` parameters.
const SUB_B: &str = "IF OS.FILE.SIZE(OS.PresentPracticeDirectory()+\"/deep/data.bin\")!=4097.\n  \
                     ENDDO \"size-mismatch\"\nWAIT 1.s\nDO \"~~~~/deep/c.cmm\"\nENDDO \"chain-ok\"\n";
/// Proves the end of the chain ran in this run: the client compares the global with the token.
const SUB_C: &str = "GLOBAL &t32ctl_probe_deep\n&t32ctl_probe_deep=\"&t32ctl_token\"\nWAIT 1.s\nENDDO\n";

/// The self-extracting bundle end to end: upload, cache hit, repair after a missing marker, a
/// nested `DO ~~~~/...` chain through the runner, and what `GetPracticeState` reports meanwhile.
pub fn bundle(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let data = pattern(4097, 4097);
    let bundle = bundle_of(&[
        ("main.cmm", MAIN.as_bytes()),
        ("sub/b.cmm", SUB_B.as_bytes()),
        ("sub/deep/c.cmm", SUB_C.as_bytes()),
        ("sub/deep/data.bin", &data),
        ("empty.txt", b""),
        ("dir with space/x.cmm", b"ENDDO\n"),
    ])?;
    let (main_macro, deep_macro) = (name("t32ctl_probe_main"), name("t32ctl_probe_deep"));
    ctx.push(Undo::ClearMacros(vec![main_macro.clone(), deep_macro.clone()]))?;
    let (stage, session) = ctx.stage()?;
    let snap = stage.ensure_snapshot(session, &bundle)?;
    let upload = snap.upload.unwrap_or_default();
    let mut sizes_ok = true;
    for (rel, want) in [("main.cmm", MAIN.len() as u64), ("sub/deep/data.bin", 4097), ("empty.txt", 0)] {
        sizes_ok &= transfer::file_size(session, &snap.dir.join(rel)?)? == want;
    }
    let t = Instant::now();
    let cached = stage.ensure_snapshot(session, &bundle)?.cached;
    let cached_time = t.elapsed();
    session.cmd(&t32stage::practice::cmd::rm(&snap.dir.join(".t32ctl/complete")?))?;
    let repaired = !stage.ensure_snapshot(session, &bundle)?.cached;
    let report = run_entry(stage, session, &snap, "main.cmm", &[], Duration::from_secs(30))?;
    let script_wrote = soft(session.macro_get(&main_macro))?.as_deref() == Some("main-ran");
    let deep = soft(session.macro_get(&deep_macro))?;
    let deep_ran = deep.as_deref().is_some_and(|v| v.trim_matches('"') == report.token);

    ctx.headline("files", Value::Count(bundle.len() as u64));
    ctx.headline("gz", Value::Bytes(upload.gz_bytes));
    ctx.headline("upload", Value::Seconds(upload.elapsed.as_secs_f64()));
    ctx.measure("unpack", Value::Seconds(upload.unpack.as_secs_f64()));
    ctx.text("cached", if cached { format!("{:.1}ms", cached_time.as_secs_f64() * 1e3) } else { "FAIL".into() });
    ctx.text("repair", ok(repaired));
    ctx.text("sizes", ok(sizes_ok));
    ctx.measure("outcome", Value::Text(format!("{:?}", report.outcome)));
    let nested = matches!(&report.outcome, RunOutcome::Finished { rc: 0, ret, .. } if ret.contains("chain-ok"));
    ctx.text("nested_do", ok(nested && deep_ran));
    ctx.measure("deep_macro", Value::Text(format!("{deep:?}")));
    ctx.headline("idle_dips", Value::Count(report.polls.idle_after_running.into()));
    ctx.measure("polls", Value::Count(report.polls.samples.into()));
    ctx.measure("running_samples", Value::Count(report.polls.running.into()));
    ctx.text("script_write", ok(script_wrote));
    ctx.check(sizes_ok && cached && repaired && nested && deep_ran && script_wrote, || {
        format!(
            "bundle: sizes={sizes_ok} cached={cached} repaired={repaired} deep={deep_ran} outcome={:?}",
            report.outcome
        )
    })
}

/// One argument per group of `practice::render_args` characters; `args.cmm` returns them joined.
const SAFE_ARGS: [&str; 5] = ["x,y", "a/b", "k=v", "P:0x10", "m+n@o-p.q_r"];
const ARGS: &str = "LOCAL &a &b &c &d &e\nENTRY &a &b &c &d &e\nENDDO \"&a|&b|&c|&d|&e\"\n";

/// A failing script must never look like success, and the stack is empty after every case.
/// Also passes `SAFE_ARGS` through the runner's `DO` line and compares what `ENTRY` received.
pub fn script_error(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let scratch = ctx.env.scratch_window().base();
    let data_err = format!("Data.LOAD.Binary \"~~~~/missing.bin\" VM:0x{scratch:08X}\nENDDO\n");
    let cases: [(&str, String, bool); 7] = [
        ("err_top.cmm", "T32PROBE_NO_SUCH_COMMAND\nENDDO\n".into(), true),
        ("err_nested.cmm", "DO \"~~~~/err_top.cmm\"\nENDDO\n".into(), true),
        ("err_missing.cmm", "DO \"~~~~/does_not_exist.cmm\"\nENDDO\n".into(), true),
        ("err_data.cmm", data_err, true),
        ("err_syntax.cmm", "IF\nENDDO\n".into(), true),
        ("stop.cmm", "STOP\nENDDO\n".into(), false),
        ("ok_false.cmm", "ENDDO FALSE()\n".into(), false),
    ];
    let mut files: Vec<(&str, &[u8])> = cases.iter().map(|(n, t, _)| (*n, t.as_bytes())).collect();
    files.push(("args.cmm", ARGS.as_bytes()));
    let bundle = bundle_of(&files)?;
    let (stage, session) = ctx.stage()?;
    let snap = stage.ensure_snapshot(session, &bundle)?;
    let (mut caught, mut halted, mut false_success, mut stack_clean) = (0, 0, Vec::new(), true);
    let mut results = Vec::new();
    for (entry, _, must_fail) in &cases {
        let r = run_entry(stage, session, &snap, entry, &[], Duration::from_secs(20))?;
        let depth = session.eval(fnc::STACK_DEPTH)?.into_int(fnc::STACK_DEPTH)?;
        stack_clean &= depth == 0;
        match &r.outcome {
            RunOutcome::Finished { rc, .. } if *rc != 0 => caught += 1,
            RunOutcome::Halted { .. } => halted += 1,
            _ => {}
        }
        if *must_fail && r.outcome.is_success() {
            false_success.push(*entry);
        }
        results.push(format!("{entry}={:?}", r.outcome));
    }
    let ok_false = results.last().is_some_and(|r| r.contains("rc: 0") && r.contains("FALSE"));
    let r = run_entry(stage, session, &snap, "args.cmm", &SAFE_ARGS, Duration::from_secs(20))?;
    let got = match &r.outcome {
        RunOutcome::Finished { rc: 0, ret, .. } => Some(ret.trim_matches('"').to_string()),
        _ => None,
    };
    let args_ok = got.as_deref() == Some(SAFE_ARGS.join("|").as_str());
    ctx.measure("args_ret", Value::Text(format!("{got:?} from {:?}", r.outcome)));
    ctx.measure("results", Value::Text(results.join(" | ")));
    ctx.headline("cases", Value::Count(cases.len() as u64));
    ctx.headline("caught", Value::Count(caught));
    ctx.headline("halted", Value::Count(halted));
    ctx.text("stack_clean", if stack_clean { "yes" } else { "NO" });
    ctx.text("false_ok", ok(ok_false));
    ctx.text("args", ok(args_ok));
    ctx.check(false_success.is_empty() && stack_clean && args_ok, || {
        format!(
            "reported success for {false_success:?}; stack clean after every case: {stack_clean}; \
             arguments intact: {args_ok}"
        )
    })
}
