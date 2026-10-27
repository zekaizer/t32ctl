// SPDX-License-Identifier: Apache-2.0
use std::{sync::Arc, time::Duration};

use t32rcl::{
    CallBudget, ConnectOpts, Endpoint, MacroName,
    testkit::{ConnectFailure, Expect, ManualClock, Request, Script, ScriptedConnector},
};
use t32stage::{Macro, Undo, VmWindow};

use super::*;
use crate::{
    cli::Gates,
    ctx::ProbeResult,
    env::{EnvConfig, WslFacts},
    outcome::Value,
    registry::Tier,
};

/// An `Env` whose state directory is removed when the test ends.
pub(crate) struct TestEnv(Env);

impl std::ops::Deref for TestEnv {
    type Target = Env;
    fn deref(&self) -> &Env {
        &self.0
    }
}

impl std::ops::DerefMut for TestEnv {
    fn deref_mut(&mut self) -> &mut Env {
        &mut self.0
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0.cfg.state_dir);
    }
}

pub(crate) fn test_env(tag: &str) -> TestEnv {
    let state_dir = std::env::temp_dir().join(format!("t32probe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
    TestEnv(Env::new(EnvConfig {
        run_id: "r1".into(),
        hosts: vec!["pv".into()],
        port: 20000,
        prompt: None,
        idle: Duration::from_secs(1),
        scale: 1.0,
        vm_window: VmWindow::new(0x0100_0000, 0x0100_0000).unwrap(),
        state_dir,
        stage_root: None,
        gzip_max: 1 << 20,
        latency_n: 3,
        gates: Gates::default(),
        wsl: WslFacts::default(),
    }))
}

pub(crate) fn connect(scripts: Vec<Result<Script, ConnectFailure>>) -> Session {
    connect_on(&ManualClock::new(), scripts)
}

pub(crate) fn connect_on(clock: &ManualClock, scripts: Vec<Result<Script, ConnectFailure>>) -> Session {
    let clock = clock.clone();
    let mut c = ScriptedConnector::new(clock.clone());
    for s in scripts {
        c = c.push(s);
    }
    let opts = ConnectOpts {
        connector: Arc::new(c),
        clock: Arc::new(clock),
        budget: CallBudget::idle(Duration::from_secs(1)).with_total(Duration::from_secs(60)),
        ..ConnectOpts::default()
    };
    Session::connect(Endpoint::new("pv", 20000), opts).unwrap()
}

#[derive(Default)]
struct Seen(Vec<String>, usize);

impl Observer for Seen {
    fn starting(&mut self, def: &ProbeDef) {
        self.0.push(def.id.name().to_string());
    }
    fn finished(&mut self, _: &Env, outcomes: &[Outcome], _: &CleanupReport) {
        self.1 = outcomes.len();
    }
}

fn clear_rc() -> Undo {
    Undo::ClearMacros(vec![MacroName::new("t32ctl_rc").unwrap()])
}

fn cleared(s: Script) -> Script {
    s.expect_req(Request::MacroSet { name: "t32ctl_rc".into(), value: String::new() }).reply_macro("")
}

fn pass(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.headline("x", Value::Count(1));
    Ok(())
}

fn check_fails(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.push(clear_rc())?;
    ctx.headline("seen", Value::Count(7));
    Err(ProbeFail::Check("expected 1, got 2".into()))
}

fn panics(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.push(clear_rc())?;
    panic!("kaboom");
}

fn unsupported(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(ProbeFail::Unsupported("too old".into()))
}

fn calls_once(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.session.eval("TRUE()")?;
    Ok(())
}

fn hangs(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.push(clear_rc())?;
    ctx.session.cmd("WAIT 99.s")?;
    Ok(())
}

fn opens_a_stage(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.push(clear_rc())?;
    ctx.stage()?;
    Ok(())
}

fn snapshots_then_hangs(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    crate::probes::scratch_regions(ctx, &[16])?;
    ctx.session.cmd("WAIT 99.s")?;
    Ok(())
}

fn kills(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.session.cmd("X")?;
    Ok(())
}

fn interrupts(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.push(clear_rc())?;
    ctx.env.cancel.trigger();
    Ok(())
}

fn def(
    id: ProbeId,
    requires: &'static [ProbeId],
    secs: u64,
    run: fn(&mut ProbeCtx<'_>) -> ProbeResult,
) -> &'static ProbeDef {
    Box::leak(Box::new(ProbeDef {
        id,
        tier: Tier::Safe,
        requires,
        needs_connection: true,
        deadline: Duration::from_secs(secs),
        about: "",
        run,
    }))
}

pub(crate) fn run(env: &mut Env, session: &mut Session, defs: &[&'static ProbeDef]) -> RunResult {
    let mut seen = Seen::default();
    let r = Executor::new(env, session, &mut seen).run(defs);
    assert_eq!(seen.1, defs.len(), "the observer saw every outcome");
    r
}

#[test]
fn failures_and_panics_still_run_cleanup_and_dependents_are_skipped() {
    let s = cleared(cleared(Script::new().attach()));
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("exec-fail");
    let defs = [
        def(ProbeId::Version, &[], 10, check_fails),
        def(ProbeId::Latency, &[ProbeId::Version], 10, pass),
        def(ProbeId::Clients, &[], 10, panics),
        def(ProbeId::Keepalive, &[], 10, unsupported),
        def(ProbeId::Tempdir, &[], 10, pass),
    ];
    let r = run(&mut env, &mut session, &defs);
    let verdicts: Vec<_> = r.outcomes.iter().map(|o| o.verdict).collect();
    use Verdict::*;
    assert_eq!(verdicts, [Fail, Skipped, Fail, Unsupported, Pass]);
    assert_eq!(r.outcomes[0].lookup("seen"), Some(&Value::Count(7)), "measurements survive a failure");
    assert_eq!(r.outcomes[0].error.as_ref().map(|e| e.tag.as_str()), Some("check"));
    assert_eq!(r.outcomes[0].cleanup.len(), 1);
    assert!(matches!(&r.outcomes[1].skip, Some(SkipReason::Prereq { probe, .. }) if probe == "version"));
    assert_eq!(r.outcomes[2].error.as_ref().map(|e| e.message.as_str()), Some("panic: kaboom"));
    assert!(r.outcomes[2].cleanup.iter().all(|c| c.clean));
    assert_eq!(crate::outcome::exit_code(&r.outcomes, r.interrupted), 1);
    let journal = env.cfg.state_dir.join("journal");
    assert_eq!(std::fs::read_dir(&journal).map(|d| d.count()).unwrap_or(0), 0, "clean journals are removed");
}

#[test]
fn a_deadline_fails_the_probe_and_cleanup_runs_on_the_revived_connection() {
    let first = Script::new().attach().expect_cmd("WAIT 99.s").silence(Duration::from_secs(6));
    let second = cleared(Script::new().attach().expect_fn("TRUE()").reply_bool(true));
    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("exec-deadline");
    let r = run(&mut env, &mut session, &[def(ProbeId::Keepalive, &[], 5, hangs)]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Fail);
    assert_eq!(o.error.as_ref().map(|e| e.tag.as_str()), Some("deadline"));
    assert!(o.revive.as_ref().is_some_and(|v| v.ok));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
    assert_eq!(session.health(), Health::Live);
}

#[test]
fn probe_deadlines_run_on_the_session_clock() {
    let clock = ManualClock::new();
    let mut session = connect_on(&clock, vec![Ok(Script::new().attach().expect_fn("TRUE()").reply_bool(true))]);
    clock.advance(Duration::from_secs(3600));
    let mut env = test_env("exec-clock");
    let r = run(&mut env, &mut session, &[def(ProbeId::Version, &[], 5, calls_once)]);
    assert_eq!(r.outcomes[0].verdict, Verdict::Pass, "{:?}", r.outcomes[0]);
}

#[test]
fn a_probe_past_its_deadline_still_gets_its_vm_restored_and_verified() {
    let vm = |len| Request::MemRead { access: "VM".into(), addr: 0x0100_0000, len };
    let prior = [0x11; 16];
    let first = Script::new().attach().expect_req(vm(16)).reply_mem(&prior);
    let first = first.expect_cmd("WAIT 99.s").silence(Duration::from_secs(6));
    let second = Script::new().attach().expect_fn("TRUE()").reply_bool(true);
    let second = second
        .expect_req(Request::MemWrite { access: "VM".into(), addr: 0x0100_0000, data: prior.to_vec() })
        .reply_ok()
        .expect_req(vm(16))
        .reply_mem(&prior);
    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("exec-restore");
    let r = run(&mut env, &mut session, &[def(ProbeId::VmRw, &[], 5, snapshots_then_hangs)]);
    let o = &r.outcomes[0];
    assert_eq!(o.error.as_ref().map(|e| e.tag.as_str()), Some("deadline"));
    assert!(o.revive.as_ref().is_some_and(|v| v.ok), "{:?}", o.revive);
    let restore: Vec<_> = o.cleanup.iter().filter(|c| c.what.starts_with("restore VM:0x1000000")).collect();
    assert!(matches!(restore.as_slice(), [c] if c.clean && c.result == "restored"), "{:?}", o.cleanup);
    assert!(crate::report::render_line(o, crate::report::WIDTH).contains("restore=ok"));
}

#[test]
fn a_deadline_inside_stage_open_is_cleaned_up_on_the_revived_connection() {
    let first = Script::new()
        .attach()
        .expect_fn("OS.PTD()")
        .reply_text("C:\\T")
        .expect(Expect::CommandStarting("GLOBAL &t32ctl_snap".into()))
        .silence(Duration::from_secs(6));
    let mut second = Script::new().attach().expect_fn("TRUE()").reply_bool(true);
    for m in Macro::ALL.iter().filter(|m| **m != Macro::Root) {
        second = second.expect_req(Request::MacroSet { name: m.text().into(), value: String::new() }).reply_macro("");
    }
    let second = cleared(second).expect_fn("TRUE()").reply_bool(true);
    let mut session = connect(vec![Ok(first), Ok(second)]);
    let mut env = test_env("exec-stage-open");
    let defs = [def(ProbeId::Keepalive, &[], 5, opens_a_stage), def(ProbeId::Latency, &[], 5, calls_once)];
    let r = run(&mut env, &mut session, &defs);
    let o = &r.outcomes[0];
    assert_eq!(o.error.as_ref().map(|e| e.tag.as_str()), Some("deadline"));
    assert!(o.revive.as_ref().is_some_and(|v| v.ok), "{:?}", o.revive);
    assert!(o.cleanup.iter().any(|c| c.what.starts_with("clear macros t32ctl_snap")), "{:?}", o.cleanup);
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
    assert_eq!(r.outcomes[1].verdict, Verdict::Pass, "{:?}", r.outcomes[1]);
}

#[test]
fn a_lost_connection_is_revived_once_then_the_rest_is_skipped() {
    let first = Script::new().attach().expect_cmd("X").eof();
    let mut session = connect(vec![Ok(first), Err(ConnectFailure::Refused)]);
    let mut env = test_env("exec-lost");
    let defs = [
        def(ProbeId::Version, &[], 5, kills),
        def(ProbeId::Latency, &[], 5, pass),
        def(ProbeId::Clients, &[], 5, pass),
    ];
    let r = run(&mut env, &mut session, &defs);
    assert_eq!(r.outcomes[0].verdict, Verdict::Fail);
    assert!(r.outcomes[0].revive.as_ref().is_some_and(|v| !v.ok));
    for o in &r.outcomes[1..] {
        assert!(matches!(o.skip, Some(SkipReason::ConnectionLost { .. })), "{o:?}");
    }
    assert_eq!(crate::outcome::exit_code(&r.outcomes, r.interrupted), 1);
}

#[test]
fn ctrl_c_finishes_the_current_cleanup_and_marks_the_rest_interrupted() {
    let s = cleared(Script::new().attach());
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("exec-intr");
    let defs = [def(ProbeId::Version, &[], 5, interrupts), def(ProbeId::Latency, &[], 5, pass)];
    let r = run(&mut env, &mut session, &defs);
    assert_eq!(r.outcomes[0].verdict, Verdict::Interrupted);
    assert!(r.outcomes[0].cleanup.iter().all(|c| c.clean) && r.outcomes[0].cleanup.len() == 1);
    assert_eq!(r.outcomes[1].verdict, Verdict::Interrupted);
    assert!(r.interrupted);
    assert_eq!(crate::outcome::exit_code(&r.outcomes, r.interrupted), 130);
}

#[test]
fn verdict_table() {
    let (v, e, _) = verdict_of(Ok(Ok(())), Some(CancelReason::Deadline));
    assert_eq!((v, e.map(|e| e.tag)), (Verdict::Fail, Some("deadline".to_string())));
    let (v, _, s) = verdict_of(Ok(Err(ProbeFail::Skip(SkipReason::TargetNotUp { state: "down".into() }))), None);
    assert_eq!(v, Verdict::Skipped);
    assert!(matches!(s, Some(SkipReason::TargetNotUp { .. })));
    let busy: ProbeFail = StageError::Busy { depth: 2 }.into();
    assert!(matches!(
        verdict_of(Ok(Err(busy)), None),
        (Verdict::Skipped, None, Some(SkipReason::PowerViewBusy { depth: 2 }))
    ));
    let unwind: ProbeFail = StageError::UnwindFailed { depth: 1 }.into();
    assert!(matches!(verdict_of(Ok(Err(unwind)), None), (Verdict::Fail, Some(_), None)));
    assert_eq!(verdict_of(Err("x".into()), Some(CancelReason::Interrupt)).0, Verdict::Interrupted);
}
