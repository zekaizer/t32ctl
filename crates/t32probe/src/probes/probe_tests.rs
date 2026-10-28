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
    let mut s = Script::new().attach().expect_cmd("WAIT 10.s");
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
fn keepalive_waits_ten_seconds() {
    let s = Script::new().attach().expect_cmd("WAIT 10.s").silence(Duration::from_secs(12)).reply_ok();
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-keepalive-10s");
    let r = run(&mut env, &mut session, &[ProbeId::Keepalive.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("n"), Some(&Value::Count(0)));
    assert_eq!(o.lookup("interval"), Some(&Value::Text("none".into())));
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
    env.cfg.gates.run_control = Some(crate::cli::RunControlInput {
        symbol: Symbol::parse("main").unwrap(),
        loop_line: None,
        watch: None,
        scratch_ram: None,
    });
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
    let f = t32stage::StageRoot::from_ptd("C:\\T").unwrap().path(t32stage::RelPath::new("c.gz").unwrap());
    let load = "Data.LOAD.Binary \"C:\\T\\t32ctl\\c.gz\" VM:0x02000000 /UNZIP";
    let s = Script::new().attach().expect_cmd(load).reply_ok().expect_cmd(load).reply_error(90, "bad gzip");
    let mut session = connect(vec![Ok(s)]);
    assert!(unzip_accepted(&mut session, &f, 0x0200_0000).unwrap());
    assert!(!unzip_accepted(&mut session, &f, 0x0200_0000).unwrap());
    assert_eq!(corrupt_token(&["crc"], 4), "ACCEPTED(crc)");
    assert_eq!(corrupt_token(&[], 4), "rejected(4/4)");
}

#[test]
fn the_gzip_ceiling_unzips_under_a_data_budget() {
    use super::storage::unzip_timed;
    let f = t32stage::StageRoot::from_ptd("C:\\T").unwrap().path(t32stage::RelPath::new("z.gz").unwrap());
    let load = "Data.LOAD.Binary \"C:\\T\\t32ctl\\z.gz\" VM:0x02000000 /UNZIP";
    let s = Script::new().attach().expect_cmd(load).silence(Duration::from_secs(5)).reply_ok();
    let mut session = connect(vec![Ok(s.expect_cmd(load).reply_error(90, "out of memory"))]);
    assert_eq!(unzip_timed(&mut session, &f, 0x0200_0000, 4 << 20).unwrap(), (true, Duration::from_secs(5)));
    assert!(!unzip_timed(&mut session, &f, 0x0200_0000, 4 << 20).unwrap().0);
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
    s = macro_names_refused(s);
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
    let lit = "C:\\T\\t32ctl\\probe-r1-ptd";
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
    s = done(yes_no(s, "OS.DIR(\"C:\\T\\t32ctl\")", true), &format!("MKDIR \"{lit}\""));
    s = save(yes_no(s, &format!("OS.DIR(\"{lit}\")"), true), &format!("{lit}\\one.bin"));
    s = s.expect_fn(&format!("OS.FILE.SIZE(\"{lit}\\one.bin\")")).reply_decimal(1);
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
    assert_eq!(text(crate::outcome::key::ROOT), Some(Value::Text("ptd".into())));
    assert_eq!(text(crate::outcome::key::EXISTS), Some(Value::Text("OS.FILE.readable".into())));
    assert_eq!(env.learned.exists, Some(t32stage::ExistsFn::OsFileReadable));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

#[test]
fn tempdir_asks_for_a_stage_root_when_the_temp_dir_is_not_ascii_safe() {
    let s = Script::new().attach().expect_fn("OS.PTD()").reply_text("C:\\Users\\김\\AppData\\Local\\Temp");
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-tempdir-non-ascii");
    let r = run(&mut env, &mut session, &[ProbeId::Tempdir.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail);
    assert!(o.error.as_ref().is_some_and(|e| e.message.contains("--stage-root")), "{o:?}");
    assert_eq!(o.lookup("ptd"), Some(&Value::Text("non-ascii".into())));
}

fn set_req(name: &str, value: &str) -> Request {
    Request::MacroSet { name: name.into(), value: value.into() }
}

fn get_req(name: &str) -> Request {
    Request::MacroGet { name: name.into() }
}

/// The steps after the round trip on run `r1`, Bare learned: nothing crosses spellings, the empty
/// set works, `never` is refused, command-line assignment reaches both spellings, `;` cuts and
/// only 4096 B survive.
fn macro_names(mut s: Script) -> Script {
    let q = |x: &str| format!("t32ctl_qr1_{x}");
    let amp = |x: &str| format!("&t32ctl_qr1_{x}");
    s = s.expect_req(set_req(&q("x1"), "bare-r1")).reply_macro("");
    s = s.expect_req(get_req(&amp("x1"))).reply_exp_error(1, "unknown macro");
    s = s.expect_req(set_req(&amp("x2"), "amp-r1")).reply_macro("");
    s = s.expect_req(get_req(&q("x2"))).reply_exp_error(1, "unknown macro");
    s = s.expect_req(get_req(&amp("x2"))).reply_macro("amp-r1");
    s = s.expect_req(set_req(&q("z"), "")).reply_macro("").expect_req(get_req(&q("z"))).reply_macro("");
    s = s.expect_req(get_req(&q("never"))).reply_exp_error(7, "unknown macro");
    s = s.expect_req(get_req(&amp("never"))).reply_exp_error(7, "unknown macro");
    s = s.expect_cmd("GLOBAL &t32ctl_qr1_g").reply_ok();
    s = s.expect_req(get_req(&q("g"))).reply_macro("").expect_req(get_req(&amp("g"))).reply_macro("");
    s = s.expect_cmd("GLOBAL &t32ctl_qr1_x3").reply_ok().expect_cmd("&t32ctl_qr1_x3=\"cmd-r1\"").reply_ok();
    s = s.expect_req(get_req(&q("x3"))).reply_macro("cmd-r1").expect_req(get_req(&amp("x3"))).reply_macro("cmd-r1");
    s = s.expect_cmd("&t32ctl_qr1_x4=\"cmd4-r1\"").reply_ok().expect_req(get_req(&q("x4"))).reply_macro("");
    s = s.expect_cmd("GLOBAL &t32ctl_qr1_x5").reply_ok().expect_cmd("&t32ctl_qr1_x5=\"a;b\"").reply_ok();
    s = s.expect_req(get_req(&q("x5"))).reply_macro("a");
    for len in [4096usize, 4097, 8192] {
        let v: String = super::pattern(len as u64, len).iter().map(|b| char::from(b'a' + b % 26)).collect();
        s = s.expect_req(set_req(&q("big"), &v));
        s = if len == 4096 {
            s.reply_macro("").expect_req(get_req(&q("big"))).reply_macro(&v)
        } else {
            s.reply_exp_error(3, "too long")
        };
    }
    for x in ["x1", "x2", "x3", "x4", "x5", "z", "g", "big"] {
        s = s.expect_req(set_req(&q(x), "")).reply_macro("");
    }
    s
}

/// The steps after the round trip with every EXP call and command refused, Amp learned.
fn macro_names_refused(mut s: Script) -> Script {
    let exp =
        |s: Script, n: usize| (0..n).fold(s, |s, _| s.expect(t32rcl::testkit::Expect::Any).reply_exp_error(1, "no"));
    s = exp(s, 9);
    s = exp(s.expect_cmd("GLOBAL &t32ctl_qr1_g").reply_error(90, "no"), 2);
    s = s.expect_cmd("GLOBAL &t32ctl_qr1_x3").reply_error(90, "no");
    s = exp(s.expect_cmd("&t32ctl_qr1_x3=\"cmd-r1\"").reply_error(90, "no"), 2);
    s = exp(s.expect_cmd("&t32ctl_qr1_x4=\"cmd4-r1\"").reply_error(90, "no"), 1);
    s = s.expect_cmd("GLOBAL &t32ctl_qr1_x5").reply_error(90, "no");
    s = exp(s.expect_cmd("&t32ctl_qr1_x5=\"a;b\"").reply_error(90, "no"), 4);
    for x in ["x1", "x2", "x3", "x4", "x5", "z", "g", "big"] {
        s = s.expect_req(set_req(&format!("&t32ctl_qr1_{x}"), "")).reply_macro("");
    }
    s
}

#[test]
fn macro_cross_spelling_reads_split_when_neither_side_sees_the_other() {
    let mut s = Script::new().attach().expect_req(set_req("t32ctl_probe_a", "v1")).reply_macro("");
    s = s.expect_req(get_req("t32ctl_probe_a")).reply_macro("v1");
    s = s.expect_req(set_req("t32ctl_probe_a", "keep")).reply_macro("").expect_cmd("GLOBAL &t32ctl_probe_a").reply_ok();
    s = s.expect_req(get_req("t32ctl_probe_a")).reply_macro("keep");
    s = s.expect(t32rcl::testkit::Expect::Any).reply_macro("").expect_req(get_req("t32ctl_probe_b")).reply_macro("no");
    for _ in 0..14 {
        s = s.expect(t32rcl::testkit::Expect::Any).reply_exp_error(1, "refused");
    }
    s = macro_names(s);
    s = s
        .expect_req(set_req("t32ctl_probe_a", ""))
        .reply_macro("")
        .expect_req(set_req("t32ctl_probe_b", ""))
        .reply_macro("");
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("probe-macro-names");
    let r = run(&mut env, &mut session, &[ProbeId::Macro.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [
        ("spelling", "bare"),
        ("xspell", "split"),
        ("empty", "ok"),
        ("never", "err7"),
        ("cmdasgn", "both"),
        ("semi", "cut"),
        ("big", "refused"),
    ] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

/// Runs only the canary check after the bundle.
fn canary_after(ctx: &mut crate::ctx::ProbeCtx<'_>) -> crate::ctx::ProbeResult {
    super::scripts::canary(ctx, "after")
}

#[test]
fn bundle_fails_when_the_canary_appears() {
    let s = Script::new().attach().expect_fn("OS.DIR(\"\\pb-r1\")").reply_bool(false);
    let mut session = connect(vec![Ok(s.expect_fn("OS.DIR(\"pb-r1\")").reply_bool(true))]);
    let mut env = test_env("bundle-canary");
    let def: &'static crate::registry::ProbeDef = Box::leak(Box::new(crate::registry::ProbeDef {
        id: ProbeId::Bundle,
        tier: crate::registry::Tier::Safe,
        requires: &[],
        needs_connection: true,
        deadline: Duration::from_secs(5),
        about: "",
        run: canary_after,
    }));
    let r = run(&mut env, &mut session, &[def]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail);
    let msg = o.error.as_ref().map(|e| e.message.clone()).unwrap_or_default();
    assert!(msg.contains("outside the snapshot directory: pb-r1"), "{msg}");
    assert_eq!(o.lookup("canary"), Some(&Value::Text("FAIL".into())));
}

#[test]
fn bundle_skips_unless_all_three_gates_hold() {
    use crate::probes::{
        channels::channels_tests::ran,
        lab::lab_tests::{closed_down, files_removed, listed, opened},
        scripts::{probe_bundle, sn_case},
    };
    let skipped = |env: &mut crate::exec::tests::TestEnv, s: Script| {
        let mut session = connect(vec![Ok(s)]);
        let r = run(env, &mut session, &[ProbeId::Bundle.def()]);
        let o = r.outcomes.into_iter().next().unwrap();
        assert_eq!(o.verdict, Verdict::Skipped, "{o:?}");
        assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
        o
    };
    let mut env = test_env("bundle-gates");
    let o = skipped(&mut env, Script::new().attach());
    assert_eq!(o.lookup("gate"), Some(&Value::Text("skip:a".into())));
    env.learned.exp_to_script = Some(true);
    let o = skipped(&mut env, Script::new().attach());
    assert_eq!(o.lookup("gate"), Some(&Value::Text("skip:b".into())));

    env.learned.abspath_same = Some(true);
    let snap = format!("{STAGE}\\{}", probe_bundle("r1").unwrap().key().hex());
    let case = sn_case(&snap);
    let mut s = Script::new().attach().expect_fn("OS.DIR(\"\\pb-r1\")").reply_bool(false);
    s = stage_opened(s.expect_fn("OS.DIR(\"pb-r1\")").reply_bool(false));
    s = s.expect_req(set_req("t32ctl_snap", &snap)).reply_macro("");
    s = listed(ran(opened(s, "bun"), "bun", &case, &[], None), "bun", &[]);
    s = stage_closed(s);
    s = files_removed(s, "bun", &case, crate::env::ScriptOut::Off, &["sn.cmm", "sn-a.bin", "sn-z.bin"]);
    s = closed_down(s, "bun");
    for m in ["t32ctl_probe_main", "t32ctl_probe_deep"] {
        s = s.expect_req(set_req(m, "")).reply_macro("");
    }
    let o = skipped(&mut env, s);
    assert_eq!(o.lookup("gate"), Some(&Value::Text("skip:c".into())));
    assert_eq!(o.lookup("sn"), Some(&Value::Text("snap=n empty=n".into())));
}

/// `{q}_<x>` suffixes a probe creates, the ones it relies on nobody creating, and the cases
/// whose bodies name them.
struct Suffixes {
    probe: &'static str,
    created: Vec<&'static str>,
    absent: Vec<&'static str>,
    bodies: Vec<String>,
}

fn bodies(cases: Vec<crate::probes::lab::Case>, subs: &[crate::probes::channels::Sub]) -> Vec<String> {
    cases.into_iter().map(|c| c.body).chain(subs.iter().map(|(_, _, b)| b.to_string())).collect()
}

fn named_suffixes(text: &str) -> Vec<String> {
    text.match_indices("{q}_")
        .map(|(i, m)| {
            text[i + m.len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>()
        })
        .collect()
}

fn suffix_table() -> Vec<Suffixes> {
    use crate::probes::{channels, macros, scriptlab as sl};
    let (vis, vis_subs) = macros::vis_cases();
    let (ret, ret_subs) = macros::ret_cases();
    let (cin, cin_subs) = channels::in_cases("C:\\T\\t32ctl\\probe-r1-in");
    let (cout, cout_subs) = channels::out_cases();
    let lab = vec![sl::o0(), sl::c1(), sl::c2(), sl::w300(1), sl::nest(1), sl::w3(), sl::looped(), sl::big()];
    let halt =
        vec![sl::hs_stop(), sl::hs_err(), sl::hs_wait(), sl::hs_unwind(), sl::hs_save(), sl::hs_open(), sl::hs_glob()];
    let mut ret_created = vec!["s1", "d1", "d2", "d3", "glo", "h1", "h2", "mk", "mk2", "n1", "n2", "n3"];
    ret_created.extend(["p", "p2", "pp", "p1", "r1", "loc", "lo"]);
    vec![
        Suffixes {
            probe: "macro",
            created: vec!["x1", "x2", "x3", "x4", "x5", "z", "g", "big"],
            absent: vec!["never"],
            bodies: Vec::new(),
        },
        Suffixes { probe: "fn-table", created: vec!["fx", "fy"], absent: Vec::new(), bodies: Vec::new() },
        Suffixes { probe: "script-lab", created: vec![sl::RCL_MACRO], absent: Vec::new(), bodies: bodies(lab, &[]) },
        Suffixes {
            probe: "script-halt",
            created: vec!["hg"],
            absent: Vec::new(),
            bodies: bodies([halt, vec![sl::hs_read()]].concat(), &[]),
        },
        Suffixes {
            probe: "channel-in",
            created: vec!["p_addr", "ca"],
            absent: Vec::new(),
            bodies: bodies(cin, &cin_subs),
        },
        Suffixes { probe: "channel-out", created: Vec::new(), absent: Vec::new(), bodies: bodies(cout, &cout_subs) },
        Suffixes {
            probe: "macro-visibility",
            created: vec!["v1", "v2", "v3", "v4", "c1", "c2", "e1", "im"],
            absent: vec!["u1", "none"],
            bodies: bodies(vis, &vis_subs),
        },
        Suffixes { probe: "macro-return", created: ret_created, absent: vec!["never"], bodies: bodies(ret, &ret_subs) },
    ]
}

#[test]
fn macro_suffixes_never_collide_across_probes() {
    let table = suffix_table();
    for t in &table {
        for b in &t.bodies {
            for x in named_suffixes(b) {
                assert!(
                    t.created.contains(&x.as_str()) || t.absent.contains(&x.as_str()),
                    "{}: a case names {{q}}_{x}, which its table entry lacks",
                    t.probe
                );
            }
        }
    }
    for a in &table {
        for b in &table {
            if a.probe == b.probe {
                continue;
            }
            for x in &a.created {
                assert!(
                    !b.created.contains(x) && !b.absent.contains(x),
                    "{} creates {{q}}_{x}, which {} also uses",
                    a.probe,
                    b.probe
                );
            }
        }
    }
}
