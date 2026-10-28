// SPDX-License-Identifier: Apache-2.0
//! fs-semantics and vm-large against canned frames.

use t32rcl::testkit::{Request, Script};

use crate::{
    exec::tests::{connect, run, test_env},
    outcome::Verdict,
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
