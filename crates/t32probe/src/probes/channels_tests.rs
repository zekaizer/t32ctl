// SPDX-License-Identifier: Apache-2.0
//! channel-in and channel-out: templates, and decisions against canned frames.

use std::{collections::BTreeSet, time::Duration};

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    env::ScriptOut,
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    probes::lab::{
        Fill,
        lab_tests::{
            closed_down, closed_down_large, dir, files_removed, listed, opened, opened_large, readable, sampled,
            staged, test_fill,
        },
        render,
    },
    registry::ProbeId,
};

const DEPTH: &str = "PRACTICE.StackDepth()";
const OUTS: [ScriptOut; 3] = [ScriptOut::VmSet, ScriptOut::Write, ScriptOut::Off];

/// Every case and sub-script of the S3 probes, by probe tag.
pub(crate) fn s3_cases() -> Vec<(&'static str, Vec<Case>, Vec<Sub>)> {
    let (cases, subs) = in_cases(&dir("in"));
    let (out, out_subs) = out_cases();
    let (vis, vis_subs) = crate::probes::macros::vis_cases();
    let (ret, ret_subs) = crate::probes::macros::ret_cases();
    vec![("in", cases, subs), ("out", out, out_subs), ("vis", vis, vis_subs), ("ret", ret, ret_subs)]
}

fn fill_with_large(tag: &str) -> Fill {
    Fill { l: Some(0x0103_6000), ..test_fill(tag) }
}

#[test]
fn every_s3_case_renders_and_passes_lint() {
    for (tag, cases, subs) in s3_cases() {
        let f = fill_with_large(tag);
        let mut ids = BTreeSet::new();
        for c in &cases {
            assert!(ids.insert(c.id.clone()), "{tag}: {} twice", c.id);
            for out in OUTS {
                render(c, &f, out).unwrap_or_else(|e| panic!("{tag} {out:?}: {e}"));
            }
            if let Some(a) = &c.args {
                crate::probes::lab::fill_text(a, &f, &c.id).unwrap();
            }
        }
        for (parent, name, body) in subs {
            assert!(ids.contains(parent), "{tag}: sub {name} of an unknown case {parent}");
            for out in OUTS {
                render(&Case::new(parent, body, Expect::End, Duration::ZERO), &f, out)
                    .unwrap_or_else(|e| panic!("{tag} {name} {out:?}: {e}"));
            }
        }
    }
}

#[test]
fn no_template_uses_one_line_repeat_except_rp1() {
    let mut one_line = Vec::new();
    for (tag, cases, subs) in s3_cases() {
        let f = fill_with_large(tag);
        let mut texts: Vec<(String, String)> =
            cases.iter().map(|c| (c.id.clone(), render(c, &f, ScriptOut::Off).unwrap().text)).collect();
        texts.extend(subs.iter().map(|(_, name, body)| (name.to_string(), body.to_string())));
        for (id, text) in texts {
            for line in text.lines() {
                let words: Vec<&str> = line.split_whitespace().collect();
                if words.first().is_some_and(|w| w.eq_ignore_ascii_case("repeat")) && words.len() > 2 {
                    one_line.push(id.clone());
                }
            }
        }
    }
    assert_eq!(one_line, ["rp1"]);
}

/// Commands that write to a path they name.
fn writes_a_path(line: &str) -> bool {
    let u = line.trim_start().to_ascii_uppercase();
    ["MKDIR", "RM ", "RMDIR", "DATA.SAVE", "OPEN ", "COPY ", "MOVE ", "ZIP ", "TAR "].iter().any(|c| u.starts_with(c))
}

#[test]
fn channel_in_writes_through_a_macro_only_behind_a_literal_guard() {
    let f = test_fill("in");
    let (cases, subs) = in_cases(&dir("in"));
    let mut bodies: Vec<(String, String)> = Vec::new();
    for c in &cases {
        bodies.push((c.id.clone(), render(c, &f, ScriptOut::Write).unwrap().text));
    }
    for (parent, name, body) in subs {
        let r = render(&Case::new(parent, body, Expect::End, Duration::ZERO), &f, ScriptOut::Write).unwrap();
        bodies.push((name.to_string(), r.text));
    }
    let mut guarded = 0;
    for (id, text) in &bodies {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.contains("~~~") {
                assert!(!writes_a_path(line), "{id}: writes through a tilde path: {line}");
            }
            if !(writes_a_path(line) && line.contains('&')) {
                continue;
            }
            let guard = lines[..i].iter().rev().find(|l| !l.starts_with(' ')).copied().unwrap_or("");
            assert!(line.starts_with("  "), "{id}: {line} is not inside an IF");
            let (left, right) = guard.strip_prefix("IF ").and_then(|g| g.split_once("==")).unwrap_or(("", ""));
            assert!(left.starts_with("\"&") && right.starts_with('"'), "{id}: {line} under {guard:?}");
            assert!(!right.contains('&') && right.contains(&f.s), "{id}: the guard compares with {right}");
            guarded += 1;
        }
    }
    assert_eq!(guarded, 2, "vi1 and a2 write through a macro");
}

// --- canned runs ---

const TAG: &str = "in";
const P: u64 = 0x0102_2000;
const B: u64 = 0x0102_6000;

fn mem_write(s: Script, addr: u64, data: &[u8]) -> Script {
    data.chunks(1024).enumerate().fold(s, |s, (k, c)| {
        let a = addr + (k * 1024) as u64;
        s.expect_req(Request::MemWrite { access: "VM".into(), addr: a, data: c.to_vec() }).reply_ok()
    })
}

/// `put_file` through the script region at the base of the window.
fn put(s: Script, tag: &str, name: &str, bytes: &[u8]) -> Script {
    let p = format!("{}\\{}", dir(tag), name.replace('/', "\\"));
    let s = mem_write(s, 0x0100_0000, bytes);
    let s = s.expect_cmd(&format!("Data.SAVE.Binary \"{p}\" VM:0x01000000++0x{:08X}", bytes.len() - 1)).reply_ok();
    s.expect_fn(&format!("OS.FILE.SIZE(\"{p}\")")).reply_decimal(bytes.len() as i64)
}

pub(crate) fn file_staged(s: Script, tag: &str, name: &str, text: &str) -> Script {
    put(readable(s, tag, &name.replace('/', "\\"), false), tag, name, text.as_bytes())
}

pub(crate) fn sub_staged(s: Script, tag: &str, parent: &str, name: &str, body: &str) -> Script {
    let r = render(&Case::new(parent, body, Expect::End, Duration::ZERO), &test_fill(tag), ScriptOut::Off).unwrap();
    let mut s = s;
    for n in r.outputs(parent, ScriptOut::Off) {
        s = readable(s, tag, &n, false);
    }
    put(readable(s, tag, &name.replace('/', "\\"), false), tag, name, r.text.as_bytes())
}

pub(crate) fn sub_removed(s: Script, tag: &str, parent: &str, name: &str, body: &str) -> Script {
    let r = render(&Case::new(parent, body, Expect::End, Duration::ZERO), &test_fill(tag), ScriptOut::Off).unwrap();
    let mut v = r.outputs(parent, ScriptOut::Off);
    v.push(name.replace('/', "\\"));
    v.iter().rev().fold(s, |s, n| readable(s, tag, n, false))
}

/// The `DO` line of a case, arguments filled.
pub(crate) fn do_line(tag: &str, case: &Case) -> String {
    let f = test_fill(tag);
    let mut line = format!("DO \"{}\\{}\"", dir(tag), case.script_name().replace('/', "\\"));
    if let Some(a) = &case.args {
        line = format!("{line} {}", crate::probes::lab::fill_text(a, &f, &case.id).unwrap());
    }
    line
}

fn halt_marker(case: &Case) -> Option<&'static str> {
    match case.expect {
        Expect::HaltAfter(m) if m != "a" && m != "z" => Some(m),
        _ => None,
    }
}

/// Staged, started and ended at the first sample: `a`, the halt marker and `z` exist.
pub(crate) fn ended_now(s: Script, tag: &str, case: &Case) -> Script {
    let id = case.id.as_str();
    let mut s = staged(s, tag, case, ScriptOut::Off);
    s = sampled(sampled(s, 0, 0).expect_cmd(&do_line(tag, case)).reply_ok(), 0, 0);
    for m in ["a"].into_iter().chain(halt_marker(case)).chain(["z"]) {
        s = readable(s, tag, &format!("{id}-{m}.bin"), true);
    }
    s
}

/// `finish` after `ended_now`: every other marker exists; `yes` asks answer yes, the others no.
/// `sub` is the body of a sub-script the case `DO`s.
pub(crate) fn finished(s: Script, tag: &str, case: &Case, yes: &[&str], sub: Option<&str>) -> Script {
    let f = test_fill(tag);
    let r = render(case, &f, ScriptOut::Off).unwrap();
    let (mut markers, mut asks) = (r.markers.clone(), r.asks.clone());
    if let Some(body) = sub {
        let sr = render(&Case::new(case.id.clone(), body, Expect::End, Duration::ZERO), &f, ScriptOut::Off).unwrap();
        markers.extend(sr.markers.into_iter().filter(|m| !r.markers.contains(m)));
        asks.extend(sr.asks);
    }
    let id = case.id.as_str();
    let seen: Vec<&str> = ["a"].into_iter().chain(halt_marker(case)).chain(["z"]).collect();
    let mut s = s.expect_fn(DEPTH).reply_decimal(0);
    for m in markers.iter().filter(|m| !seen.contains(&m.as_str())) {
        s = readable(s, tag, &format!("{id}-{m}.bin"), true);
    }
    for k in asks {
        let y = yes.contains(&k.as_str());
        s = readable(readable(s, tag, &format!("{id}-{k}-yes.bin"), y), tag, &format!("{id}-{k}-no.bin"), !y);
    }
    s
}

/// A case that ends before the first sample with every marker written.
pub(crate) fn ran(s: Script, tag: &str, case: &Case, yes: &[&str], sub: Option<&str>) -> Script {
    finished(ended_now(s, tag, case), tag, case, yes, sub)
}

pub(crate) fn yes_no(s: Script, expr: &str, v: bool) -> Script {
    s.expect_fn(expr).reply_bool(v)
}

pub(crate) fn mkdir(s: Script, tag: &str, rel: &str) -> Script {
    let p = format!("{}\\{}", dir(tag), rel.replace('/', "\\"));
    yes_no(s.expect_cmd(&format!("MKDIR \"{p}\"")).reply_ok(), &format!("OS.DIR(\"{p}\")"), true)
}

pub(crate) fn dir_gone(s: Script, tag: &str, rel: &str) -> Script {
    yes_no(s, &format!("OS.DIR(\"{}\\{}\")", dir(tag), rel.replace('/', "\\")), false)
}

fn letters(n: u32) -> Vec<u8> {
    let mut v: Vec<u8> = (0..n).map(|i| b'a' + (i % 26) as u8).collect();
    v.push(0);
    v
}

#[test]
fn channel_in_records_every_case_on_a_build_that_answers_everything() {
    let d = dir(TAG);
    let vi_dir = format!("{d}\\vi dir");
    let mut s = opened(Script::new().attach(), TAG);
    for rel in ["vi dir", "a b", "t4", "t4/sub"] {
        s = mkdir(s, TAG, rel);
    }
    s = mem_write(s, P, format!("{vi_dir}\0").as_bytes());
    s = mem_write(s, P + 0x100, &[0]);
    s = mem_write(s, P + 0x200, b"C:\\x y\\&z \"q\";\0");
    s = mem_write(s, P + 0x300, &[0xC3, 0xBC, 0xED, 0x95, 0x9C, 0]);
    s = mem_write(s, P + 0x400, b"0x40000000\0");
    s = s.expect_fn("Data.STRing(VM:0x01022000)").reply_text(&vi_dir);
    s = s.expect_fn("Data.String(VM:0x01022000)").reply_text(&vi_dir);

    s = ran(s, TAG, &vi1(), &["eq"], None);
    s = yes_no(s, &format!("OS.DIR(\"{vi_dir}\\sub\")"), true);
    s = ran(s, TAG, &vi2(), &["empty"], None);
    s = ran(s, TAG, &vi3(), &[], None);
    let out = b"C:\\x y\\&z \"q\";\r\n\xC3\xBC\xED\x95\x9C\r\n";
    let vi3_out = format!("{d}\\vi3-out.txt");
    s = readable(s, TAG, "vi3-out.txt", true).expect_fn(&format!("OS.FILE.SIZE(\"{vi3_out}\")"));
    s = s.reply_decimal(out.len() as i64).expect_cmd(&format!("Data.LOAD.Binary \"{vi3_out}\" VM:0x01026000"));
    s = s.reply_ok().expect_req(Request::MemRead { access: "VM".into(), addr: B, len: out.len() as u16 });
    s = s.reply_mem(out);
    for n in VL {
        s = mem_write(s, P + 0x1000, &letters(n));
        s = ran(s, TAG, &vl(n), if n <= 4095 { &["fn", "m"] } else { &["fn"] }, None);
    }
    s = sub_staged(s, TAG, "pr", "proc.cmm", PROC);
    s = ran(s, TAG, &pr(), &["seen"], Some(PROC));
    s = ran(s, TAG, &a1(), &["n", "p0", "p1", "p3", "p4"], None);
    s = ran(s, TAG, &a2(), &["p0", "p1"], None);
    s = yes_no(s, &format!("OS.DIR(\"{d}\\a b\\dp\")"), true);
    s = ran(ran(s, TAG, &a3n(), &["a", "b", "c"], None), TAG, &a3q(), &[], None);
    s = ran(s, TAG, &a4(), &["l"], None);
    s = ran(s, TAG, &a5(), &["a", "b", "c", "d", "e", "fa"], None);
    s = s.expect_cmd("GLOBAL &t32ctl_qr1_ca").reply_ok().expect_cmd("&t32ctl_qr1_ca=\"ca-r1\"").reply_ok();
    s = ran(s, TAG, &a6(), &[], None);
    let a7s: Vec<Case> = A7.iter().filter_map(|n| a7(&d, *n)).collect();
    assert_eq!(a7s.len(), 4);
    for c in &a7s {
        s = ran(s, TAG, c, if c.id == "a7-2037" { &[] } else { &["len"] }, None);
    }
    s = s.expect_cmd("EVAL \"eval-in-r1\"").reply_ok();
    s = ran(s, TAG, &ei(), &["e"], None);
    s = file_staged(s, TAG, "in.txt", IN_TXT);
    s = ran(s, TAG, &fi(), &["l1"], None);
    s = file_staged(s, TAG, "t4/t1.bin", "1");
    s = mem_write(s, B, &[0]);
    s = sub_staged(s, TAG, "t4", "t4/sub/b.cmm", T4_SUB);
    s = ran(s, TAG, &t4(), &["ppd", "abs", "ppdb"], Some(T4_SUB));
    s = s.expect_req(Request::MemRead { access: "VM".into(), addr: B, len: 1 }).reply_mem(&[0x31]);
    s = s.expect_fn("OS.DIR(OS.PWD()+\"\\~~~~\")").reply_bool(false);
    s = s.expect_fn("OS.FILE.ABSPATH(\"~~~/x\")").reply_text("C:\\T\\x");
    s = s.expect_fn("OS.PTD()+\"\\x\"").reply_text("C:\\t\\X");
    s = listed(s, TAG, &[]);

    s = files_removed(s, TAG, &t4(), ScriptOut::Off, &[]);
    s = sub_removed(s, TAG, "t4", "t4/sub/b.cmm", T4_SUB);
    s = readable(s, TAG, "t4\\t1.bin", false);
    s = files_removed(s, TAG, &fi(), ScriptOut::Off, &[]);
    s = readable(s, TAG, "in.txt", false);
    s = files_removed(s, TAG, &ei(), ScriptOut::Off, &[]);
    for c in a7s.iter().rev() {
        s = files_removed(s, TAG, c, ScriptOut::Off, &[]);
    }
    for c in [a6(), a5(), a4(), a3q(), a3n()] {
        s = files_removed(s, TAG, &c, ScriptOut::Off, &[]);
    }
    s = dir_gone(files_removed(s, TAG, &a2(), ScriptOut::Off, &[]), TAG, "a b/dp");
    s = files_removed(files_removed(s, TAG, &a1(), ScriptOut::Off, &[]), TAG, &pr(), ScriptOut::Off, &[]);
    s = sub_removed(s, TAG, "pr", "proc.cmm", PROC);
    for n in VL.iter().rev() {
        s = files_removed(s, TAG, &vl(*n), ScriptOut::Off, &[]);
    }
    for c in [vi3(), vi2()] {
        s = files_removed(s, TAG, &c, ScriptOut::Off, &[]);
    }
    s = dir_gone(files_removed(s, TAG, &vi1(), ScriptOut::Off, &[]), TAG, "vi dir/sub");
    for m in ["p_addr", "ca"] {
        s = s.expect_req(Request::MacroSet { name: format!("t32ctl_qr1_{m}"), value: String::new() }).reply_macro("");
    }
    for rel in ["t4/sub", "t4", "a b", "vi dir"] {
        s = dir_gone(s, TAG, rel);
    }
    let mut session = connect(vec![Ok(closed_down(s, TAG))]);
    let mut env = test_env("channel-in");
    let r = run(&mut env, &mut session, &[ProbeId::ChannelIn.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [
        ("vmstr", "ok"),
        ("vmpath", "ok"),
        ("guard", "ok"),
        ("vmbytes", "exact"),
        ("vmlen", "8192/4095"),
        ("p1param", "ok"),
        ("arg", "raw"),
        ("params", "ok"),
        ("entry", "raw"),
        ("comma", "kept"),
        ("argsafe", "ok"),
        ("semi", "cut"),
        ("reexp", "no"),
        ("dolen", "2000"),
        ("evalin", "ok"),
        ("read", "ok"),
        ("ppd", "ok"),
        ("tilde4", "ok"),
        ("tilde3", "ok"),
    ] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

const OUT: &str = "out";
const AREA: &str = "T32PQR1";

fn area_line(s: Script, n: &str, v: &str) -> Script {
    s.expect_fn(&format!("AREA.LINE(\"{AREA}\",{n}.)")).reply_text(v)
}

/// `area` with its lines read back: the text, the directory, a 1024-column cut.
fn area_read(mut s: Script) -> Script {
    s = s.expect_cmd(&format!("AREA.Create {AREA} 1024. 50. /NoSELect")).reply_ok();
    s = ran(s, OUT, &ar(), &[], None);
    let wide: String = "0123456789abcdef".repeat(64);
    for (n, v) in [("1", "area-v-r1"), ("2", "C:\\T"), ("3", wide.as_str()), ("4", ""), ("0", wide.as_str())] {
        s = area_line(s, n, v);
    }
    area_line(s, "-1", "C:\\T").expect_cmd(&format!("AREA.Delete {AREA}")).reply_ok()
}

fn area_only(ctx: &mut crate::ctx::ProbeCtx<'_>) -> crate::ctx::ProbeResult {
    let (mut lab, _) = Lab::open(ctx, OUT, LabSizes::default())?;
    area(ctx, &mut lab)?;
    let pending = ctx.ledger.pending().iter().any(|u| matches!(u, Undo::Revert { .. }));
    ctx.check(!pending, || "a Revert is still pending after the area step".into())?;
    lab.close(ctx)
}

fn leak(run: fn(&mut crate::ctx::ProbeCtx<'_>) -> crate::ctx::ProbeResult) -> &'static crate::registry::ProbeDef {
    Box::leak(Box::new(crate::registry::ProbeDef {
        id: ProbeId::ChannelOut,
        tier: crate::registry::Tier::Safe,
        requires: &[],
        needs_connection: true,
        deadline: Duration::from_secs(60),
        about: "",
        run,
    }))
}

#[test]
fn channel_out_deletes_its_area_through_revert_only_after_create_succeeded() {
    let s = opened(Script::new().attach(), OUT);
    let s = s.expect_cmd(&format!("AREA.Create {AREA} 1024. 50. /NoSELect")).reply_error(90, "no areas");
    let mut session = connect(vec![Ok(closed_down(listed(s, OUT, &[]), OUT))]);
    let mut env = test_env("channel-out-area-refused");
    let r = run(&mut env, &mut session, &[leak(area_only)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("area"), Some(&Value::Text("refused".into())));
    assert!(o.cleanup.iter().all(|c| !c.what.starts_with("revert")), "{:?}", o.cleanup);

    let s = listed(area_read(opened(Script::new().attach(), OUT)), OUT, &[]);
    let s = files_removed(s, OUT, &ar(), ScriptOut::Off, &[]);
    let mut session = connect(vec![Ok(closed_down(s, OUT))]);
    let mut env = test_env("channel-out-area");
    let r = run(&mut env, &mut session, &[leak(area_only)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("area"), Some(&Value::Text("ok".into())));
    assert_eq!(o.lookup("areacol"), Some(&Value::Text("1024".into())));
    assert_eq!(o.lookup("area_delete"), Some(&Value::Text("done".into())));
    assert!(o.cleanup.iter().all(|c| !c.what.starts_with("revert")), "{:?}", o.cleanup);
}

/// `fetch`: present, its size, loaded into the blob region, read back.
fn fetched(s: Script, tag: &str, name: &str, bytes: &[u8]) -> Script {
    let p = format!("{}\\{name}", dir(tag));
    let s = readable(s, tag, name, true).expect_fn(&format!("OS.FILE.SIZE(\"{p}\")")).reply_decimal(bytes.len() as i64);
    let s = s.expect_cmd(&format!("Data.LOAD.Binary \"{p}\" VM:0x01026000")).reply_ok();
    s.expect_req(Request::MemRead { access: "VM".into(), addr: B, len: bytes.len() as u16 }).reply_mem(bytes)
}

fn enddo_reads(mut s: Script, size: i64, first: &str, second: &str) -> Script {
    for k in 0..2 {
        s = s.expect_fn("PRACTICE.ARG.SIZE()").reply_decimal(size).expect_fn("PRACTICE.ARG(0.)").reply_text(first);
        s = s.expect_fn("PRACTICE.ARG(1.)").reply_text(second);
        if k == 0 {
            s = s.expect_cmd("PRINT \"\"").reply_ok();
        }
    }
    s
}

#[test]
fn channel_out_records_every_case_on_a_build_that_answers_everything() {
    let large = 64 * 1024;
    let mut s = opened_large(Script::new().attach(), OUT, large);
    s = fetched(ran(s, OUT, &fo(), &[], None), OUT, "fo.txt", b"file-v2\r\n");
    s = fetched(listed(s, OUT, &["fo.txt", "fo.bak", "fo.cmm"]), OUT, "fb.bin", &[0x41, 0]);
    s = ended_now(s, OUT, &eo()).expect_req(Request::EvalString).reply_string("stale");
    s = finished(s.expect_fn("EVAL.STRing()").reply_text("eval-out-r1"), OUT, &eo(), &[], None);
    for (c, n) in [(eo4000(), 4000usize), (eo4095(), 4095)] {
        s = ended_now(s, OUT, &c).expect_fn("STRing.LENgth(EVAL.STRing())").reply_decimal(n as i64);
        s = finished(s.expect_req(Request::EvalString).reply_string(&"x".repeat(n)), OUT, &c, &["len"], None);
    }
    s = ran(ran(s, OUT, &rp1(), &["once"], None), OUT, &rpb(), &["each"], None);
    s = ran(s, OUT, &m8k(), &["l4k", "trunc"], None);
    s = area_read(s);
    s = finished(enddo_reads(ended_now(s, OUT, &rt()), 2, "ret-v", "r2"), OUT, &rt(), &[], None);
    s = finished(enddo_reads(ended_now(s, OUT, &rn()), 0, "", ""), OUT, &rn(), &[], None);
    for (name, text) in [("callee.cmm", CALLEE), ("ret.cmm", RET), ("ret2.cmm", RET2)] {
        s = file_staged(s, OUT, name, text);
    }
    s = ran(s, OUT, &fw(), &["arg", "r"], None);
    for (k, yes) in [(1, &[][..]), (2, &["r"][..]), (3, &["r1"][..]), (4, &["z0"][..])] {
        s = ran(s, OUT, &el(k), yes, None);
    }
    s = ran(ran(s, OUT, &cs_a(), &["dq"], None), OUT, &cs_u(), &["u2"], None);
    s = mkdir(s, OUT, "p_-.+@=,() d");
    s = readable(ran(s, OUT, &pu(), &[], None), OUT, "p_-.+@=,() d/s_-.+@=,() t.bin", true);
    s = listed(s, OUT, &[]);

    s = dir_gone(files_removed(s, OUT, &pu(), ScriptOut::Off, &[]), OUT, "p_-.+@=,() d");
    for c in [cs_u(), cs_a(), el(4), el(3), el(2), el(1), fw()] {
        s = files_removed(s, OUT, &c, ScriptOut::Off, &[]);
    }
    for name in ["ret2.cmm", "ret.cmm", "callee.cmm"] {
        s = readable(s, OUT, name, false);
    }
    for c in [rn(), rt(), ar(), m8k(), rpb(), rp1(), eo4095(), eo4000(), eo(), fo()] {
        s = files_removed(s, OUT, &c, ScriptOut::Off, &[]);
    }
    let mut session = connect(vec![Ok(closed_down_large(s, OUT, large))]);
    let mut env = test_env("channel-out");
    let r = run(&mut env, &mut session, &[ProbeId::ChannelOut.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [
        ("file", "crlf"),
        ("create_bak", "fo.bak"),
        ("writeb", "ok"),
        ("evalout", "ok"),
        ("eval4k", "4000,4000/4095,4095"),
        ("rp1", "once"),
        ("rpb", "ok"),
        ("m8k", "trunc4096"),
        ("area", "ok"),
        ("areacol", "1024"),
        ("enddo_top", "ret"),
        ("fw", "ok"),
        ("entryline", "raw"),
        ("entry_plain", "first-word"),
        ("retfalse", "0"),
        ("dq", "ok"),
        ("utf8", "chars2"),
        ("vmsetlen", "n/a"),
        ("punct", "ok"),
    ] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
