// SPDX-License-Identifier: Apache-2.0
//! Lab rendering and judgement, pure; the Lab against canned frames.

use std::time::{Duration, Instant};

use t32rcl::{
    PracticeState,
    testkit::{Request, Script},
};

use super::*;
use crate::{
    ctx::ProbeResult,
    exec::tests::{connect, run, test_env},
    outcome::{SkipReason, Value, Verdict},
    registry::{ProbeDef, ProbeId, Tier},
};

pub(crate) const ROOT: &str = "C:\\T\\t32ctl";
const BASE: u32 = 0x0100_0000;
/// 4 KiB units of [script, M, R, P, B] in the scratch window.
const UNITS: u32 = (SCRIPT_LEN + MARKER_LEN + RESULT_LEN + INPUT_LEN + BLOB_LEN) / 4096;

pub(crate) fn dir(tag: &str) -> String {
    format!("{ROOT}\\probe-r1-{tag}")
}

/// The placeholders of a Lab opened by `opened` in the test environment.
pub(crate) fn test_fill(tag: &str) -> Fill {
    Fill {
        s: dir(tag),
        m: BASE + SCRIPT_LEN,
        r: BASE + SCRIPT_LEN + MARKER_LEN,
        p: BASE + SCRIPT_LEN + MARKER_LEN + RESULT_LEN,
        b: BASE + SCRIPT_LEN + MARKER_LEN + RESULT_LEN + INPUT_LEN,
        l: None,
        run: "r1".into(),
    }
}

pub(crate) fn yes_no(s: Script, expr: &str, v: bool) -> Script {
    s.expect_fn(expr).reply_bool(v)
}

pub(crate) fn readable(s: Script, tag: &str, name: &str, v: bool) -> Script {
    yes_no(s, &format!("OS.FILE.readable(\"{}\\{}\")", dir(tag), name.replace('/', "\\")), v)
}

/// `Lab::open` without `{L}`: work directory, a snapshot of never-initialized units, the marker.
pub(crate) fn opened(s: Script, tag: &str) -> Script {
    opened_large(s, tag, 0)
}

/// `Lab::open` with `large` bytes of `{L}` after the blob region.
pub(crate) fn opened_large(s: Script, tag: &str, large: u32) -> Script {
    let d = dir(tag);
    let mut s = yes_no(s.expect_fn("OS.PTD()").reply_text("C:\\T"), &format!("OS.DIR(\"{ROOT}\")"), true);
    s = yes_no(s, &format!("OS.DIR(\"{d}\")"), false).expect_cmd(&format!("MKDIR \"{d}\"")).reply_ok();
    s = yes_no(s, &format!("OS.DIR(\"{d}\")"), true);
    for u in 0..UNITS + large / 4096 {
        let addr = u64::from(BASE + u * 4096);
        s = s.expect_req(Request::MemRead { access: "VM".into(), addr, len: 1008 }).reply_error(113, "");
    }
    let m = u64::from(BASE + SCRIPT_LEN);
    s.expect_req(Request::MemWrite { access: "VM".into(), addr: m, data: vec![MARKER] }).reply_ok()
}

/// Cleanup of what `opened` made: the snapshot cleared back unit by unit, then the directory.
pub(crate) fn closed_down(s: Script, tag: &str) -> Script {
    closed_down_large(s, tag, 0)
}

pub(crate) fn closed_down_large(mut s: Script, tag: &str, large: u32) -> Script {
    let units = UNITS + large / 4096;
    for u in 0..units {
        s = s.expect_cmd(&format!("Data.CLEARVM VM:0x{:08X}++0x00000FFF", BASE + u * 4096)).reply_ok();
    }
    for u in 0..units {
        let addr = u64::from(BASE + u * 4096);
        s = s.expect_req(Request::MemRead { access: "VM".into(), addr, len: 1008 }).reply_error(113, "");
    }
    let d = dir(tag);
    s = yes_no(s, &format!("OS.DIR(\"{d}\")"), true).expect_cmd(&format!("RMDIR \"{d}\"")).reply_ok();
    yes_no(s, &format!("OS.DIR(\"{d}\")"), false)
}

/// Files below the Lab directory in the order `Lab::stage` claims them.
pub(crate) fn names(case: &Case, out: ScriptOut, tag: &str) -> Vec<String> {
    let r = render(case, &test_fill(tag), out).unwrap();
    let mut v = r.outputs(&case.id, out);
    v.extend(case.outputs.iter().cloned());
    v.push(case.script_name());
    v
}

/// `Lab::stage`: every name absent, the script written, saved and its size confirmed.
pub(crate) fn staged(mut s: Script, tag: &str, case: &Case, out: ScriptOut) -> Script {
    for n in names(case, out, tag) {
        s = readable(s, tag, &n, false);
    }
    let text = render(case, &test_fill(tag), out).unwrap().text.into_bytes();
    for (k, chunk) in text.chunks(1024).enumerate() {
        let addr = u64::from(BASE) + (k * 1024) as u64;
        s = s.expect_req(Request::MemWrite { access: "VM".into(), addr, data: chunk.to_vec() }).reply_ok();
    }
    let script = format!("{}\\{}", dir(tag), case.script_name().replace('/', "\\"));
    s = s.expect_cmd(&format!("Data.SAVE.Binary \"{script}\" VM:0x{BASE:08X}++0x{:08X}", text.len() - 1)).reply_ok();
    s.expect_fn(&format!("OS.FILE.SIZE(\"{script}\")")).reply_decimal(text.len() as i64)
}

pub(crate) fn sampled(s: Script, state: u8, depth: i64) -> Script {
    s.expect_req(Request::PracticeState).reply_state(state).expect_fn("PRACTICE.StackDepth()").reply_decimal(depth)
}

/// `Lab::start` on an empty stack.
pub(crate) fn started(s: Script, tag: &str, case: &Case) -> Script {
    sampled(s, 0, 0).expect_cmd(&format!("DO \"{}\\{}\"", dir(tag), case.script_name())).reply_ok()
}

/// A plain `@a ... @z ENDDO` case that ran to its end: one sample finds both markers.
pub(crate) fn ran_through(s: Script, tag: &str, case: &Case, out: ScriptOut) -> Script {
    let s = started(staged(s, tag, case, out), tag, case);
    let s = readable(
        readable(sampled(s, 0, 0), tag, &format!("{}-a.bin", case.id), true),
        tag,
        &format!("{}-z.bin", case.id),
        true,
    );
    s.expect_fn("PRACTICE.StackDepth()").reply_decimal(0)
}

/// Cleanup of a case's files, newest first; `present` names exist and are removed.
pub(crate) fn files_removed(mut s: Script, tag: &str, case: &Case, out: ScriptOut, present: &[&str]) -> Script {
    for n in names(case, out, tag).iter().rev() {
        let here = present.contains(&n.as_str());
        s = readable(s, tag, n, here);
        if here {
            s = s.expect_cmd(&format!("RM \"{}\\{n}\"", dir(tag))).reply_ok();
            s = readable(s, tag, n, false);
        }
    }
    s
}

/// `Lab::close` listing `{S}`.
pub(crate) fn listed(s: Script, tag: &str, files: &[&str]) -> Script {
    let mut s = s.expect_fn(&format!("OS.FIRSTFILE(\"{}\\*\")", dir(tag)));
    for f in files {
        s = s.reply_text(&format!("{}\\{f}", dir(tag))).expect_fn("OS.NEXTFILE()");
    }
    s.reply_text("")
}

fn leak(run: fn(&mut ProbeCtx<'_>) -> ProbeResult) -> &'static ProbeDef {
    Box::leak(Box::new(ProbeDef {
        id: ProbeId::VmFile,
        tier: Tier::Safe,
        requires: &[],
        needs_connection: true,
        deadline: Duration::from_secs(60),
        about: "",
        run,
    }))
}

fn simple() -> Case {
    Case::new("t1", "@a\n@z\nENDDO", Expect::End, Duration::from_secs(5))
}

// --- rendering ---

#[test]
fn render_expands_every_directive_exactly() {
    let f = test_fill("lab");
    let body = "@a\n@ask k OS.FILE.SIZE(\"{S}\\x\") == 0x00000001\n  @mark custom\n@z\n@put 2 \"v-{run}\"\nENDDO";
    let case = Case::new("c-1", body, Expect::End, Duration::from_secs(1));
    let d = dir("lab");
    let save = |n: &str| format!("Data.SAVE.Binary \"{d}\\c-1-{n}\" VM:0x01020000++0x0");
    let common = [
        save("a.bin"),
        format!("IF OS.FILE.SIZE(\"{d}\\x\")==0x00000001"),
        format!("  {}", save("k-yes.bin")),
        format!("IF OS.FILE.SIZE(\"{d}\\x\")!=0x00000001"),
        format!("  {}", save("k-no.bin")),
        format!("  {}", save("custom.bin")),
        save("z.bin"),
    ]
    .join("\n");
    let vm = render(&case, &f, ScriptOut::VmSet).unwrap();
    assert_eq!(vm.text, format!("{common}\nData.Set VM:0x01021200 \"v-r1\" 0\nENDDO\n"));
    assert_eq!(
        (vm.markers.clone(), vm.asks.clone(), vm.puts.clone()),
        (vec!["a".into(), "custom".into(), "z".into()], vec!["k".into()], vec![2])
    );
    let wr = render(&case, &f, ScriptOut::Write).unwrap();
    let write = format!("OPEN #8 \"{d}\\c-1-p2.txt\" /Create\nWRITE #8 \"v-r1\"\nCLOSE #8");
    assert_eq!(wr.text, format!("{common}\n{write}\nENDDO\n"));
    assert_eq!(wr.outputs("c-1", ScriptOut::Write).last().map(String::as_str), Some("c-1-p2.txt"));
    let off = render(&case, &f, ScriptOut::Off).unwrap();
    assert_eq!(off.text, format!("{common}\nENDDO\n"));
    assert_eq!(
        off.outputs("c-1", ScriptOut::Off),
        ["c-1-a.bin", "c-1-custom.bin", "c-1-z.bin", "c-1-k-yes.bin", "c-1-k-no.bin"]
    );
    let all = "{S} {M} {R0} {R15} {P0} {P7} {PL} {B} {q} {run} {A} {id}";
    let filled = fill_text(all, &Fill { l: Some(0x0300_0000), ..f.clone() }, "x");
    assert_eq!(
        filled.unwrap(),
        format!(
            "{d} 0x01020000 0x01021000 0x01021F00 0x01022000 0x01022700 0x01023000 0x01026000 t32ctl_qr1 r1 T32PQR1 x"
        )
    );
    assert_eq!(fill_text("{L}", &Fill { l: Some(0x0300_0000), ..f.clone() }, "x").unwrap(), "0x03000000");
}

#[test]
fn render_refuses_lint_violations() {
    let f = test_fill("lab");
    let refused = |body: &str, expect: Expect| {
        render(&Case::new("x", body, expect, Duration::from_secs(1)), &f, ScriptOut::VmSet).unwrap_err()
    };
    assert!(refused("@a\nOPEN #8 \"{S}\\f\" /Create\n@z", Expect::End).contains("#8"));
    assert!(refused("@a\n@put 0 \"v\"\n@z", Expect::End).contains("before @z"));
    assert!(refused("@a\n@z\n  @put 0 \"v\"", Expect::End).contains("column 0"));
    assert!(refused("@a\n  @ask k 1 == 1\n@z", Expect::End).contains("column 0"));
    assert!(refused("@a\nPRINT \"{x}\"\n@z", Expect::End).contains("unknown placeholder {x}"));
    assert!(refused("@a\nPRINT \"{L}\"\n@z", Expect::End).contains("unknown placeholder {L}"));
    assert!(refused("@a\nPRINT \"{S\"\n@z", Expect::End).contains("not closed"));
    assert!(refused("@a\n@h\nSTOP\nWAIT 1.s\n@z", Expect::HaltAfter("h")).contains("after the halt marker"));
    assert!(refused("@a\n@h\nSTOP\nDO \"{S}\\x.cmm\"\n@z", Expect::HaltAfter("h")).contains("after the halt marker"));
    assert!(refused("@a\n@ask K 1 == 1\n@z", Expect::End).contains("[a-z0-9-]+"));
    assert!(refused("@a\n@ask k 1 = 1\n@z", Expect::End).contains("` == `"));
    assert!(refused("@nope", Expect::End).contains("unknown directive"));
    let bad_id = Case::new("X", "@a", Expect::End, Duration::from_secs(1));
    assert!(render(&bad_id, &f, ScriptOut::Off).is_err());
    let ok = Case::new("x", "@a\nWAIT 1.s\n@h\nSTOP\n@z", Expect::HaltAfter("h"), Duration::from_secs(1));
    assert!(render(&ok, &f, ScriptOut::Off).is_ok(), "WAIT before the halt marker is fine");
    let c2 = Case::new("x", "@a\n@z\nOPEN #8 \"{S}\\w.txt\" /Create", Expect::End, Duration::from_secs(1));
    assert!(render(&c2.oracle_buffer(), &f, ScriptOut::Off).is_ok());
}

// --- judgement ---

const MS: Duration = Duration::from_millis(1);

fn watch(expect: Expect, timeout: Duration) -> (LabWatch, Instant) {
    let t0 = Instant::now();
    (LabWatch::new(LabTiming::DEFAULT, expect, t0, timeout), t0)
}

#[test]
fn labwatch_never_halts_without_a_marker() {
    for expect in [Expect::End, Expect::HaltAfter("h")] {
        let (mut w, t0) = watch(expect, Duration::from_secs(10));
        w.marker("a");
        for i in 0..1000u32 {
            assert_eq!(w.sample(t0 + MS * (5 * i), Until::Done, PracticeState::Idle, 1), None, "sample {i}");
        }
        assert_eq!(w.dips, 0, "an open dip is not counted yet");
        assert_eq!(
            w.sample(t0 + Duration::from_secs(10), Until::Done, PracticeState::Idle, 1),
            Some(Seen::End(Ending::TimedOut))
        );
    }
}

#[test]
fn labwatch_halts_only_after_the_declared_marker_and_hold() {
    let (mut w, t0) = watch(Expect::HaltAfter("h"), Duration::from_secs(10));
    w.marker("a");
    for i in 0..5u32 {
        assert_eq!(w.sample(t0 + MS * (100 * i), Until::Done, PracticeState::Idle, 1), None);
    }
    w.marker("h");
    let at = |ms: u32| t0 + MS * ms;
    assert_eq!(w.sample(at(500), Until::Done, PracticeState::Idle, 1), None, "hold starts at the marker");
    assert_eq!(w.sample(at(700), Until::Done, PracticeState::Idle, 1), None);
    assert_eq!(w.sample(at(790), Until::Done, PracticeState::Idle, 1), None, "three samples but 290 ms");
    let halted = Seen::End(Ending::Halted { after: "h".into() });
    assert_eq!(w.sample(at(800), Until::Done, PracticeState::Idle, 1), Some(halted));

    let (mut w, t0) = watch(Expect::HaltAfter("h"), Duration::from_secs(10));
    w.marker("h");
    assert_eq!(w.sample(t0, Until::Done, PracticeState::Idle, 1), None);
    assert_eq!(w.sample(t0 + MS * 400, Until::Done, PracticeState::Idle, 1), None, "two samples are not enough");
    assert_eq!(w.sample(t0 + MS * 450, Until::Done, PracticeState::Running, 1), None, "running resets the hold");
    for ms in [500, 900] {
        assert_eq!(w.sample(t0 + MS * ms, Until::Done, PracticeState::Idle, 1), None);
    }

    let (mut w, t0) = watch(Expect::End, Duration::from_secs(10));
    w.marker("a");
    w.stopped();
    for ms in [0, 150] {
        assert_eq!(w.sample(t0 + MS * ms, Until::Done, PracticeState::Idle, 1), None);
    }
    let stopped = Seen::End(Ending::Halted { after: "stop".into() });
    assert_eq!(w.sample(t0 + MS * 300, Until::Done, PracticeState::Idle, 1), Some(stopped), "our own stop allows it");
}

#[test]
fn labwatch_ends_on_z_and_an_empty_stack() {
    let (mut w, t0) = watch(Expect::End, Duration::from_secs(10));
    w.marker("a");
    w.marker("z");
    assert_eq!(w.sample(t0, Until::Done, PracticeState::Idle, 1), None, "z but still on the stack");
    assert_eq!(w.sample(t0 + MS, Until::Done, PracticeState::Idle, 0), Some(Seen::End(Ending::Ended)));

    let (mut w, t0) = watch(Expect::End, Duration::from_secs(10));
    w.marker("a");
    assert_eq!(w.sample(t0, Until::Done, PracticeState::Idle, 0), None);
    assert_eq!(w.sample(t0 + MS * 1999, Until::Done, PracticeState::Idle, 0), None);
    assert_eq!(w.sample(t0 + MS * 2000, Until::Done, PracticeState::Idle, 0), Some(Seen::End(Ending::Vanished)));

    let (mut w, t0) = watch(Expect::End, Duration::from_secs(10));
    assert_eq!(w.sample(t0, Until::Done, PracticeState::Idle, 0), None);
    assert_eq!(w.sample(t0 + MS * 2000, Until::Done, PracticeState::Idle, 0), Some(Seen::End(Ending::NotStarted)));
    assert_eq!(w.pre0, 2);

    let (mut w, t0) = watch(Expect::End, Duration::from_secs(10));
    assert_eq!(w.sample(t0, Until::Marker("w"), PracticeState::Running, 1), None);
    w.marker("w");
    assert_eq!(w.sample(t0 + MS, Until::Marker("w"), PracticeState::Running, 1), Some(Seen::Reached));
}

#[test]
fn labwatch_counts_dips_without_acting() {
    let (mut w, t0) = watch(Expect::End, Duration::from_secs(10));
    let seq: [(u32, PracticeState, i64); 8] = [
        (0, PracticeState::Running, 1),
        (5, PracticeState::Idle, 1),
        (10, PracticeState::Running, 2),
        (15, PracticeState::Idle, 1),
        (20, PracticeState::Idle, 1),
        (35, PracticeState::Running, 2),
        (40, PracticeState::Running, 1),
        (45, PracticeState::Idle, 1),
    ];
    w.marker("a");
    for (ms, state, depth) in seq {
        assert_eq!(w.sample(t0 + MS * ms, Until::Done, state, depth), None, "{ms} ms");
    }
    w.marker("z");
    assert_eq!(w.sample(t0 + MS * 50, Until::Done, PracticeState::Idle, 0), Some(Seen::End(Ending::Ended)));
    assert_eq!((w.dips, w.dip_max), (3, MS * 20));
    assert_eq!(w.first1, Some(Duration::ZERO));
    assert_eq!(w.seq.len(), 8, "{:?}", w.seq);
}

// --- the Lab on canned frames ---

fn one_case(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "lab", LabSizes::default())?;
    let rec = lab.run(ctx, &simple())?;
    let unwind = ctx.ledger.pending().iter().any(|u| matches!(u, Undo::UnwindPractice));
    ctx.check(rec.ended() && !unwind, || format!("{rec:?}, unwind pending: {unwind}"))?;
    lab.close(ctx)
}

#[test]
fn lab_refuses_to_start_on_a_foreign_stack() {
    let case = simple();
    let s = staged(opened(Script::new().attach(), "lab"), "lab", &case, ScriptOut::Off);
    let s = files_removed(sampled(s, 0, 1), "lab", &case, ScriptOut::Off, &["t1.cmm"]);
    let mut session = connect(vec![Ok(closed_down(s, "lab"))]);
    let mut env = test_env("lab-foreign");
    let r = run(&mut env, &mut session, &[leak(one_case)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Skipped, "{o:?}");
    assert_eq!(o.skip, Some(SkipReason::PowerViewBusy { depth: 1 }));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

#[test]
fn lab_run_registers_outputs_before_do_and_withdraws_unwind() {
    let case = simple();
    let s = staged(opened(Script::new().attach(), "lab"), "lab", &case, ScriptOut::Off);
    let first = sampled(s, 0, 0).expect_cmd(&format!("DO \"{}\\t1.cmm\"", dir("lab"))).eof();
    let second = sampled(Script::new().attach().expect_fn("TRUE()").reply_bool(true), 0, 0);
    let second = files_removed(second, "lab", &case, ScriptOut::Off, &["t1.cmm", "t1-a.bin", "t1-z.bin"]);
    let mut session = connect(vec![Ok(first), Ok(closed_down(second, "lab"))]);
    let mut env = test_env("lab-registered");
    let r = run(&mut env, &mut session, &[leak(one_case)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail);
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
    assert!(o.cleanup.iter().any(|c| c.what == "unwind PRACTICE stack"), "{:?}", o.cleanup);

    let s = ran_through(opened(Script::new().attach(), "lab"), "lab", &case, ScriptOut::Off);
    let s = files_removed(listed(s, "lab", &["t1.cmm", "t1-a.bin", "T1-Z.BIN"]), "lab", &case, ScriptOut::Off, &[]);
    let mut session = connect(vec![Ok(closed_down(s, "lab"))]);
    let mut env = test_env("lab-withdrawn");
    let r = run(&mut env, &mut session, &[leak(one_case)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert!(o.cleanup.iter().all(|c| c.what != "unwind PRACTICE stack"), "{:?}", o.cleanup);
    let rec = o.lookup("case.t1").cloned();
    assert!(matches!(&rec, Some(Value::Text(t)) if t.contains("end=ended@0") && t.contains("m=a,z")), "{rec:?}");
    assert_eq!(o.lookup("stray_names"), Some(&Value::Text(String::new())));
    assert_eq!(o.lookup("stray"), None);
}

fn just_close(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (lab, _) = Lab::open(ctx, "lab", LabSizes::default())?;
    lab.close(ctx)
}

#[test]
fn lab_close_registers_and_reports_strays() {
    let s = listed(opened(Script::new().attach(), "lab"), "lab", &["t1.bak"]);
    let s = readable(s, "lab", "t1.bak", true).expect_cmd(&format!("RM \"{}\\t1.bak\"", dir("lab"))).reply_ok();
    let s = readable(s, "lab", "t1.bak", false);
    let mut session = connect(vec![Ok(closed_down(s, "lab"))]);
    let mut env = test_env("lab-stray");
    let r = run(&mut env, &mut session, &[leak(just_close)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("stray"), Some(&Value::Count(1)));
    assert!(o.cleanup.iter().any(|c| c.what.ends_with("t1.bak") && c.result == "done"), "{:?}", o.cleanup);
}

fn makes_workdir(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let root = stage_root(ctx)?;
    workdir(ctx, &root, "w").map(|_| ())
}

#[test]
fn workdir_registers_the_removal_before_mkdir() {
    let d = dir("w");
    let s =
        yes_no(Script::new().attach().expect_fn("OS.PTD()").reply_text("C:\\T"), &format!("OS.DIR(\"{ROOT}\")"), true);
    let first = yes_no(s, &format!("OS.DIR(\"{d}\")"), false).expect_cmd(&format!("MKDIR \"{d}\"")).eof();
    let second = yes_no(Script::new().attach().expect_fn("TRUE()").reply_bool(true), &format!("OS.DIR(\"{d}\")"), true);
    let second = yes_no(second.expect_cmd(&format!("RMDIR \"{d}\"")).reply_ok(), &format!("OS.DIR(\"{d}\")"), false);
    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("lab-workdir");
    let r = run(&mut env, &mut session, &[leak(makes_workdir)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail);
    assert!(o.cleanup.iter().any(|c| c.what.ends_with("probe-r1-w") && c.result == "done"), "{:?}", o.cleanup);

    let s =
        yes_no(Script::new().attach().expect_fn("OS.PTD()").reply_text("C:\\T"), &format!("OS.DIR(\"{ROOT}\")"), true);
    let mut session = connect(vec![Ok(yes_no(s, &format!("OS.DIR(\"{d}\")"), true))]);
    let mut env = test_env("lab-workdir-exists");
    let r = run(&mut env, &mut session, &[leak(makes_workdir)]);
    let o = &r.outcomes[0];
    assert!(o.error.as_ref().is_some_and(|e| e.message.contains("work directory already exists")), "{o:?}");
    assert!(o.cleanup.is_empty(), "an existing directory is never registered: {:?}", o.cleanup);
}

fn opens_a_file() -> Case {
    let body = "@a\nOPEN #1 \"{S}\\t2.txt\" /Create\n@h\nSTOP\nCLOSE #1\n@z\nENDDO";
    Case::new("t2", body, Expect::HaltAfter("h"), Duration::from_secs(5)).outputs(&["t2.txt"])
}

fn halts_with_a_file_open(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "lab", LabSizes::default())?;
    lab.run(ctx, &opens_a_file())?;
    lab.close(ctx)
}

#[test]
fn lab_closes_the_buffers_of_a_script_it_ended() {
    let case = opens_a_file();
    let mark = |s: Script, m: &str, v: bool| readable(s, "lab", &format!("t2-{m}.bin"), v);
    let s = started(staged(opened(Script::new().attach(), "lab"), "lab", &case, ScriptOut::Off), "lab", &case);
    let s = mark(mark(mark(sampled(s, 0, 1), "a", true), "h", true), "z", false);
    let s = mark(sampled(mark(sampled(s, 0, 1), "z", false), 0, 1), "z", false);
    let s = crate::probes::scriptlab::scriptlab_tests::diagnosed(s, "stopped", "");
    let s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1).expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    let s = s.expect_cmd("END").reply_ok().expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    let s = mark(s.expect_cmd("CLOSE #1").reply_error(2, "not open"), "z", false);
    let s = files_removed(listed(s, "lab", &[]), "lab", &case, ScriptOut::Off, &[]);
    let mut session = connect(vec![Ok(closed_down(s, "lab"))]);
    let mut env = test_env("lab-close-buffers");
    let r = run(&mut env, &mut session, &[leak(halts_with_a_file_open)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("close.t2"), Some(&Value::Text("1:refused".into())));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
