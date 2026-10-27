// SPDX-License-Identifier: Apache-2.0
use std::{sync::Arc, time::Duration};

use t32rcl::{
    CallBudget, ConnectOpts, Endpoint, MacroName,
    testkit::{ConnectFailure, ManualClock, Request, Script, ScriptedConnector},
};
use t32stage::{Undo, VmWindow};

use super::*;
use crate::{
    cli::Gates,
    ctx::ProbeResult,
    env::{EnvConfig, WslFacts},
    outcome::Value,
    registry::Tier,
};

pub(crate) fn test_env(tag: &str) -> Env {
    let state_dir = std::env::temp_dir().join(format!("t32probe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
    Env::new(EnvConfig {
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
    })
}

pub(crate) fn connect(scripts: Vec<Result<Script, ConnectFailure>>) -> Session {
    let clock = ManualClock::new();
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

fn hangs(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.push(clear_rc())?;
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
    assert_eq!(verdict_of(Err("x".into()), Some(CancelReason::Interrupt)).0, Verdict::Interrupted);
}
