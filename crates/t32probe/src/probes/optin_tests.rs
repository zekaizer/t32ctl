// SPDX-License-Identifier: Apache-2.0
//! Opt-in probe decisions against canned frames.

use std::time::Duration;

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    env::ScriptOut,
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    probes::lab::lab_tests::{closed_down, files_removed, listed, opened, readable, sampled, staged, started},
    registry::ProbeId,
};

const DEPTH: &str = "PRACTICE.StackDepth()";

#[test]
fn dialog_stops_on_state_2_and_ends_only_its_own_script() {
    const TAG: &str = "dlg";
    let case = dlg();
    let mark = |s: Script, m: &str, v: bool| readable(s, TAG, &format!("dlg-{m}.bin"), v);
    let mut s = started(staged(opened(Script::new().attach(), TAG), TAG, &case, ScriptOut::Off), TAG, &case);
    s = mark(mark(sampled(s, 2, 1), "a", true), "z", false);
    s = sampled(s.expect_req(Request::Stop).reply_error(1, ""), 0, 1);
    s = s.expect_fn(DEPTH).reply_decimal(1).expect_cmd("END").reply_ok().expect_fn(DEPTH).reply_decimal(0);
    s = mark(mark(s.expect_fn(DEPTH).reply_decimal(0), "h", true), "z", false);
    s = files_removed(listed(s, TAG, &[]), TAG, &case, ScriptOut::Off, &[]);
    let mut session = connect(vec![Ok(closed_down(s, TAG))]);
    let mut env = test_env("dialog-state2");
    env.cfg.gates.dialog = true;
    let r = run(&mut env, &mut session, &[ProbeId::Dialog.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [("state2", "seen"), ("stop", "st1"), ("after", "0/1"), ("stack_clean", "yes"), ("z", "no")] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

const RC: &str = "C:\\T\\t32ctl\\probe-r1-rc";
const BASE: u64 = 0x0100_0000;
/// 4 KiB units of the run-control scratch region.
const STORE_UNITS: u64 = 16;

fn rc_opened(s: Script) -> Script {
    let s = s.expect_req(Request::TargetState).reply_state(1).expect_fn("sYmbol.EXIST(main)").reply_bool(true);
    let s = s.expect_fn("OS.PTD()").reply_text("C:\\T").expect_fn("OS.DIR(\"C:\\T\\t32ctl\")").reply_bool(true);
    let s = s.expect_fn(&format!("OS.DIR(\"{RC}\")")).reply_bool(false).expect_cmd(&format!("MKDIR \"{RC}\""));
    let mut s = s.reply_ok().expect_fn(&format!("OS.DIR(\"{RC}\")")).reply_bool(true);
    for u in 0..STORE_UNITS {
        s = s
            .expect_req(Request::MemRead { access: "VM".into(), addr: BASE + u * 4096, len: 1008 })
            .reply_error(113, "");
    }
    s
}

/// `STOre "<rc>\<name>" Break` and its download through `VM:`.
fn stored(s: Script, name: &str, text: &str) -> Script {
    let p = format!("{RC}\\{name}");
    let s = s.expect_cmd(&format!("STOre \"{p}\" Break")).reply_ok();
    let s = s.expect_fn(&format!("OS.FILE.SIZE(\"{p}\")")).reply_decimal(text.len() as i64);
    let s = s.expect_cmd(&format!("Data.LOAD.Binary \"{p}\" VM:0x01000000")).reply_ok();
    s.expect_req(Request::MemRead { access: "VM".into(), addr: BASE, len: text.len() as u16 })
        .reply_mem(text.as_bytes())
}

fn avail(s: Script, kind: &str, name: &str, v: bool) -> Script {
    s.expect_fn(&format!("PRACTICE.{kind}.AVAILable({name})")).reply_bool(v)
}

fn removed(s: Script, name: &str, present: bool) -> Script {
    let p = format!("{RC}\\{name}");
    let s = s.expect_fn(&format!("OS.FILE.readable(\"{p}\")")).reply_bool(present);
    if !present {
        return s;
    }
    s.expect_cmd(&format!("RM \"{p}\"")).reply_ok().expect_fn(&format!("OS.FILE.readable(\"{p}\")")).reply_bool(false)
}

#[test]
fn run_control_reverts_core_and_breakpoints_even_when_a_step_fails() {
    const BP: &str = "; breakpoints\nB::\nBreak.RESet\n";
    let mut s = stored(stored(rc_opened(Script::new().attach()), "bp0.cmm", BP), "bp0b.cmm", BP);
    s = avail(s, "CoMmanD", "Go", true).expect_cmd("Go").reply_ok();
    for _ in 0..100 {
        s = s.expect_req(Request::TargetState).reply_state(3);
    }
    s = s.expect_cmd("Break").reply_ok().expect_req(Request::TargetState).reply_state(1);
    s = avail(s, "FUNCtion", "CORE.NUMBER", true).expect_fn("CORE.NUMBER()").reply_decimal(2);
    s = avail(s, "CoMmanD", "CORE.select", true).expect_fn("CORE()").reply_decimal(0);
    s = s.expect_cmd("CORE.select 0").reply_ok();
    s = avail(s, "FUNCtion", "Register", true).expect_fn("Register(PC)").reply_text("0x100");
    s = s.expect_cmd("CORE.select 1").reply_error(2, "core not available");
    s = s.expect_cmd("CORE.select 0").reply_ok();
    s = s.expect_cmd("Go main").reply_ok().expect_req(Request::TargetState).reply_state(1);
    s = stored(s, "bp1.cmm", BP);
    for c in ["Step.Over", "Step", "Step.Asm", "Step.Out", "Go.Return", "Go.Up"] {
        s = avail(s, "CoMmanD", c, false);
    }
    s = avail(s, "CoMmanD", "Break.Set", true).expect_cmd("Break.Set main").reply_ok();
    let first = s.expect_cmd(&format!("STOre \"{RC}\\bp2.cmm\" Break")).silence(Duration::from_millis(1100));

    let mut s = Script::new().attach().expect_fn("TRUE()").reply_bool(true);
    s = removed(s, "bp2.cmm", false).expect_cmd("Break.Delete main").reply_ok();
    s = removed(s, "bp1.cmm", true).expect_req(Request::TargetState).reply_state(1);
    s = removed(removed(s, "bp0b.cmm", true), "bp0.cmm", true);
    for u in 0..STORE_UNITS {
        s = s.expect_cmd(&format!("Data.CLEARVM VM:0x{:08X}++0x00000FFF", BASE + u * 4096)).reply_ok();
    }
    for u in 0..STORE_UNITS {
        s = s
            .expect_req(Request::MemRead { access: "VM".into(), addr: BASE + u * 4096, len: 1008 })
            .reply_error(113, "");
    }
    s = s.expect_fn(&format!("OS.DIR(\"{RC}\")")).reply_bool(true).expect_cmd(&format!("RMDIR \"{RC}\"")).reply_ok();
    let second = s.expect_fn(&format!("OS.DIR(\"{RC}\")")).reply_bool(false);

    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("run-control-revert");
    env.cfg.gates.run_control = Some(crate::cli::RunControlInput {
        symbol: t32stage::Symbol::parse("main").unwrap(),
        loop_line: None,
        watch: None,
        scratch_ram: None,
    });
    let r = run(&mut env, &mut session, &[ProbeId::RunControl.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.error.as_ref().map(|e| e.tag.as_str()), Some("timeout"), "{o:?}");
    for (k, v) in [("store", "stable"), ("halt_all", "partial"), ("tmp_bp", "clean"), ("stepout", "none")] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert_eq!(o.lookup("revert.core"), Some(&Value::Text("done".into())));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

fn queried(ctx: &mut crate::ctx::ProbeCtx<'_>) -> crate::ctx::ProbeResult {
    let t = symbol_queries(ctx, &t32stage::Symbol::parse("main").unwrap())?;
    ctx.text("sym", t);
    Ok(())
}

#[test]
fn elf_unzip_asks_the_symbol_functions_this_build_offers() {
    let mut s = avail(Script::new().attach(), "FUNCtion", "sYmbol.BEGIN", true);
    s = s.expect_fn("sYmbol.BEGIN(main)").reply_text("P:0x1000");
    s = avail(s, "FUNCtion", "sYmbol.END", true).expect_fn("sYmbol.END(main)").reply_text("P:0x10FF");
    s = avail(s, "FUNCtion", "sYmbol.SIZEOF", false);
    s = avail(s, "FUNCtion", "sYmbol.NAME", true).expect_fn("sYmbol.NAME(P:0x1000)").reply_function(0, "no symbol");
    s = avail(s, "FUNCtion", "sYmbol.FUNCTION", true).expect_fn("sYmbol.FUNCTION(P:0x1000)").reply_text("main");
    s = avail(s, "FUNCtion", "sYmbol.SECRANGE", true);
    s = s.expect_fn("sYmbol.SECRANGE(\".text\")").reply_text("P:0x0--0xFFFF");
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("elf-symbols");
    let def: &'static crate::registry::ProbeDef = Box::leak(Box::new(crate::registry::ProbeDef {
        id: ProbeId::ElfUnzip,
        tier: crate::registry::Tier::Safe,
        requires: &[],
        needs_connection: true,
        deadline: Duration::from_secs(5),
        about: "",
        run: queried,
    }));
    let r = run(&mut env, &mut session, &[def]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("sym"), Some(&Value::Text("4/5".into())));
    assert_eq!(o.lookup("unverified.sYmbol.SIZEOF"), Some(&Value::Text("unverified:sYmbol.SIZEOF".into())));
}
