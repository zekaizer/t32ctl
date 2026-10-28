// SPDX-License-Identifier: Apache-2.0
//! script-lab decisions against canned frames.

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    env::ScriptOut,
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    probes::lab::lab_tests::{closed_down, files_removed, listed, opened, readable, sampled, staged, started, yes_no},
    registry::ProbeId,
};

const TAG: &str = "lab";
const DEPTH: &str = "PRACTICE.StackDepth()";
const R0: u64 = 0x0102_1000;

fn vm_read(addr: u64, len: u16) -> Request {
    Request::MemRead { access: "VM".into(), addr, len }
}

/// The `ERROR.*` reads of `Lab::diag` after a halt.
pub(crate) fn diagnosed(s: Script, message: &str, id: &str) -> Script {
    let s = s.expect_fn("ERROR.MESSAGE()").reply_text(message).expect_fn("ERROR.CMDLINE()").reply_text("X");
    let s = yes_no(s, "ERROR.OCCURRED()", true).expect_fn("ERROR.ID(\"PLAIN\")").reply_text(id);
    let s = s.expect_fn("ERROR.FIRSTID(\"PLAIN\")").reply_text(id).expect_fn("OS.PPF()").reply_text("C:\\x.cmm");
    let mut s = s;
    for (f, v) in [("ERROR.MESSAGE()", message), ("ERROR.CMDLINE()", "X")] {
        s = s.expect_cmd(&format!("EVAL {f}")).reply_ok().expect_req(Request::EvalString).reply_string(v);
    }
    s.expect_fn("ERROR.MESSAGE()").reply_text(message).expect_fn("ERROR.CMDLINE()").reply_text("X")
}

fn marker(s: Script, id: &str, m: &str, v: bool) -> Script {
    readable(s, TAG, &format!("{id}-{m}.bin"), v)
}

/// A case that wrote `a` and `z`, then halted: three held samples, diagnostics, one `END`.
fn halted_after_z(s: Script, case: &Case) -> Script {
    let id = case.id.as_str();
    let s = started(staged(s, TAG, case, ScriptOut::Off), TAG, case);
    let s = marker(marker(sampled(s, 0, 1), id, "a", true), id, "z", true);
    let s = diagnosed(sampled(sampled(s, 0, 1), 0, 1), "unknown command", "#err");
    let s = s.expect_fn(DEPTH).reply_decimal(1).expect_fn(DEPTH).reply_decimal(1);
    let s = s.expect_cmd("END").reply_ok().expect_fn(DEPTH).reply_decimal(0);
    marker(s, id, "v", false)
}

/// A case that ran to its end before the first sample.
fn ended(s: Script, case: &Case) -> Script {
    let id = case.id.as_str();
    let s = started(staged(s, TAG, case, ScriptOut::Off), TAG, case);
    marker(marker(sampled(s, 0, 0), id, "a", true), id, "z", true).expect_fn(DEPTH).reply_decimal(0)
}

/// The RCL calls `script-lab` makes once per round while a script runs.
fn round(s: Script, id: &str, z: bool) -> Script {
    let s = sampled(s, 1, 1).expect_fn("TRUE()").reply_bool(true);
    let s = s.expect_fn("OS.PPF()").reply_text(&format!("C:\\T\\t32ctl\\probe-r1-lab\\{id}.cmm"));
    let s = s.expect_req(Request::MacroGet { name: "t32ctl_qr1_e1".into() }).reply_macro("e1-r1");
    let s = s.expect_req(vm_read(R0, 16)).reply_mem(&[0; 16]);
    marker(s.expect_cmd("Data.Set VM:0x01021F00 \"x\" 0").reply_ok(), id, "z", z)
}

/// `w` seen while running, `rounds` rounds (the last finds `z`), then the end.
fn measured(s: Script, case: &Case, rounds: usize) -> Script {
    let id = case.id.as_str();
    let s = started(staged(s, TAG, case, ScriptOut::Off), TAG, case);
    let mut s = marker(marker(marker(sampled(s, 1, 1), id, "a", true), id, "w", true), id, "z", false);
    for k in 0..rounds {
        s = round(s, id, k + 1 == rounds);
    }
    sampled(s, 0, 0).expect_fn(DEPTH).reply_decimal(0)
}

fn cmdset(s: Script) -> Script {
    let s = s.expect_cmd("Data.Set VM:0x01021000 \"t32q\" 0").reply_ok();
    let s = s.expect_req(vm_read(R0, 5)).reply_mem(b"t32q\0");
    let s = s.expect_cmd("Data.Set VM:0x01021100 %Long %LE 0x11223344").reply_ok();
    s.expect_req(vm_read(R0 + 0x100, 4)).reply_mem(&[0x44, 0x33, 0x22, 0x11])
}

fn asks(s: Script, id: &str, answers: &[(&str, bool)]) -> Script {
    asks_in(s, TAG, id, answers)
}

fn asks_in(s: Script, tag: &str, id: &str, answers: &[(&str, bool)]) -> Script {
    answers.iter().fold(s, |s, (k, yes)| {
        let s = readable(s, tag, &format!("{id}-{k}-yes.bin"), *yes);
        readable(s, tag, &format!("{id}-{k}-no.bin"), !yes)
    })
}

#[test]
fn script_lab_passes_on_the_file_oracle_alone() {
    let mut s = cmdset(opened(Script::new().attach(), TAG));
    let o0 = o0();
    s = asks(ended(s, &o0), "o0", &[("nodir", false), ("size", true)]);
    for k in 0..4u64 {
        s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: R0 + k * 1024, data: vec![0xEE; 1024] });
        s = s.reply_ok();
    }
    let (c1, c2) = (c1(), c2());
    s = halted_after_z(s, &c1).expect_req(vm_read(R0, 0x100)).reply_mem(&[0xEE; 0x100]);
    s = readable(halted_after_z(s, &c2), TAG, "c2-w.txt", false);
    let mut cases = vec![o0, c1, c2];
    for k in 1..=20 {
        let c = w300(k);
        s = ended(s, &c);
        cases.push(c);
    }
    let c50 = format!("{}\\c50.cmm", crate::probes::lab::lab_tests::dir(TAG));
    s = readable(s, TAG, "c50.cmm", false);
    s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: 0x0100_0000, data: C50.as_bytes().to_vec() });
    s = s.reply_ok().expect_cmd(&format!("Data.SAVE.Binary \"{c50}\" VM:0x01000000++0x{:08X}", C50.len() - 1));
    s = s.reply_ok().expect_fn(&format!("OS.FILE.SIZE(\"{c50}\")")).reply_decimal(C50.len() as i64);
    let mut nests = Vec::new();
    for k in 1..=20 {
        let c = nest(k);
        s = ended(s, &c);
        nests.push(c);
    }
    s = s.expect_req(Request::MacroSet { name: "t32ctl_qr1_e1".into(), value: "e1-r1".into() }).reply_macro("");
    let (w3, lp) = (w3(), looped());
    s = asks(measured(measured(s, &w3, 2), &lp, 1), "loop", &[("n", true)]);
    s = listed(s, TAG, &[]);

    s = files_removed(files_removed(s, TAG, &lp, ScriptOut::Off, &[]), TAG, &w3, ScriptOut::Off, &[]);
    s = s.expect_req(Request::MacroSet { name: "t32ctl_qr1_e1".into(), value: String::new() }).reply_macro("");
    for c in nests.iter().rev() {
        s = files_removed(s, TAG, c, ScriptOut::Off, &[]);
    }
    s = readable(s, TAG, "c50.cmm", false);
    for c in cases.iter().rev() {
        s = files_removed(s, TAG, c, ScriptOut::Off, &[]);
    }
    let mut session = connect(vec![Ok(closed_down(s, TAG))]);
    let mut env = test_env("script-lab-pass");
    let r = run(&mut env, &mut session, &[ProbeId::ScriptLab.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    let text = |k: &str| o.lookup(k).cloned().unwrap_or_else(|| panic!("no {k} in {o:?}"));
    for (k, v) in [
        ("oracle", "fx"),
        ("cmdset", "ok"),
        ("vmset", "halt"),
        ("write", "halt"),
        ("out", "none"),
        ("blocks", "no"),
        ("wait_state", "running"),
        ("cmd_during", "ok"),
        ("ppf", "ok"),
        ("loop", "ok"),
        ("rcl_save", "-"),
    ] {
        assert_eq!(text(k), Value::Text(v.into()), "{k}");
    }
    assert_eq!(env.learned.script_out, Some(ScriptOut::Off));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

#[test]
fn script_lab_fails_when_o0_never_ends() {
    let o0 = o0();
    let s = cmdset(opened(Script::new().attach(), TAG));
    let s = started(staged(s, TAG, &o0, ScriptOut::Off), TAG, &o0);
    let s = marker(marker(sampled(s, 0, 0), "o0", "a", true), "o0", "z", false).expect_fn(DEPTH).reply_decimal(0);
    let s = asks(marker(s, "o0", "z", false), "o0", &[("nodir", false), ("size", true)]);
    let s = files_removed(listed(s, TAG, &[]), TAG, &o0, ScriptOut::Off, &[]);
    let mut session = connect(vec![Ok(closed_down(s, TAG))]);
    let mut env = test_env("script-lab-o0");
    let r = run(&mut env, &mut session, &[ProbeId::ScriptLab.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail, "{o:?}");
    assert!(o.error.as_ref().is_some_and(|e| e.message.contains("file-existence oracle failed")), "{:?}", o.error);
    assert_eq!(o.lookup("oracle"), Some(&Value::Text("FAIL".into())));
    assert_eq!(env.learned.script_out, None);
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

const HALT: &str = "halt";

fn mark(s: Script, id: &str, m: &str, v: bool) -> Script {
    readable(s, HALT, &format!("{id}-{m}.bin"), v)
}

/// Staged and started in the script-halt Lab.
fn begun(s: Script, case: &Case) -> Script {
    started(staged(s, HALT, case, ScriptOut::Off), HALT, case)
}

/// `a` and `h` found while halted, held for three samples, diagnosed.
fn halted_after_h(s: Script, case: &Case) -> Script {
    let id = case.id.as_str();
    let s = mark(mark(mark(sampled(begun(s, case), 0, 1), id, "a", true), id, "h", true), id, "z", false);
    let s = mark(sampled(mark(sampled(s, 0, 1), id, "z", false), 0, 1), id, "z", false);
    diagnosed(s, "unknown command", "#err")
}

/// `finish` on a halted stack: one `END`, then the markers still missing.
fn ended_by_finish(s: Script, id: &str, missing: &[&str]) -> Script {
    let s = s.expect_fn(DEPTH).reply_decimal(1).expect_fn(DEPTH).reply_decimal(1);
    let s = s.expect_cmd("END").reply_ok().expect_fn(DEPTH).reply_decimal(0);
    missing.iter().fold(s, |s, m| mark(s, id, m, false))
}

/// `w` found while running.
fn at_w(s: Script, case: &Case) -> Script {
    let id = case.id.as_str();
    mark(mark(mark(sampled(begun(s, case), 1, 1), id, "a", true), id, "w", true), id, "z", false)
}

#[test]
fn script_halt_records_t32_stop_status_1() {
    let (stop, err, wait, unwind, open, glob, read) =
        (hs_stop(), hs_err(), hs_wait(), hs_unwind(), hs_open(), hs_glob(), hs_read());
    let mut s = opened(Script::new().attach(), HALT);
    s = ended_by_finish(sampled(halted_after_h(s, &stop), 0, 1), "hs-stop", &["z"]);
    s = diagnosed(halted_after_h(s, &err).expect_cmd("ERROR.RESet").reply_ok(), "", "");
    s = ended_by_finish(s, "hs-err", &["z"]);
    s = sampled(at_w(s, &wait).expect_req(Request::Stop).reply_error(1, ""), 0, 1);
    s = s.expect_fn(DEPTH).reply_decimal(1).expect_cmd("END").reply_ok().expect_fn(DEPTH).reply_decimal(0);
    s = mark(s.expect_fn(DEPTH).reply_decimal(0), "hs-wait", "z", false);
    s = s.expect_fn(DEPTH).reply_decimal(0).expect_req(Request::Stop).reply_ok();
    s = at_w(s, &unwind).expect_req(Request::PracticeState).reply_state(1).expect_req(Request::Stop).reply_ok();
    s = s.expect_fn(DEPTH).reply_decimal(1).expect_fn(DEPTH).reply_decimal(1);
    s = s.expect_cmd("END").reply_ok().expect_fn(DEPTH).reply_decimal(0);
    s = s.expect_fn(DEPTH).reply_decimal(0).expect_fn(DEPTH).reply_decimal(0);
    s = mark(s, "hs-unwind", "z", false);
    s = ended_by_finish(halted_after_h(s, &open), "hs-open", &["z"]);
    let txt = format!("{}\\hs-open.txt", crate::probes::lab::lab_tests::dir(HALT));
    s = readable(s.expect_cmd(&format!("RM \"{txt}\"")).reply_ok(), HALT, "hs-open.txt", false);
    s = ended_by_finish(halted_after_h(s, &glob), "hs-glob", &["z"]);
    s = mark(mark(sampled(begun(s, &read), 0, 0), "hs-read", "a", true), "hs-read", "z", true);
    s = mark(s.expect_fn(DEPTH).reply_decimal(0), "hs-read", "v", true);
    s = asks_in(s, HALT, "hs-read", &[("kept", true), ("empty", false)]);
    s = s.expect_req(Request::MacroGet { name: "t32ctl_qr1_hg".into() }).reply_macro("hg-r1");
    s = s.expect_req(Request::MacroGet { name: "&t32ctl_qr1_hg".into() }).reply_macro("hg-r1");
    s = listed(s, HALT, &[]);
    s = files_removed(files_removed(s, HALT, &read, ScriptOut::Off, &[]), HALT, &glob, ScriptOut::Off, &[]);
    s = s.expect_req(Request::MacroSet { name: "t32ctl_qr1_hg".into(), value: String::new() }).reply_macro("");
    for c in [&open, &unwind, &wait, &err, &stop] {
        s = files_removed(s, HALT, c, ScriptOut::Off, &[]);
    }
    let mut session = connect(vec![Ok(closed_down(s, HALT))]);
    let mut env = test_env("script-halt");
    let r = run(&mut env, &mut session, &[ProbeId::ScriptHalt.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [
        ("stop", "0/1"),
        ("err", "0/1"),
        ("errmsg", "both"),
        ("errid", "#err"),
        ("reset", "clears"),
        ("t32stop", "st1"),
        ("stop_idle", "st0"),
        ("wait_cut", "yes"),
        ("end1", "ok"),
        ("unwind", "done"),
        ("stop_save", "n/a"),
        ("open_closed", "yes"),
        ("glob_end", "kept"),
    ] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
