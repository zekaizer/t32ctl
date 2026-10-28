// SPDX-License-Identifier: Apache-2.0
//! fs-semantics and vm-large against canned frames.

use t32rcl::testkit::{Expect, Request, Script};

use crate::{
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    probes::{
        lab::lab_tests::{ROOT, dir, yes_no},
        pattern,
    },
    registry::ProbeId,
};

/// `workdir(tag)` on an existing root.
fn work_opened(s: Script, tag: &str) -> Script {
    let d = dir(tag);
    let s = yes_no(s.expect_fn("OS.PTD()").reply_text("C:\\T"), &format!("OS.DIR(\"{ROOT}\")"), true);
    let s = yes_no(s, &format!("OS.DIR(\"{d}\")"), false).expect_cmd(&format!("MKDIR \"{d}\"")).reply_ok();
    yes_no(s, &format!("OS.DIR(\"{d}\")"), true)
}

/// A snapshot of `units` never-initialized 4 KiB units from the window base.
fn captured(mut s: Script, units: u64) -> Script {
    for u in 0..units {
        let addr = 0x0100_0000 + u * 4096;
        s = s.expect_req(Request::MemRead { access: "VM".into(), addr, len: 1008 }).reply_error(113, "");
    }
    s
}

/// The snapshot cleared back, then the work directory removed.
fn work_closed(mut s: Script, tag: &str, units: u64) -> Script {
    for u in 0..units {
        s = s.expect_cmd(&format!("Data.CLEARVM VM:0x{:08X}++0x00000FFF", 0x0100_0000 + u * 4096)).reply_ok();
    }
    s = captured(s, units);
    let d = dir(tag);
    let s = yes_no(s, &format!("OS.DIR(\"{d}\")"), true).expect_cmd(&format!("RMDIR \"{d}\"")).reply_ok();
    yes_no(s, &format!("OS.DIR(\"{d}\")"), false)
}

fn put(s: Script, file: &str, bytes: &[u8]) -> Script {
    let s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: 0x0100_0000, data: bytes.to_vec() }).reply_ok();
    s.expect_cmd(&format!("Data.SAVE.Binary \"{file}\" VM:0x01000000++0x{:08X}", bytes.len() - 1))
}

#[test]
fn fs_semantics_registers_both_bak_candidates_before_overwriting() {
    let d = dir("fs");
    let x = format!("{d}\\x.bin");
    let s = put(captured(work_opened(Script::new().attach(), "fs"), 32), &x, &pattern(1, 100)).reply_ok();
    let s = s.expect_fn(&format!("OS.FILE.SIZE(\"{x}\")")).reply_decimal(100);
    let s = s.expect_fn(&format!("OS.FIRSTFILE(\"{d}\\*\")")).reply_text(&x).expect_fn("OS.NEXTFILE()").reply_text("");
    let first = put(s, &x, &pattern(2, 100)).eof();
    let mut s = Script::new().attach().expect_fn("TRUE()").reply_bool(true);
    s = yes_no(s, &format!("OS.FILE.readable(\"{d}\\x.bin.bak\")"), false);
    for name in ["x.bak", "x.bin"] {
        s = yes_no(s, &format!("OS.FILE.readable(\"{d}\\{name}\")"), true);
        s = s.expect_cmd(&format!("RM \"{d}\\{name}\"")).reply_ok();
        s = yes_no(s, &format!("OS.FILE.readable(\"{d}\\{name}\")"), false);
    }
    let mut session = connect(vec![Ok(first), Ok(work_closed(s, "fs", 32))]);
    let mut env = test_env("fs-bak-first");
    let r = run(&mut env, &mut session, &[ProbeId::FsSemantics.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail);
    let at = |name: &str| o.cleanup.iter().position(|c| c.what.ends_with(&format!("\\{name}")));
    assert!(at("x.bin.bak") < at("x.bak") && at("x.bak") < at("x.bin"), "{:?}", o.cleanup);
    assert!(o.cleanup.iter().any(|c| c.what.ends_with("\\x.bak") && c.result == "done"), "{:?}", o.cleanup);
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

/// One vm-large rung of `mib` MiB at the window base; `silence` before the SAVE answers.
fn rung(mut s: Script, mib: u32, silence: Option<std::time::Duration>) -> Script {
    let (d, n) = (dir("large"), mib << 20);
    for k in 0..(n / 1024) {
        let addr = 0x0100_0000 + u64::from(k * 1024);
        s = s.expect(Expect::MemWriteLen { access: "VM".into(), addr, len: 1024 }).reply_ok();
    }
    let f = format!("{d}\\w{mib}m.bin");
    s = s.expect_cmd(&format!("Data.SAVE.Binary \"{f}\" VM:0x01000000++0x{:08X}", n - 1));
    if let Some(quiet) = silence {
        s = s.silence(quiet);
    }
    s = s.reply_ok().expect_cmd(&format!("Data.CLEARVM VM:0x01000000++0x{:08X}", n - 1)).reply_ok();
    s = s.expect_cmd(&format!("Data.LOAD.Binary \"{f}\" VM:0x01000000")).reply_ok();
    let data = pattern(u64::from(n), n as usize);
    let tail = &data[(n - (1 << 20)) as usize..];
    for (k, chunk) in tail.chunks(1008).enumerate() {
        let addr = 0x0100_0000 + u64::from(n - (1 << 20)) + (k * 1008) as u64;
        s = s.expect_req(Request::MemRead { access: "VM".into(), addr, len: chunk.len() as u16 }).reply_mem(chunk);
    }
    s.expect_cmd(&format!("RM \"{f}\"")).reply_ok()
}

#[test]
fn vm_large_sends_each_save_under_a_data_budget() {
    let d = dir("large");
    let s = captured(work_opened(Script::new().attach(), "large"), 2048);
    let mut s = rung(rung(s, 4, None), 8, Some(std::time::Duration::from_secs(15)));
    for mib in [8, 4] {
        for name in [format!("w{mib}m.bin.bak"), format!("w{mib}m.bak"), format!("w{mib}m.bin")] {
            s = yes_no(s, &format!("OS.FILE.readable(\"{d}\\{name}\")"), false);
        }
    }
    let mut session = connect(vec![Ok(work_closed(s, "large", 2048))]);
    let mut env = test_env("vm-large-budget");
    let r = run(&mut env, &mut session, &[ProbeId::VmLarge.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{:?}", o.error);
    for (k, v) in [("len", "8"), ("save", "0.00s:ka0/15.00s:ka0"), ("ka_first", "none"), ("verify", "ok")] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
