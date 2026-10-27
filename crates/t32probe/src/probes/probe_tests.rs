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
fn a_function_refused_on_an_old_build_is_unsupported_not_failed() {
    let s = Script::new()
        .attach()
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

#[test]
fn files_below_the_stage_unwind_before_the_stage_directory() {
    let root = "C:\\T\\t32ctl";
    let mut s = Script::new().attach().expect_fn("OS.PTD()").reply_text("C:\\T");
    s = s.expect(t32rcl::testkit::Expect::CommandStarting("GLOBAL &t32ctl_snap".into())).reply_ok();
    s = s.expect_fn(&format!("OS.DIR(\"{root}\")")).reply_bool(true);
    s = s.expect_fn(&format!("OS.DIR(\"{root}\\probe-r1\")")).reply_bool(false);
    s = s.expect_cmd(&format!("MKDIR \"{root}\\probe-r1\"")).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.readable(\"{root}\\probe-r1\\x.bin\")")).reply_bool(true);
    s = s.expect_cmd(&format!("RM \"{root}\\probe-r1\\x.bin\"")).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.readable(\"{root}\\probe-r1\\x.bin\")")).reply_bool(false);
    s = s.expect_fn(&format!("OS.DIR(\"{root}\\probe-r1\")")).reply_bool(true);
    s = s.expect_cmd(&format!("RMDIR \"{root}\\probe-r1\"")).reply_ok();
    s = s.expect_fn(&format!("OS.DIR(\"{root}\\probe-r1\")")).reply_bool(false);
    for _ in 0..9 {
        s = s.expect(t32rcl::testkit::Expect::Any).reply_macro("");
    }
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
