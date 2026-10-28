// SPDX-License-Identifier: Apache-2.0
//! `raw script` in-process: the bundle from a repository directory, the checks before connecting,
//! and scripted runs through `t32stage::testkit`.

use std::fs;

use serde_json::Value;
use t32rcl::testkit::{Request, Script};
use t32stage::{Bundle, BundleLimits, Macro, SnapshotKey, testkit as tk};

use crate::tests::{World, call, connected, deps, run_dirs, stopped_header};

const STRESS: &str = "ENTRY &base &len\nData.Test A:&base++(&len-1) /Prime\nENDDO\n";
const HELPER: &str = "ENDDO\n";

/// A world with `tools/t32/ddr_stress.cmm` and `tools/t32/sub/helper.cmm`.
fn scripts(tag: &str) -> World {
    let w = World::new(tag);
    w.write("ws/tools/t32/ddr_stress.cmm", STRESS);
    w.write("ws/tools/t32/sub/helper.cmm", HELPER);
    w
}

fn key() -> SnapshotKey {
    let mut b = Bundle::new(BundleLimits::default());
    b.insert(t32stage::RelPath::new("ddr_stress.cmm").unwrap(), STRESS.into()).unwrap();
    b.insert(t32stage::RelPath::new("sub/helper.cmm").unwrap(), HELPER.into()).unwrap();
    b.key()
}

/// Prelude, stage, cached snapshot, the run with `args` up to `DO`.
fn started(args: &str) -> Script {
    tk::run_started(tk::cached(tk::opened(connected(2)), &key()), &key(), "ddr_stress.cmm", args, true, &[])
}

fn ended(s: Script, files: &[(&str, &[u8])]) -> Script {
    stopped_header(tk::closed(tk::outputs_removed(tk::fetched(s, files), files), false))
}

#[test]
fn a_script_runs_from_its_directory_with_its_arguments_and_reports_a_halt() {
    let w = scripts("script-halt");
    let log: [(&str, &[u8]); 1] = [("stress.log", b"pass 1 failed\n")];
    let mut s = started("0x40000000 0x100000");
    for _ in 0..2 {
        s = s.expect_req(Request::PracticeState).reply_state(0);
        s = tk::get(s, Macro::Done, "").expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    }
    s = s.expect_cmd("EVAL ERROR.MESSAGE()").reply_ok().expect_req(Request::EvalString);
    s = s.reply_string("memory access failed at A:0x40000400");
    s = s.expect_cmd("EVAL ERROR.CMDLINE()").reply_ok().expect_req(Request::EvalString);
    s = s.reply_string("Data.Test A:0x40000000++0xfffff /Prime");
    s = s.expect_req(Request::PracticeState).reply_state(0).expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1).expect_cmd("END").reply_ok();
    s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    let out = call(
        "t32ctl raw script tools/t32/ddr_stress.cmm -- 0x40000000 0x100000",
        &deps(w.env(), vec![Ok(ended(s, &log))]),
    );
    let dirs = run_dirs(&w);
    let id = dirs[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(id.starts_with("s-"), "{id}");
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             script tools/t32/ddr_stress.cmm (bundle tools/t32, 2 files, cached)\n\
             result: halted by a PRACTICE error after 0ms\n  \
             message: memory access failed at A:0x40000400\n  \
             line:    Data.Test A:0x40000000++0xfffff /Prime\n  \
             the script was unwound; staged files were removed\n\
             outputs: ~/.local/state/t32ctl/out/{id}/ (1 file)\n  \
             stress.log  14\n"
        )
    );
    assert_eq!(out.code, 1);
    assert_eq!(fs::read(dirs[0].join("stress.log")).unwrap(), b"pass 1 failed\n");
}

#[test]
fn a_script_without_rc_finishes_with_exit_0_into_the_out_directory_it_names() {
    let w = scripts("script-out");
    let log: [(&str, &[u8]); 1] = [("dump.bin", &[1, 2, 3])];
    let s = ended(tk::read_back(tk::done(started("")), "", "", "", ""), &log);
    let out = call("t32ctl raw script tools/t32/ddr_stress.cmm --out results --json", &deps(w.env(), vec![Ok(s)]));
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(
        v["body"],
        serde_json::json!({
            "kind": "run", "what": "script", "entry": "tools/t32/ddr_stress.cmm",
            "bundle": {"root": "tools/t32", "files": 2, "cached": true},
            "result": {"outcome": "finished", "rc": null, "message": "", "after_ms": 0},
            "outputs": {"dir": "results/", "files": [{"name": "dump.bin", "bytes": 3}], "missing": []}
        })
    );
    assert_eq!(out.code, 0);
    assert_eq!(fs::read(w.0.join("ws/results/dump.bin")).unwrap(), [1, 2, 3]);
    assert!(run_dirs(&w).is_empty(), "--out replaces the run directory");
}

#[test]
fn the_entry_the_root_the_arguments_and_the_out_directory_are_checked_before_connecting() {
    let w = scripts("script-checks");
    fs::create_dir_all(w.0.join("ws/results")).unwrap();
    fs::create_dir_all(w.0.join("ws/tools/other")).unwrap();
    w.write("ws/tools/t32/.hidden.cmm", "ENDDO\n");
    let long = "a".repeat(4097);
    for (args, want) in [
        ("tools/t32/none.cmm", "error[notation]: ENTRY tools/t32/none.cmm: No such file or directory (os error 2)"),
        (
            "tools/t32/ddr_stress.cmm --root tools/other",
            "error[notation]: ENTRY tools/t32/ddr_stress.cmm is not below --root tools/other",
        ),
        (
            "tools/t32/.hidden.cmm",
            "error[notation]: ENTRY tools/t32/.hidden.cmm is a hidden file or an editor leftover, which the bundle \
             leaves out",
        ),
        (
            "tools/t32/ddr_stress.cmm -- 'a b'",
            "error[notation]: argument 0 contains ' ', which cannot be passed safely",
        ),
        (&format!("tools/t32/ddr_stress.cmm -- {long}"), "error[notation]: arguments are 4097 bytes, limit 4096"),
        (
            "tools/t32/ddr_stress.cmm --out results",
            "error[file-exists]: --out results exists; raw script --out never overwrites; choose a new directory",
        ),
        (
            "tools/t32/ddr_stress.cmm --out none/results",
            "error[notation]: --out none/results: the parent directory does not exist",
        ),
        ("tools/t32/ddr_stress.cmm --timeout 3h", "error[notation]: duration 3h is outside 1s..2h"),
    ] {
        let out = call(&format!("t32ctl raw script {args}"), &deps(w.env(), vec![]));
        assert_eq!((out.text.trim_end(), out.code), (want, 2), "{args}");
    }
    assert!(run_dirs(&w).is_empty());
}

#[test]
fn a_directory_the_bundle_cannot_carry_is_bad_bundle_naming_the_file() {
    let w = scripts("script-bundle");
    std::os::unix::fs::symlink("ddr_stress.cmm", w.0.join("ws/tools/t32/link.cmm")).unwrap();
    let out = call("t32ctl raw script tools/t32/ddr_stress.cmm", &deps(w.env(), vec![]));
    assert_eq!(
        (out.text.as_str(), out.code),
        (
            "error[bad-bundle]: tools/t32/link.cmm: a symlink; a script directory holds only regular files and \
             directories\n",
            2
        )
    );
    fs::remove_file(w.0.join("ws/tools/t32/link.cmm")).unwrap();
    w.write("ws/tools/t32/DDR_STRESS.cmm", "x");
    let out = call("t32ctl raw script tools/t32/ddr_stress.cmm", &deps(w.env(), vec![]));
    assert_eq!(
        (out.text.as_str(), out.code),
        ("error[bad-bundle]: tools/t32: DDR_STRESS.cmm and ddr_stress.cmm differ only in case\n", 2)
    );
}

#[test]
fn a_root_past_the_bundle_limits_is_bad_bundle_before_it_is_read() {
    let w = scripts("script-limits");
    let big = fs::File::create(w.0.join("ws/tools/t32/big.bin")).unwrap();
    big.set_len(BundleLimits::default().max_bytes + 1).unwrap();
    let out = call("t32ctl raw script tools/t32/ddr_stress.cmm", &deps(w.env(), vec![]));
    assert_eq!(
        (out.text.as_str(), out.code),
        ("error[bad-bundle]: tools/t32/big.bin: past the bundle limits of 512 files / 33554432 bytes\n", 2)
    );
}
