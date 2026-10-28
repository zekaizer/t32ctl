// SPDX-License-Identifier: Apache-2.0
//! Board procedures in-process: the grade check, parameters, and scripted runs through
//! `t32stage::testkit`, one per row of the result table.

use std::{fs, time::Instant};

use serde_json::Value;
use t32rcl::{
    Clock,
    testkit::{ManualClock, Request, Script, ScriptedConnector},
};
use t32stage::{Bundle, BundleLimits, Macro, SnapshotKey, VmLease, VmWindow, testkit as tk};

use super::*;
use crate::tests::{World, call, connected, deps, profile_hash, run_dirs, stopped_header, with_procs};

/// The snapshot key of the profile directory's bundle.
fn key(w: &World) -> SnapshotKey {
    let mut b = Bundle::new(BundleLimits::default());
    for f in profile::walk(&w.0.join("ws/board/support/t32ctl")).unwrap() {
        b.insert(f.rel, f.bytes).unwrap();
    }
    b.key()
}

/// Attach and the run state, without the CPU check (`debugger-config`).
fn attached(state: u8) -> Script {
    Script::new().attach().expect_req(Request::TargetState).reply_state(state)
}

/// The stage opened, the snapshot cached, the run started with `params`.
fn started(s: Script, w: &World, entry: &str, params: &[(&str, &str)]) -> Script {
    let s = tk::cached(tk::opened(s), &key(w));
    tk::run_started(s, &key(w), entry, "", true, params)
}

/// The poll that finds the runner done, then the read-back.
fn finished(s: Script, fail: &str, rc: &str, msg: &str) -> Script {
    tk::read_back(tk::done(s), fail, rc, msg, "")
}

/// After the run: `files` fetched, cleanup, the header.
fn ended(s: Script, files: &[(&str, &[u8])]) -> Script {
    let s = tk::fetched(s, files);
    stopped_header(tk::closed(tk::outputs_removed(s, files), false))
}

/// The unwind after a halt, a timeout or a dialog: `state` first, one script on the stack, `END`.
fn unwound(s: Script, state: u8) -> Script {
    let mut s = s.expect_req(Request::PracticeState).reply_state(state);
    if state != 0 {
        s = s.expect_req(Request::Stop).reply_ok();
    }
    s.expect_fn("PRACTICE.StackDepth()")
        .reply_decimal(1)
        .expect_fn("PRACTICE.StackDepth()")
        .reply_decimal(1)
        .expect_cmd("END")
        .reply_ok()
        .expect_fn("PRACTICE.StackDepth()")
        .reply_decimal(0)
}

/// `run proc attach` whose run ends with `ending`; it writes no output.
fn attach_with(tag: &str, ending: impl FnOnce(Script) -> Script) -> (World, Output) {
    let w = with_procs(tag);
    let s = ended(ending(started(attached(2), &w, "attach.cmm", &[])), &[]);
    let out = call("t32ctl run proc attach", &deps(w.env(), vec![Ok(s)]));
    (w, out)
}

/// The text after the header and the `proc` line.
fn result_lines(out: &Output) -> Vec<&str> {
    out.text.lines().skip(2).collect()
}

#[test]
fn a_procedure_runs_from_the_cached_profile_bundle_and_writes_its_outputs_to_a_fresh_directory() {
    let w = with_procs("proc-ok");
    let report = b"clocks: locked\n";
    let files: [(&str, &[u8]); 1] = [("selftest.txt", report)];
    let script = || {
        let s = started(connected(2), &w, "selftest.cmm", &[]);
        let s = s.expect_req(Request::PracticeState).silence(std::time::Duration::from_millis(2400)).reply_state(0);
        let s = tk::get(s, Macro::Done, tk::TOKEN).expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
        ended(tk::read_back(s, "", "0", "clocks locked, pmic ok", ""), &files)
    };
    let out = call("t32ctl read proc selftest", &deps(w.env(), vec![Ok(script())]));
    let dirs = run_dirs(&w);
    assert_eq!(dirs.len(), 1);
    let id = dirs[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(id.starts_with("p-"), "{id}");
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             proc selftest (profile={}, 4 files, cached)\n\
             result: finished rc=0 after 2.4s: \"clocks locked, pmic ok\"\n\
             outputs: ~/.local/state/t32ctl/out/{id}/ (1 file)\n  \
             selftest.txt  15\n",
            &profile_hash(&w)[..8]
        )
    );
    assert_eq!(out.code, 0);
    assert_eq!(fs::read(dirs[0].join("selftest.txt")).unwrap(), report);
    let journal = w.pv_dir().join("journal");
    assert!(journal.read_dir().map_or(true, |mut d| d.next().is_none()), "a clean close leaves no journal");

    fs::remove_dir_all(&dirs[0]).unwrap();
    let out = call("t32ctl read proc selftest --json", &deps(w.env(), vec![Ok(script())]));
    let id = run_dirs(&w)[0].file_name().unwrap().to_string_lossy().into_owned();
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(
        v["body"],
        serde_json::json!({
            "kind": "run", "what": "proc", "name": "selftest", "grade": "read",
            "profile": {"hash": profile_hash(&w), "files": 4, "cached": true}, "params": [],
            "result": {"outcome": "finished", "rc": 0, "message": "clocks locked, pmic ok", "after_ms": 2400},
            "outputs": {"dir": format!("~/.local/state/t32ctl/out/{id}/"),
                        "files": [{"name": "selftest.txt", "bytes": 15}], "missing": []}
        })
    );
}

#[test]
fn a_finished_run_is_exit_0_only_for_rc_0_or_no_rc() {
    let (_, out) = attach_with("proc-rc0", |s| finished(s, "", "0", ""));
    assert_eq!((result_lines(&out), out.code), (vec!["result: finished rc=0 after 0ms", "outputs: none"], 0));
    let (_, out) = attach_with("proc-rc-unset", |s| finished(s, "", "", "attached"));
    assert_eq!((result_lines(&out), out.code), (vec!["result: finished after 0ms: \"attached\"", "outputs: none"], 0));
    let (_, out) = attach_with("proc-rc3", |s| finished(s, "", "3.", "pmic not ready"));
    assert_eq!(
        (result_lines(&out), out.code),
        (vec!["result: finished rc=3 after 0ms: \"pmic not ready\"", "outputs: none"], 1)
    );
    let (_, out) = attach_with("proc-rc-text", |s| finished(s, "", "\"busy\"", ""));
    assert_eq!((result_lines(&out), out.code), (vec!["result: finished rc=\"busy\" after 0ms", "outputs: none"], 1));
    assert!(!out.text.contains("error["), "exit 1 has no error line");
}

#[test]
fn a_caught_error_a_halt_and_a_timeout_are_exit_1_with_what_happened() {
    let (_, out) = attach_with("proc-caught", |s| finished(s, "1", "", "unknown command"));
    assert_eq!(
        (result_lines(&out), out.code),
        (
            vec![
                "result: failed by a PRACTICE error after 0ms",
                "  message: unknown command",
                "  the runner caught it; staged files were removed",
                "outputs: none",
            ],
            1
        )
    );
    let (_, out) = attach_with("proc-halted", |mut s| {
        for _ in 0..2 {
            s = s.expect_req(Request::PracticeState).reply_state(0);
            s = tk::get(s, Macro::Done, "").expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
        }
        s = s.expect_cmd("EVAL ERROR.MESSAGE()").reply_ok().expect_req(Request::EvalString).reply_string("bus error");
        s = s
            .expect_cmd("EVAL ERROR.CMDLINE()")
            .reply_ok()
            .expect_req(Request::EvalString)
            .reply_string("Data.Set A:0x0 1");
        unwound(s, 0)
    });
    assert_eq!(
        (result_lines(&out), out.code),
        (
            vec![
                "result: halted by a PRACTICE error after 0ms",
                "  message: bus error",
                "  line:    Data.Set A:0x0 1",
                "  the script was unwound; staged files were removed",
                "outputs: none",
            ],
            1
        )
    );
    let (_, out) = attach_with("proc-timeout", |mut s| {
        for _ in 0..4 {
            s = s.expect_req(Request::PracticeState).silence(std::time::Duration::from_secs(8)).reply_state(1);
        }
        unwound(s, 1)
    });
    assert_eq!(
        (result_lines(&out), out.code),
        (vec!["result: timed out after 30s; the script was stopped and unwound", "outputs: none"], 1)
    );
}

#[test]
fn a_dialog_or_a_vanished_runner_is_exit_5_after_the_body() {
    let (_, out) = attach_with("proc-dialog", |s| unwound(s.expect_req(Request::PracticeState).reply_state(2), 2));
    assert_eq!(
        (result_lines(&out), out.code),
        (
            vec![
                "outputs: none",
                "error[dialog]: the script opened a dialog; t32ctl stopped it and unwound the script, and the window \
                 may still be open in PowerView",
            ],
            5
        )
    );
    let (_, out) = attach_with("proc-vanished", |s| {
        let s = s.expect_req(Request::PracticeState).reply_state(0);
        tk::get(s, Macro::Done, "").expect_fn("PRACTICE.StackDepth()").reply_decimal(0)
    });
    assert_eq!(out.code, 5);
    assert_eq!(
        out.text.lines().last(),
        Some(
            "error[script-vanished]: the run ended without the runner reaching its end: something outside t32ctl \
             cleared the PRACTICE stack"
        )
    );
}

#[test]
fn a_declared_output_the_run_did_not_write_is_missing_and_exit_1() {
    let w = with_procs("proc-missing");
    let s = ended(finished(started(connected(2), &w, "selftest.cmm", &[]), "", "0", ""), &[]);
    let out = call("t32ctl read proc selftest", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        (result_lines(&out), out.code),
        (vec!["result: finished rc=0 after 0ms", "outputs: none", "missing: selftest.txt"], 1)
    );
    assert_eq!(run_dirs(&w), Vec::<std::path::PathBuf>::new(), "an empty output directory is removed");
}

#[test]
fn the_grade_word_must_be_the_declared_grade_and_the_hint_keeps_the_parameters() {
    let w = with_procs("proc-grade");
    let out = call("t32ctl run proc selftest --x 'a b' --build b", &deps(w.env(), vec![]));
    assert_eq!(
        out.text,
        "error[grade-mismatch]: proc selftest runs at grade read, not run\n\
         next: t32ctl read proc selftest --x 'a b' --build b\n"
    );
    assert_eq!(out.code, 2);
    let out = call("t32ctl write proc flsh", &deps(w.env(), vec![]));
    assert_eq!(out.code, 2);
    assert!(
        out.text.starts_with("error[unknown-proc]: the board profile declares no procedure \"flsh\""),
        "{}",
        out.text
    );
    assert_eq!(run_dirs(&w), Vec::<std::path::PathBuf>::new());
    let out = call("t32ctl write proc selftest --json", &deps(w.env(), vec![]));
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["next"], serde_json::json!(["t32ctl read proc selftest"]));
}

#[test]
fn the_grade_mismatch_hint_keeps_a_build_clap_took_before_the_parameters() {
    let w = with_procs("proc-grade-build");
    for (line, next) in [
        ("t32ctl read proc flash --build bld --part user", "t32ctl burn proc flash --part user --build bld"),
        ("t32ctl read --build bld proc flash", "t32ctl burn proc flash --build bld"),
        ("t32ctl read --build bld proc flash --part user", "t32ctl burn proc flash --part user --build bld"),
    ] {
        let out = call(line, &deps(w.env(), vec![]));
        assert_eq!(
            (out.text.lines().last(), out.code),
            (Some(format!("next: {next}").as_str()), 2),
            "{line}\n{}",
            out.text
        );
    }
}

#[test]
fn parameters_that_do_not_match_the_declaration_are_proc_params_before_connecting() {
    let w = with_procs("proc-params");
    w.write("ws/empty.bin", "");
    let usage = "  usage: t32ctl burn proc flash [--image FILE] [--part boot0|boot1|user] [--offset N]";
    for (args, want) in [
        ("--speed 3", "proc flash declares no parameter --speed"),
        ("--part user --part boot0", "--part is given twice"),
        ("--part emmc", "--part emmc: not one of boot0, boot1, user"),
        ("--offset -1", "--offset -1: -1 is below the minimum 0"),
        ("extra", "extra is not a --PARAM VALUE pair"),
        ("--offset", "--offset needs a value"),
        (
            "",
            "--image: the default {build}/zephyr/zephyr.bin needs a build directory; pass --image FILE or --build DIR",
        ),
        ("--image missing.bin", "--image missing.bin: No such file or directory (os error 2)"),
        ("--image empty.bin", "--image empty.bin: an empty file"),
        ("--image board", "--image board: not a regular file"),
    ] {
        let out = call(&format!("t32ctl burn proc flash {args}"), &deps(w.env(), vec![]));
        assert_eq!(
            (out.text.as_str(), out.code),
            (format!("error[proc-params]: {want}\n{usage}\n").as_str(), 2),
            "{args}"
        );
    }
}

#[test]
fn help_after_the_name_prints_the_declaration() {
    let w = with_procs("proc-help");
    let out = call("t32ctl burn proc flash --help", &deps(w.env(), vec![]));
    let listed = call("t32ctl read procs flash", &deps(w.env(), vec![]));
    assert_eq!((out.text.as_str(), out.code), (listed.text.as_str(), 0));
}

#[test]
fn a_busy_powerview_refuses_the_run_without_starting_anything() {
    let w = with_procs("proc-busy");
    let busy = |state: u8, depth: Option<i64>| {
        let s = tk::cached(tk::opened(attached(2)), &key(&w)).expect_req(Request::PracticeState).reply_state(state);
        let s = match depth {
            Some(d) => s.expect_fn("PRACTICE.StackDepth()").reply_decimal(d),
            None => s,
        };
        stopped_header(tk::closed(s, false))
    };
    let out = call("t32ctl run proc attach", &deps(w.env(), vec![Ok(busy(1, None))]));
    assert_eq!(out.code, 4);
    assert_eq!(
        out.text.lines().nth(1),
        Some("error[pv-script-running]: the run: a PRACTICE script is running in PowerView; nothing was started")
    );
    let out = call("t32ctl run proc attach", &deps(w.env(), vec![Ok(busy(0, Some(2)))]));
    assert_eq!(out.code, 5);
    assert_eq!(
        out.text.lines().nth(1),
        Some(
            "error[pv-script-halted]: the run: a PRACTICE script t32ctl did not start has halted on the stack (depth \
             2); someone must end it in PowerView"
        )
    );
}

#[test]
fn a_window_another_process_holds_is_vm_busy() {
    let w = with_procs("proc-vm-busy");
    let window = VmWindow::new(VmWindow::DEFAULT_BASE, VmWindow::DEFAULT_LEN).unwrap();
    let _held = VmLease::acquire(&w.pv_dir(), window, "t32probe").unwrap();
    let s = connected(2).expect_fn("OS.PTD()").reply_text(tk::PTD);
    let out = call("t32ctl read proc selftest", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        (out.text.lines().nth(1), out.code),
        (
            Some(
                format!(
                    "error[vm-busy]: opening the stage: VM: window is leased by another process (pid {} t32probe)",
                    std::process::id()
                )
                .as_str()
            ),
            4
        )
    );
}

/// `burn proc flash` with a 6000-byte image through a 4 KiB window, up to the run's `DO`.
fn flash_started(w: &World) -> Script {
    let image = format!("{}\\image.bin", tk::at("in-tok"));
    let mut s = tk::opened(connected(2));
    s = s.expect_fn(&format!("OS.DIR(\"{}\")", tk::at("in-tok"))).reply_bool(false);
    s = s.expect_cmd(&format!("MKDIR \"{}\"", tk::at("in-tok"))).reply_ok();
    let bytes: Vec<u8> = (0..6000).map(|i| (i % 253) as u8).collect();
    s = s
        .expect_req(Request::MemWrite { access: "VM".into(), addr: 0x0100_0000, data: bytes[..4096].to_vec() })
        .reply_ok();
    s = s.expect_cmd(&format!("Data.SAVE.Binary \"{image}\" VM:0x01000000++0x00000FFF")).reply_ok();
    s = s
        .expect_req(Request::MemWrite { access: "VM".into(), addr: 0x0100_0000, data: bytes[4096..].to_vec() })
        .reply_ok();
    s = s.expect_cmd(&format!("Data.SAVE.Binary \"{image}\" VM:0x01000000++0x0000076F /Append")).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.SIZE(\"{image}\")")).reply_decimal(6000);
    let s = tk::cached(s, &key(w));
    tk::run_started(s, &key(w), "flash_emmc.cmm", "", true, &[("image", &image), ("part", "user"), ("offset", "0.")])
}

fn flash_world(tag: &str) -> World {
    let w = with_procs(tag);
    w.write("home/.config/t32ctl/config.toml", "schema = 1\n[staging]\nvm_len = 0x1000\n");
    w.write("ws/image.bin", "");
    fs::write(w.0.join("ws/image.bin"), (0..6000).map(|i| (i % 253) as u8).collect::<Vec<u8>>()).unwrap();
    w
}

/// Cleanup after a flash run with `files`: the outputs, the parameter macros, the output
/// directory, the image and its directory, the t32ctl macros.
fn flash_closed(mut s: Script, files: &[(&str, &[u8])]) -> Script {
    let out = tk::out_dir();
    for (name, _) in files.iter().rev() {
        s = tk::file_removed(s, &format!("{out}\\{name}"));
    }
    if !files.is_empty() {
        s = tk::file_absent(s, &format!("{out}.tar.gz"));
        s = tk::file_removed(s, &format!("{out}.tar"));
    }
    for name in ["image", "part", "offset"] {
        s = tk::set_name(s, &format!("t32ctl_p_{name}"), "");
    }
    s = tk::dir_removed(s, &out);
    s = tk::file_removed(s, &format!("{}\\image.bin", tk::at("in-tok")));
    tk::closed(tk::dir_removed(s, &tk::at("in-tok")), false)
}

#[test]
fn a_file_parameter_is_uploaded_in_slices_and_a_burn_needs_an_explicit_rc() {
    let w = flash_world("proc-flash");
    let log: [(&str, &[u8]); 1] = [("verify.log", b"ok\n")];
    let s = finished(flash_started(&w), "", "0", "wrote 6000 bytes to user at 0x0, verified");
    let s = stopped_header(flash_closed(tk::fetched(s, &log), &log));
    let out = call("t32ctl burn proc flash --image image.bin", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        out.text.lines().skip(1).take(3).collect::<Vec<_>>(),
        [
            &format!("proc flash (profile={}, 4 files, cached)", &profile_hash(&w)[..8]) as &str,
            "params: image=image.bin (6000, 2 slices) part=user offset=0",
            "result: finished rc=0 after 0ms: \"wrote 6000 bytes to user at 0x0, verified\"",
        ]
    );
    assert_eq!(out.code, 0);

    let s = stopped_header(flash_closed(tk::fetched(finished(flash_started(&w), "", "", ""), &log), &log));
    let out = call("t32ctl burn proc flash --image image.bin", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        (out.text.lines().nth(3), out.code),
        (
            Some(
                "result: finished without rc after 0ms: a burn procedure must set &t32ctl_rc=\"0\" after its \
                 verification"
            ),
            1
        )
    );

    let s = stopped_header(flash_closed(tk::fetched(finished(flash_started(&w), "", "0", ""), &log), &log));
    let out = call("t32ctl burn proc flash --image image.bin --json", &deps(w.env(), vec![Ok(s)]));
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(
        v["body"]["params"],
        serde_json::json!([
            {"name": "image", "value": "image.bin",
             "file": {"path": "C:\\T\\t32ctl\\in-tok\\image.bin", "bytes": 6000, "slices": 2}},
            {"name": "part", "value": "user"},
            {"name": "offset", "value": "0"},
        ])
    );
}

/// A clock that sets the interrupt flag once the manual clock passes `at`, like a Ctrl-C during a
/// silent reply.
#[derive(Debug)]
struct CtrlC {
    clock: ManualClock,
    at: Instant,
    cancel: Cancel,
}

impl Clock for CtrlC {
    fn now(&self) -> Instant {
        let now = self.clock.now();
        if now >= self.at {
            self.cancel.trigger();
        }
        now
    }
}

#[test]
fn an_interrupted_burn_unwinds_after_a_reconnect_and_warns_about_the_storage() {
    let w = flash_world("proc-ctrl-c");
    let first = flash_started(&w).expect_req(Request::PracticeState).silence(std::time::Duration::from_secs(1));
    let second =
        Script::new().attach().expect_req(Request::PracticeState).reply_state(1).expect_req(Request::Stop).reply_ok();
    let second =
        second.expect_fn("PRACTICE.StackDepth()").reply_decimal(1).expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    let second = second.expect_cmd("END").reply_ok().expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    let second = stopped_header(flash_closed(second, &[]));
    let clock = ManualClock::new();
    let cancel = Cancel::new();
    let connector = ScriptedConnector::new(clock.clone()).push(Ok(first)).push(Ok(second));
    let mut d = deps(w.env(), vec![]);
    d.connector = Arc::new(connector);
    d.clock =
        Arc::new(CtrlC { at: clock.now() + std::time::Duration::from_millis(500), clock, cancel: cancel.clone() });
    d.cancel = cancel;
    let out = call("t32ctl burn proc flash --image image.bin", &d);
    assert_eq!(out.code, 130, "{}", out.text);
    let lines: Vec<&str> = out.text.lines().collect();
    assert_eq!(lines[0], "stopped core=0 pc=0x40083e10");
    assert!(lines[1].starts_with("error[interrupted]: the run: "), "{}", out.text);
    assert_eq!(
        lines[2..],
        ["  the burn procedure was interrupted: the storage may hold a partial image", "  cleanup ran: 6 steps"]
    );
}

#[test]
fn an_upload_past_the_transfer_bound_runs_nothing_and_is_exit_1() {
    let w = flash_world("proc-upload-bound");
    w.write(
        "home/.config/t32ctl/config.toml",
        "schema = 1\n[staging]\nvm_len = 0x1000\n[calls]\ntransfer_timeout = \"1s\"\n",
    );
    let mut first = tk::opened(connected(2));
    first = first.expect_fn(&format!("OS.DIR(\"{}\")", tk::at("in-tok"))).reply_bool(false);
    first = first.expect_cmd(&format!("MKDIR \"{}\"", tk::at("in-tok"))).reply_ok();
    first = first.expect(t32rcl::testkit::Expect::MemWriteLen { access: "VM".into(), addr: 0x0100_0000, len: 4096 });
    let first = first.silence(std::time::Duration::from_secs(2));
    let mut second = Script::new().attach();
    second = tk::file_absent(second, &format!("{}\\image.bin", tk::at("in-tok")));
    let second = stopped_header(tk::closed(tk::dir_removed(second, &tk::at("in-tok")), false));
    let out = call("t32ctl burn proc flash --image image.bin", &deps(w.env(), vec![Ok(first), Ok(second)]));
    assert_eq!(
        (result_lines(&out), out.code),
        (
            vec![
                "params: image=image.bin part=user offset=0",
                "result: the upload of --image stopped at the 1s bound; the procedure did not run",
                "outputs: none",
            ],
            1
        )
    );
}

#[test]
fn a_target_down_error_hints_the_boards_attach_procedure() {
    let w = with_procs("proc-attach-hint");
    let down = Script::new().attach().expect_req(Request::TargetState).reply_state(0);
    let down = down.expect_req(Request::TargetState).reply_state(0);
    let out = call("t32ctl read reg pc", &deps(w.env(), vec![Ok(down)]));
    assert_eq!(
        out.text,
        "down\nerror[target-down]: the target is down: registers need an attached target\nnext: t32ctl run proc attach\n"
    );
    let down = Script::new().attach().expect_req(Request::TargetState).reply_state(0);
    let down = down.expect_req(Request::TargetState).reply_state(0);
    let out = call("t32ctl read reg pc --build bld", &deps(w.env(), vec![Ok(down)]));
    assert_eq!(out.text.lines().last(), Some("next: t32ctl run proc attach --build bld"));
}

#[test]
fn our_script_left_on_the_stack_after_cleanup_is_residue_and_overrides_exit_0() {
    let w = with_procs("proc-residue");
    let s = started(attached(2), &w, "attach.cmm", &[]).expect_req(Request::PracticeState).reply_state(0);
    let s = tk::get(s, Macro::Done, tk::TOKEN).expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    let mut s = tk::fetched(tk::read_back(s, "", "0", "", ""), &[]);
    s = s.expect_req(Request::PracticeState).reply_state(0).expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    for _ in 0..3 {
        s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1).expect_cmd("END").reply_ok();
    }
    s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    let s = stopped_header(tk::closed(tk::outputs_removed(s, &[]), false));
    let out = call("t32ctl run proc attach", &deps(w.env(), vec![Ok(s)]));
    let end = "in PowerView type: END (END also closes every dialog and OPEN file)";
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             ! residue: unwind PRACTICE stack; {end}\n\
             proc attach (profile={}, 4 files, cached)\n\
             result: finished rc=0 after 0ms\n\
             outputs: none\n\
             error[residue]: the script t32ctl started could not be unwound (failed: our PRACTICE script did not \
             end: stack depth 1 after T32_Stop and END); {end}\n",
            &profile_hash(&w)[..8]
        )
    );
    assert_eq!(out.code, 5);
}

#[test]
fn a_failed_output_fetch_keeps_the_result_and_is_pv_error() {
    let w = with_procs("proc-fetch");
    let out_dir = tk::out_dir();
    let s = finished(started(attached(2), &w, "attach.cmm", &[]), "", "0", "");
    let s = s.expect_fn(&format!("OS.FIRSTFILE(\"{out_dir}\\*\")")).reply_text("x.log");
    let s = s.expect_cmd(&format!("TAR \"{out_dir}.tar\" \"{out_dir}\\*\"")).reply_error(90, "refused");
    let s = tk::file_absent(s, &format!("{out_dir}.tar"));
    let s = s.expect_fn(&format!("OS.DIR(\"{out_dir}\")")).reply_bool(true);
    let s = s.expect_cmd(&format!("RMDIR \"{out_dir}\"")).reply_error(90, "critical error accessing file");
    let s = stopped_header(tk::closed(s, false));
    let out = call("t32ctl run proc attach", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(out.code, 5);
    let lines: Vec<&str> = out.text.lines().collect();
    assert_eq!(lines[1], format!("! residue: remove dir {out_dir}; in PowerView type: RMDIR \"{out_dir}\""));
    assert_eq!(lines[3..5], ["result: finished rc=0 after 0ms", "outputs: none"]);
    assert_eq!(
        lines[5],
        "error[pv-error]: fetching the outputs: command: PowerView refused: command failed (code 90 FN1): refused"
    );
}
