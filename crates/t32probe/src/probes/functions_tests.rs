// SPDX-License-Identifier: Apache-2.0
//! fn-table decisions against canned frames.

use std::time::Duration;

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    registry::ProbeId,
};

const AVAILABLE: [&str; 4] = ["Data.Set", "PRACTICE.GLOBAL.VALUE", "OS.FIRSTFILE", "OS.NEXTFILE"];

fn refused(s: Script, expr: &str) -> Script {
    s.expect_fn(expr).reply_function(0, "unknown function")
}

/// `ERROR.ID`, `ERROR.FIRSTID` and `ERROR.RESet` after a refusal.
fn ids(s: Script) -> Script {
    let s = s.expect_fn("ERROR.ID(\"PLAIN\")").reply_text("#syntax");
    s.expect_fn("ERROR.FIRSTID(\"PLAIN\")").reply_text("#syntax").expect_cmd("ERROR.RESet").reply_ok()
}

fn eval_reads(s: Script, slot: &str) -> Script {
    let s = s.expect_req(Request::EvalString).reply_string(slot);
    refused(refused(s, "EVAL.STRing()"), "EVAL.TYPE()")
}

/// Everything before the option that may open a dialog, on a build that refuses most calls.
fn until_bogus() -> Script {
    let mut s = Script::new().attach();
    for (kind, names) in [("CoMmanD", COMMANDS), ("FUNCtion", FUNCTIONS)] {
        for n in names {
            let e = format!("PRACTICE.{kind}.AVAILable({n})");
            s = if AVAILABLE.contains(n) { s.expect_fn(&e).reply_bool(true) } else { refused(s, &e) };
        }
    }
    let set = Request::MacroSet { name: "t32ctl_qr1_fx".into(), value: "va".into() };
    s = s.expect_req(set).reply_macro("");
    s = s.expect_fn("STRing.LENgth(\"&t32ctl_qr1_fx\")").reply_decimal("&t32ctl_qr1_fx".len() as i64);
    s = s.expect_cmd("GLOBAL &t32ctl_qr1_fy").reply_ok().expect_cmd("&t32ctl_qr1_fy=\"va\"").reply_ok();
    s = s.expect_fn("STRing.LENgth(\"&t32ctl_qr1_fy\")").reply_decimal(2);
    for total in CMD_LENGTHS {
        s = s.expect_cmd(&format!("PRINT \"{}\"", "x".repeat(total - 8)));
        s = if total < 2037 { s.reply_ok() } else { s.reply_error(90, "too long") };
    }
    s = eval_reads(s.expect_cmd("EVAL \"t32q-\"+\"ok\"").reply_ok(), "t32q-ok");
    s = eval_reads(s.expect_fn("OS.PTD()").reply_text("C:\\T"), "t32q-ok");
    s = eval_reads(s.expect_fn("VERSION.BUILD()").reply_decimal(168_999), "168999.");
    s = eval_reads(s.expect_cmd("PRINT \"x\"").reply_ok(), "168999.");

    s = s.expect_req(Request::MemRead { access: "VM".into(), addr: 0x0100_0000, len: 1008 }).reply_error(113, "");
    s = s.expect_fn("OS.PTD()").reply_text("C:\\T");
    let load = "Data.LOAD.Binary \"C:\\T\\t32ctl\\probe-r1-none\\none.bin\" VM:0x01000000";
    for c in [BAD_COMMANDS[0], load] {
        for _ in 0..2 {
            s = ids(s.expect_cmd(c).reply_error(90, "refused"));
        }
    }
    for (label, f) in BAD_FUNCTIONS {
        for _ in 0..2 {
            s = if label == "symexist" { s.expect_fn(f).reply_bool(false) } else { ids(refused(s, f)) };
        }
    }
    for expr in EPOCH {
        for _ in 0..EPOCH_N {
            s = match expr {
                None => s.expect_req(Request::TargetState).reply_state(0),
                Some("SYStem.CPU()") => s.expect_fn("SYStem.CPU()").reply_text("CortexA53"),
                Some(f) => refused(s, f),
            };
        }
    }
    let answers = ["C:\\work", "C:\\x.cmm", "C:\\work\\x.cmm", "C:\\T\\x", "C:\\T", "", "C:\\print.lst"];
    for (expr, a) in PATHS.into_iter().zip(answers) {
        s = s.expect_fn(expr).reply_text(a);
    }
    s
}

const BOGUS: &str = "Data.Set VM:0x01000000 %Long 0x1 /T32PROBEBOGUS";

/// The scratch unit cleared back and the two macros emptied.
fn cleaned(s: Script) -> Script {
    let s = s.expect_cmd("Data.CLEARVM VM:0x01000000++0x00000FFF").reply_ok();
    let s = s.expect_req(Request::MemRead { access: "VM".into(), addr: 0x0100_0000, len: 1008 }).reply_error(113, "");
    let clear = |s: Script, n: &str| s.expect_req(Request::MacroSet { name: n.into(), value: String::new() });
    clear(clear(s, "t32ctl_qr1_fx").reply_macro(""), "t32ctl_qr1_fy").reply_macro("")
}

#[test]
fn fn_table_records_error_ids_and_never_fails_on_refusals() {
    let s = until_bogus().expect_cmd(BOGUS).silence(Duration::from_secs(2)).reply_error(90, "unknown option");
    let s = ids(ids(s).expect_cmd(BOGUS).reply_error(90, "unknown option"));
    let mut session = connect(vec![Ok(cleaned(s))]);
    let mut env = test_env("fn-table-refusals");
    let r = run(&mut env, &mut session, &[ProbeId::FnTable.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    let asked = COMMANDS.len() + FUNCTIONS.len();
    for (k, v) in [
        ("avail", format!("4/{asked}")),
        ("fn_amp", "mixed".into()),
        ("cmdlen", "2000".into()),
        ("eval", "cleared-by:fnnum".into()),
        ("errid", "yes".into()),
        ("cpu", "CortexA53".into()),
        ("pc_down", "err0".into()),
        ("bslash", "root".into()),
        ("pgv", "yes".into()),
        ("list", "yes".into()),
    ] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v)), "{k}");
    }
    let nope = o.lookup("err.nope.1").cloned();
    assert!(matches!(&nope, Some(Value::Text(t)) if t.contains("st=90") && t.contains("id=#syntax")), "{nope:?}");
    assert_eq!(o.lookup("err.symexist.1"), Some(&Value::Text("ok FALSE".into())));
    assert!(o.lookup("bogus").is_none(), "a refusal inside 3 s is no dialog");
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

#[test]
fn fn_table_gives_the_bogus_data_set_three_seconds() {
    let first = until_bogus().expect_cmd(BOGUS).silence(Duration::from_secs(4));
    let second = cleaned(Script::new().attach().expect_fn("TRUE()").reply_bool(true));
    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("fn-table-dialog");
    let r = run(&mut env, &mut session, &[ProbeId::FnTable.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail, "{o:?}");
    assert_eq!(o.error.as_ref().map(|e| e.tag.as_str()), Some("timeout"));
    assert_eq!(o.lookup("bogus"), Some(&Value::Text("dialog?".into())));
    assert_eq!(o.lookup("bslash"), Some(&Value::Text("root".into())), "the reads before it are kept");
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
