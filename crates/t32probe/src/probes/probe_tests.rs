// SPDX-License-Identifier: Apache-2.0
//! Probe decisions at the transport boundary: canned frames in, verdicts and measurements out.

use std::time::Duration;

use t32rcl::testkit::{Request, Script};
use t32stage::Symbol;

use crate::{
    exec::tests::{connect, run, test_env},
    outcome::{SkipReason, Value, Verdict},
    registry::ProbeId,
};

#[test]
fn reconnect_forces_a_timeout_and_the_executor_revives_once() {
    let first = Script::new().attach().expect_cmd("WAIT 5.s").silence(Duration::from_millis(400));
    let second = Script::new().attach().expect_fn("TRUE()").reply_bool(true);
    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("probe-reconnect");
    let r = run(&mut env, &mut session, &[ProbeId::Reconnect.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("kind"), Some(&Value::Text("idle".into())));
    assert_eq!(o.lookup("poisoned"), Some(&Value::Text("ok".into())));
    assert!(o.revive.as_ref().is_some_and(|v| v.ok && v.first_call.is_some()));
}

#[test]
fn keepalive_reports_the_interval_from_frame_times() {
    let mut s = Script::new().attach().expect_cmd("WAIT 3.s");
    for _ in 0..3 {
        s = s.silence(Duration::from_millis(1000)).keepalive(1);
    }
    let mut session = connect(vec![Ok(s.reply_ok())]);
    let mut env = test_env("probe-keepalive");
    let r = run(&mut env, &mut session, &[ProbeId::Keepalive.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass);
    assert_eq!(o.lookup("n"), Some(&Value::Count(3)));
    assert_eq!(o.lookup("interval"), Some(&Value::Seconds(1.0)));
    assert_eq!(o.lookup("raw"), Some(&Value::Text("fe00 fe00 fe00".into())));
}

#[test]
fn clients_counts_under_whichever_spelling_the_build_knows() {
    let s = Script::new().attach().expect_fn("RCL.TCP.NrUsedCons()").reply_decimal(1);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-clients-bare");
    let r = run(&mut env, &mut session, &[ProbeId::Clients.def()]);
    assert_eq!(r.outcomes[0].verdict, Verdict::Pass, "{:?}", r.outcomes[0]);
    assert_eq!(r.outcomes[0].lookup("NrUsedCons"), Some(&Value::Count(1)));

    let s = Script::new()
        .attach()
        .expect_fn("RCL.TCP.NrUsedCons()")
        .reply_function(0, "no function 'RCL.TCP.NRUSEDCONS' exists")
        .expect_fn("api.RCL.TCP.NrUsedCons()")
        .reply_decimal(2);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-clients-api");
    let r = run(&mut env, &mut session, &[ProbeId::Clients.def()]);
    assert_eq!(r.outcomes[0].verdict, Verdict::Pass, "{:?}", r.outcomes[0]);
    assert_eq!(r.outcomes[0].lookup("NrUsedCons"), Some(&Value::Count(2)));
}

#[test]
fn a_function_refused_on_an_old_build_is_unsupported_not_failed() {
    let s = Script::new()
        .attach()
        .expect_fn("RCL.TCP.NrUsedCons()")
        .reply_function(0, "unknown function")
        .expect_fn("api.RCL.TCP.NrUsedCons()")
        .reply_function(0, "unknown function")
        .expect_fn("SOFTWARE.BUILD()")
        .reply_decimal(150_000)
        .expect_fn("SOFTWARE.BUILD.BASE()")
        .reply_decimal(149_000);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-clients-old");
    let r = run(&mut env, &mut session, &[ProbeId::Clients.def()]);
    assert_eq!(r.outcomes[0].verdict, Verdict::Unsupported);

    let s = Script::new()
        .attach()
        .expect_fn("RCL.TCP.NrUsedCons()")
        .reply_function(0, "unknown function")
        .expect_fn("api.RCL.TCP.NrUsedCons()")
        .reply_function(0, "unknown function")
        .expect_fn("SOFTWARE.BUILD()")
        .reply_decimal(180_000)
        .expect_fn("SOFTWARE.BUILD.BASE()")
        .reply_decimal(179_000);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-clients-new");
    let r = run(&mut env, &mut session, &[ProbeId::Clients.def()]);
    assert_eq!(r.outcomes[0].verdict, Verdict::Fail, "a documented feature that fails is a failure");
}

#[test]
fn run_control_is_skipped_when_the_target_is_down() {
    let s = Script::new().attach().expect_req(Request::TargetState).reply_state(0);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-runcontrol");
    env.cfg.gates.run_control = Some(Symbol::parse("main").unwrap());
    let r = run(&mut env, &mut session, &[ProbeId::RunControl.def()]);
    assert_eq!(r.outcomes[0].verdict, Verdict::Skipped);
    assert!(matches!(&r.outcomes[0].skip, Some(SkipReason::TargetNotUp { state }) if state == "down"));
}

#[test]
fn version_never_fails_on_an_old_build_number() {
    let s = Script::new()
        .attach()
        .expect_fn("SOFTWARE.BUILD()")
        .reply_decimal(150_000)
        .expect_fn("VERSION.BUILD()")
        .reply_function(0, "unknown")
        .expect_fn("SOFTWARE.BUILD.BASE()")
        .reply_decimal(149_000)
        .expect_fn("VERSION.BUILD.BASE()")
        .reply_function(0, "unknown")
        .expect_fn("SOFTWARE.BUILD()")
        .reply_decimal(150_000)
        .expect_fn("SOFTWARE.BUILD.BASE()")
        .reply_decimal(149_000);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-version");
    let r = run(&mut env, &mut session, &[ProbeId::Version.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass);
    assert_eq!(o.lookup("min"), Some(&Value::Text("too-old".into())));
    assert_eq!(o.lookup("nrusedcons"), Some(&Value::Text("?".into())));
    assert_eq!(env.learned.build.map(|b| b.build), Some(150_000));
}

#[test]
fn a_macro_probe_that_dies_mid_trial_leaves_no_trial_spelling_behind() {
    let set = |name: &str, value: &str| Request::MacroSet { name: name.into(), value: value.into() };
    let first = Script::new().attach().expect_req(set("t32ctl_probe_a", "v1")).reply_exp_error(1, "unknown macro");
    let first = first.expect_req(set("&t32ctl_probe_a", "v1")).silence(Duration::from_millis(1100));
    let second = Script::new().attach().expect_fn("TRUE()").reply_bool(true);
    let second = second.expect_req(set("t32ctl_probe_a", "")).reply_macro("");
    let second = second.expect_req(set("t32ctl_probe_b", "")).reply_macro("");
    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("probe-macro-dies");
    let r = run(&mut env, &mut session, &[ProbeId::Macro.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail);
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
    assert_eq!(session.tuning().macro_spelling(), t32rcl::MacroSpelling::Bare);
}

fn staged(ctx: &mut crate::ctx::ProbeCtx<'_>) -> crate::ctx::ProbeResult {
    ctx.staged_file("x.bin")?;
    Ok(())
}

/// The probe stage of run `r1` at `C:\T\t32ctl\probe-r1`.
const STAGE: &str = "C:\\T\\t32ctl\\probe-r1";

/// `Stage::open` on a fresh run: root exists, `probe-r1` is created.
fn stage_opened(s: Script) -> Script {
    let s = s.expect_fn("OS.PTD()").reply_text("C:\\T");
    let s = s.expect(t32rcl::testkit::Expect::CommandStarting("GLOBAL &t32ctl_snap".into())).reply_ok();
    let s = s.expect_fn("OS.DIR(\"C:\\T\\t32ctl\")").reply_bool(true);
    let s = s.expect_fn(&format!("OS.DIR(\"{STAGE}\")")).reply_bool(false);
    s.expect_cmd(&format!("MKDIR \"{STAGE}\"")).reply_ok()
}

/// A directory below the stage that exists at cleanup and is removed.
fn dir_removed(s: Script, rel: &str) -> Script {
    let p = if rel.is_empty() { STAGE.to_string() } else { format!("{STAGE}\\{rel}") };
    let s = s.expect_fn(&format!("OS.DIR(\"{p}\")")).reply_bool(true);
    s.expect_cmd(&format!("RMDIR \"{p}\"")).reply_ok().expect_fn(&format!("OS.DIR(\"{p}\")")).reply_bool(false)
}

/// `Stage::close` after everything below `probe-r1` is gone: the directory, then the macros.
fn stage_closed(s: Script) -> Script {
    let mut s = dir_removed(s, "");
    for _ in 0..9 {
        s = s.expect(t32rcl::testkit::Expect::Any).reply_macro("");
    }
    s
}

#[test]
fn files_below_the_stage_unwind_before_the_stage_directory() {
    let mut s = stage_opened(Script::new().attach());
    s = s.expect_fn(&format!("OS.FILE.readable(\"{STAGE}\\x.bin\")")).reply_bool(true);
    s = s.expect_cmd(&format!("RM \"{STAGE}\\x.bin\"")).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.readable(\"{STAGE}\\x.bin\")")).reply_bool(false);
    s = stage_closed(s);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-staged");
    let def: &'static crate::registry::ProbeDef = Box::leak(Box::new(crate::registry::ProbeDef {
        id: ProbeId::VmFile,
        tier: crate::registry::Tier::Safe,
        requires: &[],
        needs_connection: true,
        deadline: Duration::from_secs(5),
        about: "",
        run: staged,
    }));
    let r = run(&mut env, &mut session, &[def]);
    assert_eq!(r.outcomes[0].verdict, Verdict::Pass);
    assert!(r.outcomes[0].cleanup.iter().all(|c| c.clean), "{:?}", r.outcomes[0].cleanup);
}

#[test]
fn tar_download_removes_a_directory_it_made_before_a_later_mkdir_failed() {
    let mut s = stage_opened(Script::new().attach()).expect_fn(&format!("OS.DIR(\"{STAGE}\")")).reply_bool(true);
    s = s.expect_fn(&format!("OS.DIR(\"{STAGE}\\o\")")).reply_bool(false);
    s = s.expect_cmd(&format!("MKDIR \"{STAGE}\\o\"")).reply_ok();
    s = s.expect_fn(&format!("OS.DIR(\"{STAGE}\\o\\sub\")")).reply_bool(false);
    s = s.expect_cmd(&format!("MKDIR \"{STAGE}\\o\\sub\"")).reply_error(90, "access denied");
    s = s.expect_fn(&format!("OS.DIR(\"{STAGE}\\o\\sub\")")).reply_bool(false);
    s = stage_closed(dir_removed(s, "o"));
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-tar-mkdir");
    let r = run(&mut env, &mut session, &[ProbeId::TarDownload.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail);
    assert!(o.cleanup.iter().any(|c| c.what.ends_with("probe-r1\\o") && c.result == "done"), "{:?}", o.cleanup);
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

#[test]
fn elf_unzip_reports_a_timed_out_direct_load_not_the_poisoned_call_after_it() {
    let elf = b"\x7fELF t32probe test".to_vec();
    let gz = t32stage::transfer::gzip(&elf);
    let (at, n) = (0x0180_0000u64, gz.len());
    let vm_read = Request::MemRead { access: "VM".into(), addr: at, len: n as u16 };
    let file = format!("{STAGE}\\x.elf.gz");
    let mut s = stage_opened(Script::new().attach()).expect_req(vm_read.clone()).reply_mem(&vec![0; n]);
    s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: at, data: gz.clone() }).reply_ok();
    s = s.expect_cmd(&format!("Data.SAVE.Binary \"{file}\" VM:0x01800000++0x{:08X}", n - 1)).reply_ok();
    for _ in 0..2 {
        s = s.expect_fn(&format!("OS.FILE.SIZE(\"{file}\")")).reply_decimal(n as i64);
    }
    s = s.expect_cmd(&format!("Data.LOAD.Elf \"{file}\" /UNZIP /NoCODE /NoReg"));
    let first = s.silence(Duration::from_millis(1100));
    let mut s = Script::new().attach().expect_fn("TRUE()").reply_bool(true);
    s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: at, data: vec![0; n] }).reply_ok();
    s = s.expect_req(vm_read).reply_mem(&vec![0; n]);
    s = s.expect_fn(&format!("OS.FILE.readable(\"{file}\")")).reply_bool(true);
    s = s.expect_cmd(&format!("RM \"{file}\"")).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.readable(\"{file}\")")).reply_bool(false);
    let second = stage_closed(s);
    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("probe-elf-timeout");
    env.cfg.gates.elf = Some(crate::cli::ElfInput { path: "x.elf".into(), bytes: elf, symbol: None });
    let r = run(&mut env, &mut session, &[ProbeId::ElfUnzip.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.error.as_ref().map(|e| e.tag.as_str()), Some("timeout"), "{:?}", o.error);
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

/// Every rung of the chunk ladder at `0x01000000`; `refuse` makes PowerView refuse the first
/// write of that rung.
fn ladder_script(refuse: Option<usize>) -> Script {
    use super::{pattern, storage::LADDER};
    let mut s = Script::new().attach();
    for (i, (r, w)) in LADDER.into_iter().enumerate() {
        let data = pattern(r as u64, 256 * 1024);
        for (k, chunk) in data.chunks(w).enumerate() {
            let addr = 0x0100_0000 + (k * w) as u64;
            s = s.expect_req(Request::MemWrite { access: "VM".into(), addr, data: chunk.to_vec() });
            if refuse == Some(i) {
                return s.reply_error(16, "bus error");
            }
            s = s.reply_ok();
        }
        for (k, chunk) in data.chunks(r).enumerate() {
            let addr = 0x0100_0000 + (k * r) as u64;
            s = s.expect_req(Request::MemRead { access: "VM".into(), addr, len: chunk.len() as u16 }).reply_mem(chunk);
        }
    }
    s
}

#[test]
fn the_chunk_ladder_calls_its_own_maximum_a_client_limit_and_a_refusal_not() {
    use super::storage::chunk_ladder;
    let region = t32stage::VmRegion::new(0x0100_0000, std::num::NonZeroU32::new(0x10_1000).unwrap()).unwrap();
    let all = chunk_ladder(&mut connect(vec![Ok(ladder_script(None))]), &region).unwrap();
    assert_eq!((all.token().as_deref(), all.stop), (Some("r15360/w15360(client-limit)"), None));
    let refused = chunk_ladder(&mut connect(vec![Ok(ladder_script(Some(2)))]), &region).unwrap();
    assert_eq!((refused.token().as_deref(), refused.stop.as_deref()), (Some("r2040/w2048"), Some("remote")));
}

#[test]
fn a_corrupt_gzip_powerview_loads_counts_as_accepted() {
    use super::storage::{corrupt_token, unzip_accepted};
    let f = t32stage::StageRoot::from_ptd("C:\\T").path(t32stage::RelPath::new("c.gz").unwrap());
    let load = "Data.LOAD.Binary \"C:\\T\\t32ctl\\c.gz\" VM:0x02000000 /UNZIP";
    let s = Script::new().attach().expect_cmd(load).reply_ok().expect_cmd(load).reply_error(90, "bad gzip");
    let mut session = connect(vec![Ok(s)]);
    assert!(unzip_accepted(&mut session, &f, 0x0200_0000).unwrap());
    assert!(!unzip_accepted(&mut session, &f, 0x0200_0000).unwrap());
    assert_eq!(corrupt_token(&["crc"], 4), "ACCEPTED(crc)");
    assert_eq!(corrupt_token(&[], 4), "rejected(4/4)");
}

#[test]
fn the_macro_probe_falls_back_to_the_ampersand_spelling_and_keeps_it() {
    let set = |name: &str, value: &str| Request::MacroSet { name: name.into(), value: value.into() };
    let get = |name: &str| Request::MacroGet { name: name.into() };
    let (a, b) = ("&t32ctl_probe_a", "&t32ctl_probe_b");
    let mut s = Script::new().attach().expect_req(set("t32ctl_probe_a", "v1")).reply_exp_error(1, "unknown macro");
    s = s.expect_req(set(a, "v1")).reply_macro("").expect_req(get(a)).reply_macro("v1");
    s = s.expect_req(set(a, "keep")).reply_macro("").expect_cmd("GLOBAL &t32ctl_probe_a").reply_ok();
    s = s.expect_req(get(a)).reply_macro("keep");
    s = s.expect(t32rcl::testkit::Expect::Any).reply_macro("").expect_req(get(b)).reply_macro("not it");
    for _ in 0..14 {
        s = s.expect(t32rcl::testkit::Expect::Any).reply_exp_error(1, "refused");
    }
    s = s.expect_req(set(a, "")).reply_macro("").expect_req(set(b, "")).reply_macro("");
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-macro-amp");
    let r = run(&mut env, &mut session, &[ProbeId::Macro.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup(crate::outcome::key::SPELLING), Some(&Value::Text("amp".into())));
    assert_eq!(o.lookup("max_value"), Some(&Value::Bytes(0)));
    assert_eq!(env.learned.macro_spelling, Some(t32rcl::MacroSpelling::Ampersand));
}

#[test]
fn tempdir_picks_the_literal_root_and_the_first_exists_function_that_answers() {
    let lit = "C:\\T\\t32ctl\\probe-r1-lit";
    let mac = "&t32ctl_root\\probe-r1-mac";
    let yes_no = |s: Script, expr: &str, v: bool| s.expect_fn(expr).reply_bool(v);
    let done = |s: Script, cmd: &str| s.expect_cmd(cmd).reply_ok();
    let save = |s: Script, file: &str| done(s, &format!("Data.SAVE.Binary \"{file}\" VM:0x01000000++0x00000000"));
    let reads = |mut s: Script| {
        for off in (0..4096u64).step_by(1008) {
            let len = (4096 - off).min(1008) as u16;
            let addr = 0x0100_0000 + off;
            s = s.expect_req(Request::MemRead { access: "VM".into(), addr, len }).reply_mem(&vec![0; len.into()]);
        }
        s
    };
    let mut s = reads(Script::new().attach().expect_fn("OS.PTD()").reply_text("C:\\T"));
    s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: 0x0100_0000, data: vec![0x5A] }).reply_ok();
    for (root, dir) in [("C:\\T\\t32ctl", lit), ("&t32ctl_root", mac)] {
        if root.starts_with('&') {
            s = done(done(s, "GLOBAL &t32ctl_root"), "&t32ctl_root=OS.PTD()+\"\\t32ctl\"");
            s = s.expect_req(Request::MacroGet { name: "t32ctl_root".into() }).reply_macro("C:\\T\\t32ctl");
        }
        s = done(yes_no(s, &format!("OS.DIR(\"{root}\")"), true), &format!("MKDIR \"{dir}\""));
        s = save(yes_no(s, &format!("OS.DIR(\"{dir}\")"), true), &format!("{dir}\\one.bin"));
        s = s.expect_fn(&format!("OS.FILE.SIZE(\"{dir}\\one.bin\")")).reply_decimal(1);
    }
    s = done(yes_no(s, "OS.DIR(\"&t32ctl_root\")", true), "EVAL OS.DIR(\"&t32ctl_root\")");
    s = s.expect_req(Request::EvalString).reply_string("TRUE()");
    s = yes_no(
        yes_no(s, &format!("OS.FILE.readable(\"{lit}\\one.bin\")"), true),
        &format!("OS.FILE.readable(\"{lit}\\missing.bin\")"),
        false,
    );
    for f in ["OS.FILE.EXIST", "FILE.EXIST"] {
        for name in ["one.bin", "missing.bin"] {
            s = s.expect_fn(&format!("{f}(\"{lit}\\{name}\")")).reply_function(0, "unknown function");
        }
    }
    s = s.expect_cmd(&format!("MKDIR \"{lit}\"")).reply_error(90, "exists");
    s = yes_no(done(s, &format!("MKDIR \"{lit}\\a\\b\"")), &format!("OS.DIR(\"{lit}\\a\\b\")"), true);
    s = done(done(s, &format!("RM \"{lit}\\missing.bin\"")), &format!("MKDIR \"{lit}\\with space\""));
    s = save(s, &format!("{lit}\\with space\\x y.bin"));
    s = yes_no(s, &format!("OS.FILE.readable(\"{lit}\\with space\\x y.bin\")"), true);
    s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(0).expect_fn("PRACTICE.SD()").reply_decimal(0);
    for gone in [
        format!("OS.FILE.readable(\"{lit}\\with space\\x y.bin\")"),
        format!("OS.DIR(\"{lit}\\with space\")"),
        format!("OS.DIR(\"{lit}\\a\\b\")"),
        format!("OS.DIR(\"{lit}\\a\")"),
        format!("OS.FILE.readable(\"{mac}\\one.bin\")"),
        format!("OS.DIR(\"{mac}\")"),
        format!("OS.FILE.readable(\"{lit}\\one.bin\")"),
        format!("OS.DIR(\"{lit}\")"),
    ] {
        s = yes_no(s, &gone, false);
    }
    for k in 0..4u64 {
        let addr = 0x0100_0000 + k * 1024;
        s = s.expect_req(Request::MemWrite { access: "VM".into(), addr, data: vec![0; 1024] }).reply_ok();
    }
    let mut session = connect(vec![Ok(reads(s))]);
    let mut env = test_env("probe-tempdir");
    let r = run(&mut env, &mut session, &[ProbeId::Tempdir.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    let text = |k: &str| o.lookup(k).cloned();
    assert_eq!(text(crate::outcome::key::ROOT), Some(Value::Text("literal".into())));
    assert_eq!(text(crate::outcome::key::EXISTS), Some(Value::Text("OS.FILE.readable".into())));
    assert_eq!(text("macro_fn"), Some(Value::Text("ok".into())));
    assert_eq!(env.learned.exists, Some(t32stage::ExistsFn::OsFileReadable));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
