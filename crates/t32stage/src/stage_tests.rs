// SPDX-License-Identifier: Apache-2.0
use t32rcl::{
    Tuning,
    testkit::{Expect, Request, Script},
};

use super::*;
use crate::{
    bundle::BundleLimits,
    testutil::{session_with, temp_dir},
};

const BASE: u32 = 0x0100_0000;

fn config(tag: &str) -> StageConfig {
    StageConfig {
        window: VmWindow::new(BASE, 0x10_0000).unwrap(),
        guard: VmGuard::Dedicated,
        state_dir: temp_dir(tag),
        root: None,
        subdir: None,
        retention: Retention::Cache,
        exists: ExistsFn::OsFileReadable,
        poll: PollSchedule { initial: Duration::ZERO, max: Duration::ZERO, start_grace: Duration::ZERO },
        holder: "test".into(),
        journal_name: tag.into(),
    }
}

fn opened(s: Script) -> Script {
    s.expect_fn("OS.PTD()")
        .reply_text("C:\\T")
        .expect_cmd(
            "GLOBAL &t32ctl_snap &t32ctl_out &t32ctl_token &t32ctl_entry &t32ctl_args &t32ctl_done &t32ctl_fail \
             &t32ctl_rc &t32ctl_msg &t32ctl_ret",
        )
        .reply_ok()
        .expect_fn("OS.DIR(\"C:\\T\\t32ctl\")")
        .reply_bool(true)
}

fn set(s: Script, m: Macro, v: &str) -> Script {
    s.expect_req(Request::MacroSet { name: m.amp(), value: v.into() }).reply_macro("")
}

fn get(s: Script, m: Macro, v: &str) -> Script {
    s.expect_req(Request::MacroGet { name: m.amp() }).reply_macro(v)
}

fn idle(s: Script, depth: i64) -> Script {
    s.expect_req(Request::PracticeState).reply_state(0).expect_fn("PRACTICE.StackDepth()").reply_decimal(depth)
}

/// Close: the PRACTICE unwind finds an empty stack, then every t32ctl macro is cleared.
fn closed(mut s: Script, unwind: bool) -> Script {
    if unwind {
        s = idle(s, 0);
    }
    for m in Macro::ALL {
        s = set(s, *m, "");
    }
    s
}

/// The token, the result macros emptied, then `ERROR.RESet` and `DO`.
fn started_do(mut s: Script, script: &str) -> Script {
    s = set(s, Macro::Token, "tok");
    for m in [Macro::Done, Macro::Fail, Macro::Rc, Macro::Msg, Macro::Ret] {
        s = set(s, m, "");
    }
    s.expect_cmd("ERROR.RESet").reply_ok().expect_cmd(&format!("DO \"{script}\"")).reply_ok()
}

/// The `Finished` read-back: what the runner left in `fail`, `rc`, `msg` and `ret`.
fn read_back(mut s: Script, fail: &str, rc: &str, msg: &str, ret: &str) -> Script {
    s = get(s, Macro::Fail, fail);
    s = get(s, Macro::Rc, rc);
    s = get(s, Macro::Msg, msg);
    get(s, Macro::Ret, ret)
}

/// `DO`, polls and the `Finished` read-back.
fn executed(mut s: Script, script: &str, rc: &str, ret: &str) -> Script {
    s = started_do(s, script);
    s = s.expect_req(Request::PracticeState).reply_state(1);
    s = s.expect_req(Request::PracticeState).reply_state(0);
    s = get(s, Macro::Done, "tok").expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    read_back(s, "", rc, "", ret)
}

fn bundle() -> Bundle {
    let mut b = Bundle::new(BundleLimits::default());
    b.insert(RelPath::new("main.cmm").unwrap(), b"ENDDO 7.\n".to_vec()).unwrap();
    b
}

fn stage(session: &mut Session, cfg: StageConfig) -> Stage {
    let mut st = Stage::open(session, cfg).unwrap();
    st.fixed_token = Some("tok".into());
    st
}

#[test]
fn a_failed_open_hands_its_cleanup_back_without_running_it() {
    let s = Script::new().attach().expect_fn("OS.PTD()").reply_text("C:\\T");
    let s = s.expect(Expect::CommandStarting("GLOBAL &t32ctl_snap".into())).reply_error(90, "refused");
    let mut session = session_with(vec![Ok(s)], |_| {});
    let cfg = config("stage-open-failed");
    let state_dir = cfg.state_dir.clone();
    let failed = Stage::open(&mut session, cfg).unwrap_err();
    assert!(matches!(failed.error, StageError::Rcl(_)), "{failed}");
    assert!(matches!(failed.cleanup.pending(), [Undo::ClearMacros(_)]));
    failed.cleanup.discard();
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn a_temp_dir_that_is_not_ascii_safe_needs_an_explicit_root() {
    let s = Script::new().attach().expect_fn("OS.PTD()").reply_text("C:\\Users\\김\\AppData\\Local\\Temp");
    let mut session = session_with(vec![Ok(s)], |_| {});
    let cfg = config("stage-open-non-ascii");
    let state_dir = cfg.state_dir.clone();
    let failed = Stage::open(&mut session, cfg).unwrap_err();
    assert!(matches!(failed.error, StageError::Path(PathError::Char { bad: '김', .. })), "{failed}");
    assert!(failed.cleanup.pending().is_empty());
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn a_cached_snapshot_costs_one_call_and_a_run_finishes_by_token() {
    let b = bundle();
    let dir = format!("C:\\T\\t32ctl\\{}", b.key());
    let mut s = opened(Script::new().attach());
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(true);
    s = idle(s, 0);
    s = set(s, Macro::Snap, &dir);
    s = set(s, Macro::Entry, "sub\\main.cmm");
    s = set(s, Macro::Args, "a 0x10");
    s = executed(s, &format!("{dir}\\.t32ctl\\runner.cmm"), "\"0\"", "7.");
    s = closed(s, false);
    let mut session = session_with(vec![Ok(s)], |_| {});

    let cfg = config("stage-cached");
    let state_dir = cfg.state_dir.clone();
    let mut st = stage(&mut session, cfg);
    let snap = st.ensure_snapshot(&mut session, &b).unwrap();
    assert!(snap.cached);
    let req = RunRequest {
        entry: RelPath::new("sub/main.cmm").unwrap(),
        args: vec!["a".into(), "0x10".into()],
        timeout: Duration::from_secs(5),
        collect_outputs: false,
    };
    let report = st.run(&mut session, &snap, &req).unwrap();
    assert_eq!(
        report.outcome,
        RunOutcome::Finished { caught: false, rc: ScriptRc::Int(0), message: String::new(), ret: "7.".into() }
    );
    assert_eq!(report.polls.running, 1);
    let cleanup = st.close(&mut session);
    assert!(cleanup.is_clean(), "{cleanup:?}");
    assert!(!state_dir.join("journal/stage-cached.undo").exists());
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn an_operator_script_started_after_our_run_is_never_unwound() {
    let b = bundle();
    let dir = format!("C:\\T\\t32ctl\\{}", b.key());
    let mut s = opened(Script::new().attach());
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(true);
    s = idle(s, 0);
    s = set(s, Macro::Snap, &dir);
    s = set(s, Macro::Entry, "main.cmm");
    s = set(s, Macro::Args, "");
    s = executed(s, &format!("{dir}\\.t32ctl\\runner.cmm"), "0", "");
    s = s.expect_req(Request::PracticeState).reply_state(1);
    s = closed(s, false);
    let mut session = session_with(vec![Ok(s)], |_| {});

    let cfg = config("stage-foreign");
    let state_dir = cfg.state_dir.clone();
    let mut st = stage(&mut session, cfg);
    let snap = st.ensure_snapshot(&mut session, &b).unwrap();
    let req = RunRequest {
        entry: RelPath::new("main.cmm").unwrap(),
        args: vec![],
        timeout: Duration::from_secs(5),
        collect_outputs: false,
    };
    assert!(st.run(&mut session, &snap, &req).unwrap().outcome.is_success());
    assert!(matches!(st.run(&mut session, &snap, &req), Err(StageError::Busy { depth: -1 })));
    assert!(st.close(&mut session).is_clean());
    let _ = std::fs::remove_dir_all(state_dir);
}

/// Open, cached snapshot, then the run request up to and including `DO` of the runner.
fn started(entry: &str) -> (Bundle, String, Script) {
    let b = bundle();
    let dir = format!("C:\\T\\t32ctl\\{}", b.key());
    let mut s = opened(Script::new().attach());
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(true);
    s = idle(s, 0);
    s = set(s, Macro::Snap, &dir);
    s = set(s, Macro::Entry, entry);
    s = set(s, Macro::Args, "");
    (b, dir, s)
}

fn run_once(tag: &str, b: &Bundle, s: Script) -> RunOutcome {
    let mut session = session_with(vec![Ok(s)], |_| {});
    let cfg = config(tag);
    let state_dir = cfg.state_dir.clone();
    let mut st = stage(&mut session, cfg);
    let snap = st.ensure_snapshot(&mut session, b).unwrap();
    let req = RunRequest {
        entry: RelPath::new("main.cmm").unwrap(),
        args: vec![],
        timeout: Duration::from_secs(5),
        collect_outputs: false,
    };
    let outcome = st.run(&mut session, &snap, &req).unwrap().outcome;
    assert!(st.close(&mut session).is_clean());
    let _ = std::fs::remove_dir_all(state_dir);
    outcome
}

#[test]
fn a_script_halted_with_an_error_is_a_failure_with_the_error_text() {
    let (b, dir, s) = started("main.cmm");
    let mut s = started_do(s, &format!("{dir}\\.t32ctl\\runner.cmm"));
    for _ in 0..2 {
        s = s.expect_req(Request::PracticeState).reply_state(0);
        s = get(s, Macro::Done, "").expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    }
    s = s.expect_cmd("EVAL ERROR.MESSAGE()").reply_ok().expect_req(Request::EvalString).reply_string("unknown command");
    s = s.expect_cmd("EVAL ERROR.CMDLINE()").reply_ok().expect_req(Request::EvalString).reply_string("NO_SUCH");
    s = idle(s, 1).expect_fn("PRACTICE.StackDepth()").reply_decimal(1).expect_cmd("END").reply_ok();
    s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    s = closed(s, false);

    let outcome = run_once("stage-halted", &b, s);
    let error = PracticeErrorInfo { message: "unknown command".into(), cmdline: "NO_SUCH".into() };
    assert_eq!(outcome, RunOutcome::Halted { error: Some(error) });
    assert!(!outcome.is_success());
}

/// A run that finishes by token after one running sample, with `fail`, `rc` and `msg` read back.
fn finished_with(tag: &str, fail: &str, rc: &str, msg: &str) -> RunOutcome {
    let (b, dir, s) = started("main.cmm");
    let mut s = started_do(s, &format!("{dir}\\.t32ctl\\runner.cmm"));
    s = s.expect_req(Request::PracticeState).reply_state(0);
    s = get(s, Macro::Done, "tok").expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    let s = closed(read_back(s, fail, rc, msg, ""), false);
    run_once(tag, &b, s)
}

#[test]
fn a_runner_that_caught_an_error_is_a_failure_with_its_message_whatever_rc_holds() {
    let outcome = finished_with("stage-caught", "1", "", "unknown command");
    let want = RunOutcome::Finished {
        caught: true,
        rc: ScriptRc::Unset,
        message: "unknown command".into(),
        ret: String::new(),
    };
    assert_eq!(outcome, want);
    assert!(!outcome.is_success());
}

#[test]
fn the_entry_scripts_rc_and_message_come_back_as_it_set_them() {
    let outcome = finished_with("stage-rc3", "", "\"3\"", "pmic not ready");
    let want = RunOutcome::Finished {
        caught: false,
        rc: ScriptRc::Int(3),
        message: "pmic not ready".into(),
        ret: String::new(),
    };
    assert_eq!(outcome, want);
    assert!(outcome.is_success(), "the runner ended normally; rc is the caller's to judge");
    assert!(matches!(finished_with("stage-rc-unset", "", "", ""), RunOutcome::Finished { rc: ScriptRc::Unset, .. }));
}

#[test]
fn a_busy_stack_is_refused_and_a_dialog_is_stopped_and_unwound() {
    let b = bundle();
    let dir = format!("C:\\T\\t32ctl\\{}", b.key());
    let mut s = opened(Script::new().attach());
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(true);
    s = idle(s, 2);
    s = idle(s, 0);
    s = set(s, Macro::Snap, &dir);
    s = set(s, Macro::Entry, "main.cmm");
    s = set(s, Macro::Args, "");
    s = set(s, Macro::Token, "tok");
    for m in [Macro::Done, Macro::Fail, Macro::Rc, Macro::Msg, Macro::Ret] {
        s = set(s, m, "");
    }
    s = s.expect_cmd("ERROR.RESet").reply_ok().expect(Expect::CommandStarting("DO ".into())).reply_ok();
    s = s.expect_req(Request::PracticeState).reply_state(2);
    s = s.expect_req(Request::PracticeState).reply_state(2).expect_req(Request::Stop).reply_ok();
    s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1).expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    s = s.expect_cmd("END").reply_ok().expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    s = closed(s, false);
    let mut session = session_with(vec![Ok(s)], |_| {});

    let cfg = config("stage-dialog");
    let state_dir = cfg.state_dir.clone();
    let mut st = stage(&mut session, cfg);
    let snap = st.ensure_snapshot(&mut session, &b).unwrap();
    let req = RunRequest {
        entry: RelPath::new("main.cmm").unwrap(),
        args: vec![],
        timeout: Duration::from_secs(5),
        collect_outputs: false,
    };
    assert!(matches!(st.run(&mut session, &snap, &req), Err(StageError::Busy { depth: 2 })));
    assert_eq!(st.run(&mut session, &snap, &req).unwrap().outcome, RunOutcome::DialogAborted);
    assert!(st.close(&mut session).is_clean());
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn a_script_that_outlives_stop_and_end_is_an_unwind_failure_not_busy() {
    let b = bundle();
    let dir = format!("C:\\T\\t32ctl\\{}", b.key());
    let mut s = opened(Script::new().attach());
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(true);
    s = idle(s, 0);
    s = set(s, Macro::Snap, &dir);
    s = set(s, Macro::Entry, "main.cmm");
    s = set(s, Macro::Args, "");
    s = set(s, Macro::Token, "tok");
    for m in [Macro::Done, Macro::Fail, Macro::Rc, Macro::Msg, Macro::Ret] {
        s = set(s, m, "");
    }
    s = s.expect_cmd("ERROR.RESet").reply_ok().expect(Expect::CommandStarting("DO ".into())).reply_ok();
    s = s.expect_req(Request::PracticeState).reply_state(1).expect_req(Request::PracticeState).reply_state(1);
    s = s.expect_req(Request::Stop).reply_ok().expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    for _ in 0..3 {
        s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1).expect_cmd("END").reply_ok();
    }
    s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    s = closed(s, true);
    let mut session = session_with(vec![Ok(s)], |_| {});

    let cfg = config("stage-unwind-failed");
    let state_dir = cfg.state_dir.clone();
    let mut st = stage(&mut session, cfg);
    let snap = st.ensure_snapshot(&mut session, &b).unwrap();
    let req = RunRequest {
        entry: RelPath::new("main.cmm").unwrap(),
        args: vec![],
        timeout: Duration::ZERO,
        collect_outputs: false,
    };
    assert!(matches!(st.run(&mut session, &snap, &req), Err(StageError::UnwindFailed { depth: 1 })));
    assert!(st.close(&mut session).is_clean());
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn an_uncached_snapshot_is_uploaded_inflated_unpacked_and_verified() {
    let b = bundle();
    let key = b.key();
    let dir = format!("C:\\T\\t32ctl\\{key}");
    let packed = b.pack(BASE);
    let gz_at = BASE + packed.raw_len.next_multiple_of(4096);
    let tmp = format!("C:\\T\\t32ctl\\{key}.gz");

    let mut s = opened(Script::new().attach());
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(false);
    s = idle(s, 0);
    s = s.expect_fn(&format!("OS.DIR(\"{dir}\")")).reply_bool(false).expect_cmd(&format!("MKDIR \"{dir}\"")).reply_ok();
    s = s.expect_fn(&format!("OS.DIR(\"{dir}\\.t32ctl\")")).reply_bool(false);
    s = s.expect_cmd(&format!("MKDIR \"{dir}\\.t32ctl\"")).reply_ok();
    s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: gz_at.into(), data: packed.gz.clone() }).reply_ok();
    s = s.expect_cmd(&format!("Data.SAVE.Binary \"{tmp}\" VM:0x{gz_at:08X}++0x{:08X}", packed.gz.len() - 1)).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.SIZE(\"{tmp}\")")).reply_decimal(packed.gz.len() as i64);
    s = s.expect_cmd(&format!("Data.LOAD.Binary \"{tmp}\" VM:0x{BASE:08X} /UNZIP")).reply_ok();
    s = s
        .expect_cmd(&format!(
            "Data.SAVE.Binary \"{dir}\\.t32ctl\\unpack.cmm\" VM:0x{BASE:08X}++0x{:08X}",
            packed.unpack_len - 1
        ))
        .reply_ok();
    s = s.expect_cmd(&format!("RM \"{tmp}\"")).reply_ok();
    s = set(s, Macro::Snap, &dir);
    s = executed(s, &format!("{dir}\\.t32ctl\\unpack.cmm"), "0", "");
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(true);
    s = s.expect_fn(&format!("OS.FILE.readable(\"{tmp}\")")).reply_bool(false);
    s = closed(s, false);
    let mut session = session_with(vec![Ok(s)], |o| o.tuning = Tuning::new(1008, 0x3C00).unwrap());

    let cfg = config("stage-upload");
    let state_dir = cfg.state_dir.clone();
    let mut st = stage(&mut session, cfg);
    let snap = st.ensure_snapshot(&mut session, &b).unwrap();
    assert!(!snap.cached);
    assert_eq!(snap.upload.map(|u| u.gz_bytes), Some(packed.gz.len() as u64));
    assert!(st.close(&mut session).is_clean());
    let _ = std::fs::remove_dir_all(state_dir);
}

/// The `.bak` removals a transient stage registers while it unpacks `bundle()` into a snapshot
/// directory that does or does not exist yet.
fn bak_undos_after_unpack(tag: &str, dir_exists: bool) -> Vec<String> {
    let b = bundle();
    let key = b.key();
    let dir = format!("C:\\T\\t32ctl\\{key}");
    let packed = b.pack(BASE);
    let gz_at = BASE + packed.raw_len.next_multiple_of(4096);
    let tmp = format!("C:\\T\\t32ctl\\{key}.gz");

    let mut s = opened(Script::new().attach());
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(false);
    s = idle(s, 0);
    for d in [dir.clone(), format!("{dir}\\.t32ctl")] {
        s = s.expect_fn(&format!("OS.DIR(\"{d}\")")).reply_bool(dir_exists);
        if !dir_exists {
            s = s.expect_cmd(&format!("MKDIR \"{d}\"")).reply_ok();
        }
    }
    s = s.expect(Expect::MemWriteLen { access: "VM".into(), addr: gz_at.into(), len: packed.gz.len() }).reply_ok();
    s = s.expect(Expect::CommandStarting(format!("Data.SAVE.Binary \"{tmp}\""))).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.SIZE(\"{tmp}\")")).reply_decimal(packed.gz.len() as i64);
    s = s.expect(Expect::CommandStarting(format!("Data.LOAD.Binary \"{tmp}\""))).reply_ok();
    s = s.expect(Expect::CommandStarting(format!("Data.SAVE.Binary \"{dir}\\.t32ctl\\unpack.cmm\""))).reply_ok();
    s = s.expect_cmd(&format!("RM \"{tmp}\"")).reply_ok();
    s = set(s, Macro::Snap, &dir);
    s = executed(s, &format!("{dir}\\.t32ctl\\unpack.cmm"), "0", "");
    s = s.expect_fn(&format!("OS.FILE.readable(\"{dir}\\.t32ctl\\complete\")")).reply_bool(true);
    let mut session = session_with(vec![Ok(s)], |o| o.tuning = Tuning::new(1008, 0x3C00).unwrap());

    let cfg = StageConfig { retention: Retention::Transient, ..config(tag) };
    let state_dir = cfg.state_dir.clone();
    let mut st = stage(&mut session, cfg);
    st.ensure_snapshot(&mut session, &b).unwrap();
    let prefix = format!("remove file {dir}\\");
    let baks = st
        .pending()
        .iter()
        .map(Undo::describe)
        .filter(|d| d.ends_with(".bak"))
        .map(|d| d.strip_prefix(&prefix).unwrap_or(&d).to_string())
        .collect();
    drop(st);
    let _ = std::fs::remove_dir_all(state_dir);
    baks
}

#[test]
fn a_transient_stage_removes_the_bak_files_of_a_snapshot_it_unpacked_again() {
    let mut baks = bak_undos_after_unpack("stage-bak-repair", true);
    baks.sort();
    assert_eq!(
        baks,
        [
            ".t32ctl\\complete.bak",
            ".t32ctl\\runner.bak",
            ".t32ctl\\runner.cmm.bak",
            ".t32ctl\\unpack.bak",
            ".t32ctl\\unpack.cmm.bak",
            "main.bak",
            "main.cmm.bak",
        ]
    );
    assert!(bak_undos_after_unpack("stage-bak-fresh", false).is_empty());
}

#[test]
fn download_gunzips_what_powerview_zipped() {
    let payload: Vec<u8> = (0..3000).map(|i| (i % 13) as u8).collect();
    let gz = transfer::gzip(&payload);
    let mut s = opened(Script::new().attach());
    s = s.expect_fn("OS.FILE.SIZE(\"C:\\T\\t32ctl\\a.bin\")").reply_decimal(payload.len() as i64);
    s = s.expect_cmd("ZIP \"C:\\T\\t32ctl\\a.bin\" \"C:\\T\\t32ctl\\a.bin.gz\"").reply_ok();
    s = s.expect_fn("OS.FILE.SIZE(\"C:\\T\\t32ctl\\a.bin.gz\")").reply_decimal(gz.len() as i64);
    s = s.expect_cmd(&format!("Data.LOAD.Binary \"C:\\T\\t32ctl\\a.bin.gz\" VM:0x{BASE:08X}")).reply_ok();
    s = s.expect_req(Request::MemRead { access: "VM".into(), addr: BASE.into(), len: gz.len() as u16 }).reply_mem(&gz);
    s = s.expect_cmd("RM \"C:\\T\\t32ctl\\a.bin.gz\"").reply_ok();
    s = s.expect_fn("OS.FILE.readable(\"C:\\T\\t32ctl\\a.bin.gz\")").reply_bool(false);
    s = closed(s, false);
    let mut session = session_with(vec![Ok(s)], |o| o.tuning = Tuning::new(0x3C00, 0x3C00).unwrap());

    let cfg = config("stage-download");
    let state_dir = cfg.state_dir.clone();
    let mut st = stage(&mut session, cfg);
    let src = st.path("a.bin").unwrap();
    assert_eq!(st.download(&mut session, &src).unwrap(), payload);
    assert!(st.close(&mut session).is_clean());
    let _ = std::fs::remove_dir_all(state_dir);
}
