// SPDX-License-Identifier: Apache-2.0
//! target-read and load-code decisions against canned frames.

use t32rcl::testkit::{Request, Script};

use crate::{
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    registry::ProbeId,
};

const ROOT: &str = "C:\\T\\t32ctl";
const BASE: u64 = 0x0100_0000;

fn dir(tag: &str) -> String {
    format!("{ROOT}\\probe-r1-{tag}")
}

/// `workdir(tag)` and a snapshot over `units` never-initialized 4 KiB units.
fn opened(s: Script, tag: &str, units: u64) -> Script {
    let d = dir(tag);
    let s = s.expect_fn("OS.PTD()").reply_text("C:\\T").expect_fn(&format!("OS.DIR(\"{ROOT}\")")).reply_bool(true);
    let s = s.expect_fn(&format!("OS.DIR(\"{d}\")")).reply_bool(false).expect_cmd(&format!("MKDIR \"{d}\""));
    let mut s = s.reply_ok().expect_fn(&format!("OS.DIR(\"{d}\")")).reply_bool(true);
    for u in 0..units {
        s = s
            .expect_req(Request::MemRead { access: "VM".into(), addr: BASE + u * 4096, len: 1008 })
            .reply_error(113, "");
    }
    s
}

/// The snapshot cleared back, then the work directory removed.
fn closed(mut s: Script, tag: &str, units: u64) -> Script {
    for u in 0..units {
        s = s.expect_cmd(&format!("Data.CLEARVM VM:0x{:08X}++0x00000FFF", BASE + u * 4096)).reply_ok();
    }
    for u in 0..units {
        s = s
            .expect_req(Request::MemRead { access: "VM".into(), addr: BASE + u * 4096, len: 1008 })
            .reply_error(113, "");
    }
    let d = dir(tag);
    let s = s.expect_fn(&format!("OS.DIR(\"{d}\")")).reply_bool(true).expect_cmd(&format!("RMDIR \"{d}\"")).reply_ok();
    s.expect_fn(&format!("OS.DIR(\"{d}\")")).reply_bool(false)
}

fn avail(s: Script, kind: &str, name: &str, v: bool) -> Script {
    s.expect_fn(&format!("PRACTICE.{kind}.AVAILable({name})")).reply_bool(v)
}

fn absent(s: Script, tag: &str, name: &str) -> Script {
    s.expect_fn(&format!("OS.FILE.readable(\"{}\\{name}\")", dir(tag))).reply_bool(false)
}

#[test]
fn target_read_skips_printing_when_printer_open_is_refused() {
    const UNITS: u64 = 64;
    let mut s = opened(Script::new().attach().expect_req(Request::TargetState).reply_state(3), "tr", UNITS);
    for f in ["SYStem.CPU", "CORE", "Register"] {
        s = avail(s, "FUNCtion", f, true);
    }
    for _ in 0..20 {
        s = s.expect_fn("SYStem.CPU()").reply_text("CortexA53").expect_fn("CORE()").reply_decimal(0);
        s = s.expect_fn("Register(PC)").reply_function(0, "target running");
    }
    s = avail(avail(s, "CoMmanD", "PRinTer.OPEN", true), "CoMmanD", "WinPrint", true);
    s = s.expect_fn("PRINTER.FILENAME()").reply_text("");
    s = s.expect_cmd(&format!("PRinTer.OPEN \"{}\\tr.lst\"", dir("tr"))).reply_error(2, "file already open");
    let s = closed(absent(s, "tr", "tr.lst"), "tr", UNITS);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("target-read-printer");
    env.cfg.gates.target_read = Some(crate::cli::TargetReadInput {
        symbol: t32stage::Symbol::parse("main").unwrap(),
        va: None,
        per_section: None,
        read_range: None,
    });
    let r = run(&mut env, &mut session, &[ProbeId::TargetRead.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [
        ("regs", "n/a"),
        ("spr", "n/a"),
        ("chunk", "n/a"),
        ("xlate", "n/a"),
        ("var", "n/a"),
        ("mem", "n/a"),
        ("print", "refused"),
        ("frame", "n/a"),
    ] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
