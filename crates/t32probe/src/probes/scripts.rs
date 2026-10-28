// SPDX-License-Identifier: Apache-2.0
//! Probes about PRACTICE scripts: macro, bundle, script-error.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use t32rcl::{MacroName, MacroSpelling};
use t32stage::{Macro, RunOutcome, Undo, transfer};

use super::{
    Listing, bundle_of, exp_as, hex,
    lab::{Answer, Case, CaseRecord, Expect, Lab, LabSizes, Slot},
    list_dir, macro_prefix, ok, pattern, run_entry, soft,
};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::{SkipReason, Value, key},
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

/// The bundle of run `run`, every file below `pb-<run>/`: a snapshot path that expands to nothing
/// in the unpack script would put them below PowerView's working directory, where the canary
/// looks.
pub(crate) fn probe_bundle(run: &str) -> Result<t32stage::Bundle, ProbeFail> {
    let data = pattern(4097, 4097);
    let files: [(&str, &[u8]); 6] = [
        ("main.cmm", MAIN.as_bytes()),
        ("sub/b.cmm", SUB_B.as_bytes()),
        ("sub/deep/c.cmm", SUB_C.as_bytes()),
        ("sub/deep/data.bin", &data),
        ("empty.txt", b""),
        ("dir with space/x.cmm", b"ENDDO\n"),
    ];
    let named: Vec<(String, &[u8])> = files.iter().map(|(p, b)| (format!("pb-{run}/{p}"), *b)).collect();
    let refs: Vec<(&str, &[u8])> = named.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    bundle_of(&refs)
}

/// `OS.DIR` of `\pb-<run>` and `pb-<run>`: `Check` when either exists, `when` naming the step.
pub(crate) fn canary(ctx: &mut ProbeCtx<'_>, when: &str) -> ProbeResult {
    let top = format!("pb-{}", ctx.env.cfg.run_id);
    for p in [format!("\\{top}"), top] {
        let expr = format!("OS.DIR(\"{p}\")");
        let found = soft(ctx.session.eval(&expr).and_then(|v| v.into_bool(&expr)))?;
        ctx.measure(&format!("canary.{when}.{p}"), Value::Text(format!("{found:?}")));
        if found == Some(true) {
            ctx.text("canary", "FAIL");
            return Err(ProbeFail::Check(format!("{when}: the bundle wrote outside the snapshot directory: {p}")));
        }
    }
    Ok(())
}

/// Whether a script reads `&t32ctl_snap` as the value just set over EXP 2.
pub(crate) fn sn_case(dir: &str) -> Case {
    let body = format!("@a\n@ask snap \"&t32ctl_snap\" == \"{dir}\"\n@ask empty \"&t32ctl_snap\" == \"\"\n@z\nENDDO");
    Case::new("sn", body, Expect::HaltAfter("a"), Duration::from_secs(10))
}

fn gate_closed(ctx: &mut ProbeCtx<'_>, gate: &str, why: &str) -> ProbeResult {
    ctx.text("gate", format!("skip:{gate}"));
    Err(ProbeFail::Skip(SkipReason::NotApplicable { why: why.into() }))
}

/// The self-extracting bundle end to end: upload, cache hit, repair after a missing marker, a
/// nested `DO ~~~~/...` chain through the runner, and what `GetPracticeState` reports meanwhile.
/// Runs only where macro-visibility and unpack-literal showed that the unpack script sees its
/// snapshot path, and where a script reads `&t32ctl_snap` under its real name.
pub fn bundle(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    if ctx.learned.exp_to_script != Some(true) {
        return gate_closed(ctx, "a", "EXP 2 values do not reach scripts (macro-visibility v1)");
    }
    if ctx.learned.abspath_same != Some(true) {
        return gate_closed(ctx, "b", "OS.FILE.ABSPATH rewrites the snapshot path; the unpack guard would refuse it");
    }
    canary(ctx, "before")?;
    let run = ctx.env.cfg.run_id.clone();
    let bundle = probe_bundle(&run)?;
    let (main_macro, deep_macro) = (name("t32ctl_probe_main"), name("t32ctl_probe_deep"));
    ctx.push(Undo::ClearMacros(vec![main_macro.clone(), deep_macro.clone()]))?;
    let (stage, session) = ctx.stage()?;
    let dir = stage.path(&bundle.key().hex())?;
    session.macro_set(&Macro::Snap.name(), &dir.text())?;
    let (mut lab, _) = Lab::open(ctx, "bun", LabSizes::default())?;
    let sn = lab.run(ctx, &sn_case(&dir.text()));
    let closed = lab.close(ctx);
    let sn = sn?;
    closed?;
    ctx.measure("sn", Value::Text(format!("snap={} empty={}", sn.answer("snap").token(), sn.answer("empty").token())));
    if sn.answer("snap") != Answer::Yes {
        return gate_closed(ctx, "c", "&t32ctl_snap is not visible to a script under its real name");
    }
    ctx.text("gate", "run");

    let top = format!("pb-{run}");
    let (stage, session) = ctx.stage()?;
    let snap = stage.ensure_snapshot(session, &bundle)?;
    let upload = snap.upload.unwrap_or_default();
    let mut sizes_ok = true;
    for (rel, want) in [("main.cmm", MAIN.len() as u64), ("sub/deep/data.bin", 4097), ("empty.txt", 0)] {
        sizes_ok &= transfer::file_size(session, &snap.dir.join(&format!("{top}/{rel}"))?)? == want;
    }
    let t = Instant::now();
    let cached = stage.ensure_snapshot(session, &bundle)?.cached;
    let cached_time = t.elapsed();
    session.cmd(&t32stage::practice::cmd::rm(&snap.dir.join(".t32ctl/complete")?))?;
    let repaired = !stage.ensure_snapshot(session, &bundle)?.cached;
    let report = run_entry(stage, session, &snap, &format!("{top}/main.cmm"), &[], Duration::from_secs(30))?;
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
    canary(ctx, "after")?;
    ctx.text("canary", "clean");
    snapshot_strays(ctx, &bundle, &snap.dir)?;
    ctx.check(sizes_ok && cached && repaired && nested && deep_ran && script_wrote, || {
        format!(
            "bundle: sizes={sizes_ok} cached={cached} repaired={repaired} deep={deep_ran} outcome={:?}",
            report.outcome
        )
    })
}

/// List the snapshot directory and every directory the unpack script creates: a file the stage
/// did not register gets its removal registered and is reported as `stray`.
fn snapshot_strays(ctx: &mut ProbeCtx<'_>, bundle: &t32stage::Bundle, snap: &t32stage::StagePath) -> ProbeResult {
    let mut dirs = vec![snap.clone()];
    for d in bundle.pack(0).dirs {
        dirs.push(snap.join(&d)?);
    }
    let mut stray = Vec::new();
    for d in &dirs {
        let Listing::Names { names, .. } = list_dir(ctx.session, d)? else { continue };
        let (stage, _) = ctx.stage()?;
        let known: BTreeSet<String> = stage
            .pending()
            .iter()
            .filter_map(|u| match u {
                Undo::RemoveFile(p) | Undo::RemoveDir(p) => Some(p.text().to_ascii_lowercase()),
                _ => None,
            })
            .collect();
        for n in names {
            let Ok(p) = d.join(&n) else { continue };
            if !known.contains(&p.text().to_ascii_lowercase()) && !dirs.contains(&p) {
                stray.push(p.text());
                stage.register(Undo::RemoveFile(p))?;
            }
        }
    }
    ctx.measure("stray_names", Value::Text(stray.join(",")));
    if !stray.is_empty() {
        ctx.headline("stray", Value::Count(stray.len() as u64));
    }
    Ok(())
}

/// Scripts the script-error cases `DO`: file name below `{S}` without `.cmm`, whether running
/// it must fail, and its text.
pub(crate) const ERR_SUBS: [(&str, bool, &str); 8] = [
    ("err_top", true, "T32PROBE_NO_SUCH_COMMAND\nENDDO\n"),
    ("err_nested", true, "DO \"{S}\\err_top.cmm\"\nENDDO\n"),
    ("err_missing", true, "DO \"{S}\\does_not_exist.cmm\"\nENDDO\n"),
    ("err_data", true, "Data.LOAD.Binary \"{S}\\missing.bin\" VM:{B}\nENDDO\n"),
    ("err_syntax", true, "IF\nENDDO\n"),
    ("err_save", true, "Data.SAVE.Binary \"{S}\\no_such_dir\\x.bin\" VM:{M}++0x0\nENDDO\n"),
    ("stop", false, "STOP\nENDDO\n"),
    ("ok_false", false, "ENDDO FALSE()\n"),
];

/// `DO`es one of `ERR_SUBS` under `ON ERROR JUMPTO`. `failed` is the handler's first line after
/// the reset; the depth and message reads come after `@z`. A halt inside the `DO` cannot be
/// declared (no `DO` after a halt marker), so it ends by the case timeout.
const RUNNER: &str = "@a\nON ERROR JUMPTO t32p_failed\nDO \"{S}\\<case>.cmm\"\n@mark d\nON ERROR inherit\n\
                      @mark ok\n@z\nENDDO\nt32p_failed:\nON ERROR inherit\n@mark failed\n@z\n\
                      @ask d1 PRACTICE.StackDepth() == 1.\nPRIVATE &m\n&m=ERROR.MESSAGE()\n@put 0 \"[&m]\"\n@v\nENDDO";
const ES: &str = "@a\nON ERROR JUMPTO t32p_f\nData.SAVE.Binary \"{S}\\no_such_dir\\x.bin\" VM:{M}++0x0\n\
                  ON ERROR inherit\n@mark notcaught\n@z\nENDDO\nt32p_f:\nON ERROR inherit\n@mark caught\n@z\nENDDO";
const IG: &str = "@a\nIF 1==1\n  GOTO t32p_l1\n@mark fell\nt32p_l1:\n@mark jumped\nIF 1==0\n  @mark iffalse\n\
                  IF OS.DIR(\"{S}\\nope\")==FALSE()\n  @mark dirfalse\n\
                  IF OS.FILE.SIZE(\"{S}\\ig-a.bin\")!=0x00000002\n  @mark sizene\n@z\nENDDO";

/// The runner case of `ERR_SUBS[i]`: id `r-<name>` with `_` turned into `-`.
pub(crate) fn err_case(name: &str) -> Case {
    let id = format!("r-{}", name.replace('_', "-"));
    Case::new(id, RUNNER.replace("<case>", name), Expect::HaltAfter("d"), Duration::from_secs(20))
}

pub(crate) fn es_case() -> Case {
    Case::new("es", ES, Expect::HaltAfter("a"), Duration::from_secs(20))
}

pub(crate) fn ig_case() -> Case {
    Case::new("ig", IG, Expect::End, Duration::from_secs(10))
}

/// What the script-error cases showed, as report tokens.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ErrTokens {
    pub caught: String,
    pub false_ok: String,
    pub stop: &'static str,
    pub ok_false: &'static str,
    pub hdepth: &'static str,
    pub after: String,
    pub msg: &'static str,
    pub save_err: &'static str,
    pub ifgoto: &'static str,
}

impl ErrTokens {
    /// `runs` pairs each `ERR_SUBS` name and its must-fail flag with its runner's record.
    /// `created`: the file `err_save` and `es` must not create exists.
    pub fn of(runs: &[(&str, bool, CaseRecord)], es: &CaseRecord, ig: &CaseRecord, created: bool) -> Self {
        let must: Vec<&(&str, bool, CaseRecord)> = runs.iter().filter(|(_, m, _)| *m).collect();
        let caught = must.iter().filter(|(_, _, r)| r.has("failed")).count();
        let false_ok: Vec<&str> = must.iter().filter(|(_, _, r)| r.has("ok")).map(|(n, _, _)| *n).collect();
        let rec = |name: &str| runs.iter().find(|(n, _, _)| *n == name).map(|(_, _, r)| r);
        let stop = match rec("stop") {
            Some(r) if r.has("failed") => "caught",
            Some(r) if r.has("ok") => "ok",
            _ => "halted",
        };
        let ok_false = if rec("ok_false").is_some_and(|r| r.has("ok")) { "ok" } else { "FAIL" };
        let hdepth = match runs.iter().find(|(_, _, r)| r.has("failed")).map(|(_, _, r)| r.answer("d1")) {
            Some(Answer::Yes) => "1",
            Some(Answer::No) => "other",
            _ => "?",
        };
        let deepest = runs.iter().map(|(_, _, r)| r).chain([es, ig]).map(|r| r.depth).max().unwrap_or(0);
        let msg = runs.iter().flat_map(|(_, _, r)| &r.puts).any(|(_, s)| matches!(s, Slot::Text(_)));
        let save_err = if created {
            "created"
        } else if es.has("caught") {
            "caught"
        } else {
            "not"
        };
        let ig_ok = ig.has("jumped") && !ig.has("fell") && !ig.has("iffalse") && ig.has("dirfalse") && ig.has("sizene");
        Self {
            caught: format!("{caught}/{}", must.len()),
            false_ok: if false_ok.is_empty() { "none".into() } else { false_ok.join(",") },
            stop,
            ok_false,
            hdepth,
            after: if deepest == 0 { "clean".into() } else { format!("depth{deepest}") },
            msg: if msg { "slot" } else { "none" },
            save_err,
            ifgoto: if ig_ok { "ok" } else { "FAIL" },
        }
    }
}

/// A failing script must never look like success, and the stack is empty after every case.
/// Every case runs on literal paths under `ON ERROR JUMPTO`; the oracle files say which path it
/// took.
pub fn script_error(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "err", LabSizes::default())?;
    let result = script_error_cases(ctx, &mut lab);
    let closed = lab.close(ctx);
    result.and(closed)
}

fn script_error_cases(ctx: &mut ProbeCtx<'_>, lab: &mut Lab) -> ProbeResult {
    lab.register(ctx, "no_such_dir", true)?;
    lab.register(ctx, "no_such_dir/x.bin", false)?;
    for (name, _, body) in ERR_SUBS {
        lab.stage_file(ctx, &format!("{name}.cmm"), body)?;
    }
    let mut runs = Vec::new();
    for (name, must_fail, _) in ERR_SUBS {
        runs.push((name, must_fail, lab.run(ctx, &err_case(name))?));
    }
    let es = lab.run(ctx, &es_case())?;
    let ig = lab.run(ctx, &ig_case())?;
    let created = lab.readable(ctx, "no_such_dir/x.bin")?;
    let dir = lab.dir_exists(ctx, "no_such_dir")?;
    ctx.measure("no_such_dir", Value::Text(format!("dir={dir} file={created}")));
    let t = ErrTokens::of(&runs, &es, &ig, created);
    ctx.text("caught", t.caught.clone());
    ctx.text("false_ok", t.false_ok.clone());
    ctx.text("stop", t.stop);
    ctx.text("ok_false", t.ok_false);
    ctx.text("hdepth", t.hdepth);
    ctx.text("after", t.after.clone());
    ctx.text("msg", t.msg);
    ctx.text("save_err", t.save_err);
    ctx.text("ifgoto", t.ifgoto);
    if !lab.cut.is_empty() {
        return Err(ProbeFail::Check(format!("cut by the probe deadline: {}", lab.cut.join(","))));
    }
    ctx.check(t.false_ok == "none" && t.after == "clean", || {
        format!("reported success for {}; stack after the cases: {}", t.false_ok, t.after)
    })
}

#[cfg(test)]
#[path = "scripts_tests.rs"]
mod tests;
