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

/// The snapshot's first read of each 4 KiB unit of a `len`-byte region, all never initialized.
fn unit_reads(mut s: Script, len: u64) -> Script {
    for u in 0..len.div_ceil(4096) {
        let n = (len - u * 4096).min(1008) as u16;
        s = s.expect_req(Request::MemRead { access: "VM".into(), addr: BASE + u * 4096, len: n }).reply_error(113, "");
    }
    s
}

/// `workdir(tag)` and a snapshot over a never-initialized `len`-byte region.
fn opened(s: Script, tag: &str, len: u64) -> Script {
    let d = dir(tag);
    let s = s.expect_fn("OS.PTD()").reply_text("C:\\T").expect_fn(&format!("OS.DIR(\"{ROOT}\")")).reply_bool(true);
    let s = s.expect_fn(&format!("OS.DIR(\"{d}\")")).reply_bool(false).expect_cmd(&format!("MKDIR \"{d}\""));
    unit_reads(s.reply_ok().expect_fn(&format!("OS.DIR(\"{d}\")")).reply_bool(true), len)
}

/// The snapshot cleared back, then the work directory removed.
fn closed(mut s: Script, tag: &str, len: u64) -> Script {
    for u in 0..len.div_ceil(4096) {
        let end = (len - u * 4096).min(4096) - 1;
        s = s.expect_cmd(&format!("Data.CLEARVM VM:0x{:08X}++0x{end:08X}", BASE + u * 4096)).reply_ok();
    }
    let s = unit_reads(s, len);
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
    const LEN: u64 = 256 * 1024;
    let mut s = opened(Script::new().attach().expect_req(Request::TargetState).reply_state(3), "tr", LEN);
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
    let s = closed(absent(s, "tr", "tr.lst"), "tr", LEN);
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

#[test]
fn load_code_restores_pc_through_revert() {
    let elf = b"\x7fELF t32probe code".to_vec();
    let x = format!("{}\\x.elf", dir("load"));
    let mut s = opened(Script::new().attach().expect_req(Request::TargetState).reply_state(1), "load", 18);
    s = avail(avail(s, "CoMmanD", "Data.LOAD.Elf", true), "CoMmanD", "Register.Set", true);
    s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: BASE, data: elf.clone() }).reply_ok();
    s = s.expect_cmd(&format!("Data.SAVE.Binary \"{x}\" VM:0x01000000++0x{:08X}", elf.len() - 1)).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.SIZE(\"{x}\")")).reply_decimal(elf.len() as i64);
    s = avail(s, "FUNCtion", "Register", true).expect_fn("Register(PC)").reply_text("0x80000000");
    s = s.expect_cmd(&format!("Data.LOAD.Elf \"{x}\" /NoReg")).reply_ok();
    s = s.expect_fn("Register(PC)").reply_text("0x80000000");
    s = s.expect_cmd(&format!("Data.LOAD.Elf \"{x}\"")).reply_ok().expect_fn("Register(PC)").reply_text("0x80001000");
    s = avail(s, "FUNCtion", "FOUND", true).expect_cmd(&format!("Data.LOAD.Elf \"{x}\" /DIFF /NoReg /NosYmbol"));
    s = s.reply_ok().expect_fn("FOUND()").reply_bool(false);
    s = s.expect_cmd("Register.Set PC 0x80000000").reply_ok().expect_fn("Register(PC)").reply_text("0x80000000");
    s = s.expect_fn(&format!("OS.FILE.readable(\"{x}\")")).reply_bool(true).expect_cmd(&format!("RM \"{x}\""));
    s = s.reply_ok().expect_fn(&format!("OS.FILE.readable(\"{x}\")")).reply_bool(false);
    let mut session = connect(vec![Ok(closed(s, "load", 18))]);
    let mut env = test_env("load-code-pc");
    env.cfg.gates.load_code = Some(crate::cli::ElfInput { path: "x.elf".into(), bytes: elf, symbol: None });
    let r = run(&mut env, &mut session, &[ProbeId::LoadCode.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [("load", "0.00:ka0,0.00:ka0"), ("pc", "set"), ("code", "match")] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert_eq!(o.lookup("revert.pc"), Some(&Value::Text("done".into())));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
