// SPDX-License-Identifier: Apache-2.0
//! Whole calls in-process against scripted PowerView conversations.

use std::path::Path;

use serde_json::Value;
use t32rcl::{
    TargetState,
    testkit::{ConnectFailure, ManualClock, Request, Script, ScriptedConnector},
};

use super::*;
use crate::cli::tests::words;

/// The board profile every world starts with.
pub(crate) const PROFILE: &str =
    "schema = 1\n[target]\nname = \"acme_evb\"\ncpu = \"CortexA53\"\ncores = 4\nword = 8\n";

/// A home and a workspace in a fresh temp directory, removed when dropped. `T32CTL_PROFILE`
/// names `board/support/t32ctl`, which holds `PROFILE`.
pub(crate) struct World(pub(crate) PathBuf);

impl World {
    pub(crate) fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("t32ctl-run-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ws")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let w = Self(root);
        w.write("ws/board/support/t32ctl/profile.toml", PROFILE);
        w
    }

    pub(crate) fn env(&self) -> Env {
        Env {
            cwd: self.0.join("ws"),
            home: Some(self.0.join("home")),
            profile: Some("board/support/t32ctl".into()),
            auto_hosts: vec!["127.0.0.1".into()],
            ..Env::default()
        }
    }

    pub(crate) fn pv_dir(&self) -> PathBuf {
        self.0.join("home/.local/state/t32ctl/pv/auto-20000")
    }

    pub(crate) fn write(&self, rel: &str, text: &str) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn deps(env: Env, connects: Vec<Result<Script, ConnectFailure>>) -> Deps {
    let clock = ManualClock::new();
    let connector = connects.into_iter().fold(ScriptedConnector::new(clock.clone()), |c, s| c.push(s));
    let poll = PollSchedule { initial: Duration::ZERO, max: Duration::ZERO, start_grace: Duration::ZERO };
    let stage_token = Some(t32stage::testkit::TOKEN.to_string());
    Deps { env, connector: Arc::new(connector), clock: Arc::new(clock), cancel: Cancel::new(), poll, stage_token }
}

pub(crate) fn call(line: &str, deps: &Deps) -> Output {
    let argv: Vec<OsString> = words(line).into_iter().map(OsString::from).collect();
    run(&argv, deps)
}

/// Attach, then the prelude: `T32_GetState`, and `SYStem.CPU()` unless the target is down.
pub(crate) fn connected(state: u8) -> Script {
    connected_to(state, "CortexA53")
}

pub(crate) fn connected_to(state: u8, cpu: &str) -> Script {
    let s = Script::new().attach().expect_req(Request::TargetState).reply_state(state);
    if state == 0 { s } else { s.expect_fn("SYStem.CPU()").reply_text(cpu) }
}

/// `read status` reads the cores before the `pv` line.
fn cores(s: Script) -> Script {
    s.expect_fn("CORE.NUMBER()").reply_decimal(4)
}

/// The two lines `read status` prints for `World`'s profile and a matching CPU.
const TARGET: &str = "target  acme_evb cpu=CortexA53 (matches profile) cores=4\nprofile board/support/t32ctl/\n";

/// The `pv` line's requests. `clients` `None`: PowerView refuses both spellings.
fn pv(s: Script, clients: Option<i64>, practice: u8, depth: i64) -> Script {
    let s = s
        .expect_fn("SOFTWARE.BUILD()")
        .reply_decimal(168_999)
        .expect_fn("SOFTWARE.BUILD.BASE()")
        .reply_decimal(166_414);
    let s = match clients {
        Some(n) => s.expect_fn("RCL.TCP.NrUsedCons()").reply_decimal(n),
        None => s
            .expect_fn("RCL.TCP.NrUsedCons()")
            .reply_function(0, "unknown function")
            .expect_fn("api.RCL.TCP.NrUsedCons()")
            .reply_function(0, "unknown function"),
    };
    s.expect_req(Request::PracticeState).reply_state(practice).expect_fn("PRACTICE.StackDepth()").reply_decimal(depth)
}

/// The header of a stopped target: `T32_GetState`, `CORE()`, `Register(PC)`.
pub(crate) fn stopped_header(s: Script) -> Script {
    s.expect_req(Request::TargetState)
        .reply_state(2)
        .expect_fn("CORE()")
        .reply_decimal(0)
        .expect_fn("Register(PC)")
        .reply_function(4, "0x40083E10")
}

#[test]
fn read_status_prints_the_header_after_the_pv_line_it_read_first() {
    let w = World::new("stopped");
    let d = deps(w.env(), vec![Ok(stopped_header(pv(cores(connected(2)), Some(1), 0, 0)))]);
    let out = call("t32ctl read status", &d);
    assert_eq!(
        out.text,
        format!("stopped core=0 pc=0x40083e10\n{TARGET}pv      build=168999 clients=1 practice=idle\n")
    );
    assert_eq!(out.code, 0);
    assert!(w.pv_dir().is_dir(), "the state directory exists before the call connects");
}

#[test]
fn a_running_target_shows_its_cores_and_a_down_one_only_down() {
    let w = World::new("running");
    let running = pv(cores(connected(3)), Some(1), 0, 0).expect_req(Request::TargetState).reply_state(3);
    let d = deps(w.env(), vec![Ok(running.expect_fn("CORE.NUMBER()").reply_decimal(4))]);
    assert_eq!(call("t32ctl read status", &d).text.lines().next(), Some("running cores=0-3"));
    let down = cores(connected(0).expect_fn("SYStem.CPU()").reply_text("CortexA53"));
    let down = pv(down, Some(1), 0, 0).expect_req(Request::TargetState).reply_state(0);
    let d = deps(w.env(), vec![Ok(down)]);
    let out = call("t32ctl read status", &d);
    assert_eq!((out.text.lines().next(), out.code), (Some("down"), 0));
    assert_eq!(out.text.lines().nth(1), Some("target  acme_evb cpu=CortexA53 (not checked while down) cores=4"));
}

#[test]
fn a_refused_pc_shortens_the_header_and_never_fails_the_call() {
    let w = World::new("nopc");
    let s = pv(cores(connected(2)), Some(1), 0, 0).expect_req(Request::TargetState).reply_state(2);
    let s = s.expect_fn("CORE()").reply_decimal(1).expect_fn("Register(PC)").reply_function(0, "target system down");
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(s)]));
    assert_eq!((out.text.lines().next(), out.code), (Some("stopped core=1"), 0));
}

#[test]
fn clients_fall_back_to_the_api_spelling_then_print_a_question_mark() {
    let w = World::new("clients");
    let s = cores(connected(2))
        .expect_fn("SOFTWARE.BUILD()")
        .reply_decimal(172_803)
        .expect_fn("SOFTWARE.BUILD.BASE()")
        .reply_decimal(172_138)
        .expect_fn("RCL.TCP.NrUsedCons()")
        .reply_function(0, "unknown function")
        .expect_fn("api.RCL.TCP.NrUsedCons()")
        .reply_decimal(2)
        .expect_req(Request::PracticeState)
        .reply_state(0)
        .expect_fn("PRACTICE.StackDepth()")
        .reply_decimal(0);
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(out.text.lines().nth(3), Some("pv      build=172803 clients=2 practice=idle"));
    let s = stopped_header(pv(cores(connected(2)), None, 0, 0));
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(out.text.lines().nth(3), Some("pv      build=168999 clients=? practice=idle"));
}

#[test]
fn an_idle_practice_with_a_non_empty_stack_is_a_halted_script() {
    let w = World::new("halted");
    let out =
        call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(pv(cores(connected(2)), Some(1), 0, 1)))]));
    assert_eq!(out.text.lines().nth(3), Some("pv      build=168999 clients=1 practice=halted (depth 1)"));
    let out =
        call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(pv(cores(connected(2)), Some(1), 2, 1)))]));
    assert_eq!(out.text.lines().nth(3), Some("pv      build=168999 clients=1 practice=dialog (depth 1)"));
}

#[test]
fn full_status_adds_the_binding_the_config_and_the_state_directory() {
    let w = World::new("full");
    let board = w.0.join("ws/boards/acme/acme_evb");
    w.write("ws/build/CMakeCache.txt", &format!("BOARD_DIR:PATH={}\nBOARD:STRING=acme_evb/a53\n", board.display()));
    w.write("ws/boards/acme/acme_evb/support/t32ctl/profile.toml", PROFILE);
    w.write("home/.config/t32ctl/config.toml", "schema = 1\n[connection]\nport = 20000\n");
    let env = Env { auto_hosts: vec!["127.0.0.1".into(), "172.29.32.1".into()], profile: None, ..w.env() };
    let d = deps(env, vec![Err(ConnectFailure::Refused), Ok(stopped_header(pv(cores(connected(2)), Some(1), 0, 0)))]);
    let out = call("t32ctl read status --full", &d);
    let hash = profile::hash(&profile::walk(&w.0.join("ws/boards/acme/acme_evb/support/t32ctl")).unwrap());
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             target  acme_evb cpu=CortexA53 (matches profile) cores=4\n\
             profile boards/acme/acme_evb/support/t32ctl/ profile={} (1 file)\n\
             binding build=build (auto-detection: ./build/CMakeCache.txt), profile from BOARD_DIR, \
             variant base (BOARD=acme_evb/a53)\n\
             config  ~/.config/t32ctl/config.toml: host=auto (172.29.32.1) port=20000\n\
             state   ~/.local/state/t32ctl/pv/auto-20000/\n\
             pv      build=168999 clients=1 practice=idle\n",
            hash.short()
        )
    );
    let d = deps(w.env(), vec![Ok(stopped_header(pv(cores(connected(2)), Some(1), 0, 0)))]);
    let out = call("t32ctl read status --full --build elsewhere", &d);
    let binding = "binding build=elsewhere (--build), profile from T32CTL_PROFILE, variant base (no BOARD)\n";
    assert!(out.text.contains(binding), "{}", out.text);
}

#[test]
fn json_renders_the_same_report() {
    let w = World::new("json");
    let out = call(
        "t32ctl read status --json",
        &deps(w.env(), vec![Ok(stopped_header(pv(cores(connected(2)), Some(1), 0, 0)))]),
    );
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["schema"], "t32ctl.report/1");
    assert_eq!(v["command"], "read status");
    assert_eq!(v["exit"], serde_json::json!({"code": 0, "class": "ok"}));
    assert_eq!(v["header"], serde_json::json!({"run": "stopped", "core": 0, "at": {"pc": "0x40083e10"}}));
    assert_eq!(v["body"]["kind"], "status");
    assert_eq!(v["body"]["pv"]["clients"], 1);
    assert_eq!(v["body"]["target"]["cpu_check"], "match");
    assert_eq!(v["body"]["profile"], serde_json::json!({"dir": "board/support/t32ctl/"}));
    assert!(v["body"].get("binding").is_none(), "--full members are absent without --full");
    assert_eq!(out.text.lines().count(), 1);
}

#[test]
fn an_interrupt_before_connecting_sends_nothing_and_exits_130() {
    let w = World::new("interrupt");
    let d = deps(w.env(), vec![]);
    d.cancel.trigger();
    let out = call("t32ctl read status", &d);
    assert_eq!(out.text, "error[interrupted]: interrupted before connecting; nothing was sent\n");
    assert_eq!(out.code, 130);
}

#[test]
fn every_candidate_refused_is_unreachable_with_what_was_tried() {
    let w = World::new("unreachable");
    let env = Env { auto_hosts: vec!["127.0.0.1".into(), "172.29.32.1".into()], ..w.env() };
    let d = deps(env, vec![Err(ConnectFailure::Refused), Err(ConnectFailure::Refused)]);
    let out = call("t32ctl read status", &d);
    assert_eq!(
        out.text,
        "error[unreachable]: 127.0.0.1:20000: connection refused\n\
         \x20 PowerView not running, RCL not enabled (RCL=NETTCP in config.t32), or wrong [connection] port in \
         ~/.config/t32ctl/config.toml\n\
         \x20 tried 127.0.0.1:20000 (refused), 172.29.32.1:20000 (refused)\n"
    );
    assert_eq!(out.code, 5);
}

#[test]
fn journals_a_dead_call_left_are_replayed_and_reported_as_notices() {
    let w = World::new("replay");
    w.write(
        "home/.local/state/t32ctl/pv/auto-20000/journal/t32ctl-1-1.undo",
        "rm-file\tLC:\\T\\t32ctl\tk/a.bin\nunwind\n",
    );
    let s = Script::new().attach().expect_req(Request::PracticeState).reply_state(0);
    let s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    let s = s.expect_fn("OS.FILE.readable(\"C:\\T\\t32ctl\\k\\a.bin\")").reply_bool(false);
    let s = s.expect_req(Request::TargetState).reply_state(2).expect_fn("SYStem.CPU()").reply_text("CortexA53");
    let s = pv(cores(s), Some(1), 0, 0);
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             ! cleaned up after an interrupted call: 2 steps\n\
             {TARGET}pv      build=168999 clients=1 practice=idle\n"
        )
    );
    assert_eq!(out.code, 0);
    assert!(!w.pv_dir().join("journal/t32ctl-1-1.undo").exists());
}

#[test]
fn a_journal_whose_script_may_still_run_is_kept_and_no_notice_says_end() {
    let w = World::new("replay-deferred");
    let journal = "rm-file\tLC:\\T\\t32ctl\tk/a.bin\nunwind\n";
    w.write("home/.local/state/t32ctl/pv/auto-20000/journal/t32ctl-1-1.undo", journal);
    let s = Script::new().attach().expect_req(Request::PracticeState).reply_state(1);
    let s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    let s = s.expect_req(Request::TargetState).reply_state(2).expect_fn("SYStem.CPU()").reply_text("CortexA53");
    let s = pv(cores(s), Some(1), 1, 1);
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             ! cleanup left by an earlier run: deferred: its script may still be running (PRACTICE Running, \
             stack depth 1); a later call cleans up after it\n\
             {TARGET}pv      build=168999 clients=1 practice=running (depth 1)\n"
        )
    );
    assert_eq!(out.code, 0);
    let kept = std::fs::read_to_string(w.pv_dir().join("journal/t32ctl-1-1.undo")).unwrap();
    assert_eq!(kept, journal);
}

#[test]
fn a_residue_left_for_a_human_keeps_its_condition_in_the_notice() {
    let w = World::new("replay-break");
    w.write("home/.local/state/t32ctl/pv/auto-20000/journal/t32ctl-1-1.undo", "break\n");
    let s = Script::new().attach();
    let s = s.expect_req(Request::TargetState).reply_state(2).expect_fn("SYStem.CPU()").reply_text("CortexA53");
    let s = pv(cores(s), Some(1), 0, 0);
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             ! residue: break target (left by an earlier run): only if t32ctl left the target running; \
             in PowerView type: Break\n\
             {TARGET}pv      build=168999 clients=1 practice=idle\n"
        )
    );
    assert_eq!(out.code, 0);
}

#[test]
fn a_refused_read_is_exit_2_and_the_header_still_prints() {
    let w = World::new("refused");
    let s = cores(connected(2))
        .expect_fn("SOFTWARE.BUILD()")
        .reply_decimal(168_999)
        .expect_fn("SOFTWARE.BUILD.BASE()")
        .reply_decimal(166_414)
        .expect_fn("RCL.TCP.NrUsedCons()")
        .reply_decimal(1)
        .expect_req(Request::PracticeState)
        .reply_state(0)
        .expect_fn("PRACTICE.StackDepth()")
        .reply_function(0, "syntax error");
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         error[refused]: PRACTICE.StackDepth(): function: PowerView refused: function failed (code 0): syntax error\n"
    );
    assert_eq!(out.code, 2);
}

#[test]
fn a_broken_config_stops_the_call_before_it_connects() {
    let w = World::new("badconfig");
    w.write("home/.config/t32ctl/config.toml", "schema = 1\n[lease]\nidle = \"10m\"\n");
    let out = call("t32ctl read status --json", &deps(w.env(), vec![]));
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["error"]["kind"], "profile-invalid");
    let file = w.0.join("home/.config/t32ctl/config.toml");
    assert_eq!(v["error"]["message"], format!("{}:2:1: [lease] is planned, not in this t32ctl", file.display()));
    assert_eq!((v["header"].clone(), out.code), (Value::Null, 5));
}

#[test]
fn a_usage_error_renders_json_when_json_follows_the_grade_word() {
    let w = World::new("usage");
    let d = deps(w.env(), vec![]);
    let out = call("t32ctl read stat --json", &d);
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!((v["command"].as_str(), v["error"]["kind"].as_str(), out.code), (Some("read stat"), Some("usage"), 2));
    let out = call("t32ctl --json read status", &d);
    assert!(out.text.starts_with("error[usage]: unexpected argument '--json' found\n"), "{}", out.text);
    assert_eq!(out.code, 2);
}

#[test]
fn describe_is_a_document_that_never_connects() {
    let w = World::new("describe");
    let d = deps(Env::default(), vec![]);
    let out = call("t32ctl read describe --json", &d);
    assert_eq!((out.text, out.code), (describe::json(), 0));
    assert_eq!(call("t32ctl read describe --format permissions", &d).text, describe::permissions());
    assert_eq!(call("t32ctl read describe --format skill", &d).text, describe::skill());
    assert!(!Path::new(&w.0).join("home/.local").exists());
}

#[test]
fn a_call_without_a_profile_is_no_profile_and_connects_nowhere() {
    let w = World::new("noprofile");
    let out = call("t32ctl read status", &deps(Env { profile: None, ..w.env() }, vec![]));
    assert_eq!(
        out.text,
        "error[no-profile]: no target profile for this build\n\
         \x20 T32CTL_PROFILE: unset\n\
         \x20 .t32ctl.toml: not in a git repository\n\
         \x20 BOARD_DIR: no build directory (--build, T32CTL_BUILD, .t32ctl.toml, ./build)\n"
    );
    assert_eq!(out.code, 5);
    assert!(!w.pv_dir().exists(), "nothing is created before the profile loads");
}

#[test]
fn a_broken_profile_stops_the_call_before_it_connects() {
    let w = World::new("badprofile");
    w.write(
        "ws/board/support/t32ctl/profile.toml",
        "schema = 1\n[target]\nname = \"x\"\ncpu = \"y\"\ncores = 0\nword = 8\n",
    );
    let out = call("t32ctl read status", &deps(w.env(), vec![]));
    assert_eq!(
        out.text,
        "error[profile-invalid]: board/support/t32ctl/profile.toml:5:9: target.cores = 0 is outside 1..=4294967295\n"
    );
    assert_eq!(out.code, 5);
}

#[test]
fn read_status_reports_a_cpu_mismatch_as_a_notice_and_exits_0() {
    let w = World::new("mismatch");
    let s = stopped_header(pv(cores(connected_to(2, "CortexA72")), Some(1), 0, 0));
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         ! connected CPU CortexA72 is not the profile's CortexA53: every other command refuses\n\
         target  acme_evb cpu=CortexA72 (profile: CortexA53, mismatch) cores=4\n\
         profile board/support/t32ctl/\n\
         pv      build=168999 clients=1 practice=idle\n"
    );
    assert_eq!(out.code, 0);
    let s = stopped_header(pv(cores(connected_to(2, "cortexa53")), Some(1), 0, 0));
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(out.text.lines().nth(1), Some("target  acme_evb cpu=cortexa53 (matches profile) cores=4"));
}

#[test]
fn the_prelude_refuses_a_cpu_that_is_not_the_profiles_and_skips_the_check_while_down() {
    let w = World::new("prelude");
    let env = w.env();
    let setup = Setup::resolve(&env, &Globals { json: false, build: None }).unwrap();
    let conn = |script: Script| {
        let d = deps(w.env(), vec![Ok(script)]);
        let opts =
            ConnectOpts { connector: Arc::clone(&d.connector), clock: Arc::clone(&d.clock), ..ConnectOpts::default() };
        Conn::connect(&["127.0.0.1".to_string()], 20000, &opts).unwrap()
    };
    let mut c = conn(connected_to(2, "CortexA72"));
    let e = c.prelude(&setup.pv_dir, &setup.profile, CpuCheck::Refuse).unwrap_err();
    assert_eq!(e.kind, ErrorKind::CpuMismatch);
    assert_eq!(e.message, "connected CPU CortexA72 is not the profile's CortexA53");
    assert_eq!(e.exit().code(), 5);
    let mut c = conn(connected(0));
    assert_eq!(c.prelude(&setup.pv_dir, &setup.profile, CpuCheck::Refuse), Ok(TargetState::Down));
}

/// One raw request after the prelude of a stopped target, then the header.
fn raw(request: &str, reply: impl FnOnce(Script) -> Script) -> Script {
    stopped_header(reply(connected(2).expect_fn(request)))
}

#[test]
fn raw_eval_prints_the_value_in_argument_notation_and_its_type() {
    let w = World::new("eval");
    type Reply = fn(Script) -> Script;
    let cases: [(Reply, &str, &str); 8] = [
        (|s| s.reply_bool(true), "TRUE() (bool)", r#"{"kind":"eval","type":"bool","value":"TRUE()"}"#),
        (|s| s.reply_decimal(-5), "-5 (decimal)", r#"{"kind":"eval","type":"decimal","value":"-5"}"#),
        (|s| s.reply_function(4, "0x1F"), "0x1f (hex)", r#"{"kind":"eval","type":"hex","value":"0x1f"}"#),
        (|s| s.reply_function(0x10, "2.5"), "2.5 (float)", r#"{"kind":"eval","type":"float","value":"2.5"}"#),
        (|s| s.reply_function(0x400, "1.5s"), "1.5s (time)", r#"{"kind":"eval","type":"time","value":"1.5s"}"#),
        (
            |s| s.reply_text("C:\\Users\\op\\AppData\\Local\\Temp"),
            "\"C:\\Users\\op\\AppData\\Local\\Temp\" (string)",
            r#"{"kind":"eval","type":"string","value":"C:\\Users\\op\\AppData\\Local\\Temp"}"#,
        ),
        (|s| s.reply_function(0x8000, ""), "(empty)", r#"{"kind":"eval","type":"empty","value":""}"#),
        (|s| s.reply_function(0x9999, "x"), "x (type 0x9999)", r#"{"kind":"eval","type":"type 0x9999","value":"x"}"#),
    ];
    for (reply, text, json) in cases {
        let out = call("t32ctl raw eval 'OS.PTD()'", &deps(w.env(), vec![Ok(raw("OS.PTD()", reply))]));
        assert_eq!((out.text, out.code), (format!("stopped core=0 pc=0x40083e10\n{text}\n"), 0));
        let out = call("t32ctl raw eval 'OS.PTD()' --json", &deps(w.env(), vec![Ok(raw("OS.PTD()", reply))]));
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!((v["command"].as_str(), v["body"].to_string()), (Some("raw eval"), json.to_string()));
    }
}

#[test]
fn raw_text_is_sent_literally_and_a_refusal_is_exit_2_with_powerviews_message() {
    let w = World::new("evalrefused");
    let s = raw("OS.FILE.SIZE(\"&p\")", |s| s.reply_function(0, "file not found"));
    let out = call("t32ctl raw eval 'OS.FILE.SIZE(\"&p\")'", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         error[refused]: OS.FILE.SIZE(\"&p\"): function: PowerView refused: function failed (code 0): file not found\n"
    );
    assert_eq!(out.code, 2);
}

#[test]
fn a_down_target_still_gets_raw_text_and_a_target_state_code_is_exit_3() {
    let w = World::new("evaldown");
    let s = connected(0).expect_fn("Register(PC)").reply_error(3, "");
    let s = s.expect_req(Request::TargetState).reply_state(0);
    let out = call("t32ctl raw eval 'Register(PC)'", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        out.text,
        "down\nerror[target-down]: Register(PC): function: PowerView refused: target state (code 3 STD_NOTRUNNING)\n"
    );
    assert_eq!(out.code, 3);
    let s = connected(3).expect_cmd("Step").reply_error(2, "");
    let s = s.expect_req(Request::TargetState).reply_state(3).expect_fn("CORE.NUMBER()").reply_decimal(4);
    let out = call("t32ctl raw cmd Step", &deps(w.env(), vec![Ok(s)]));
    assert_eq!((out.text.lines().nth(1).map(|l| l.starts_with("error[not-halted]: Step:")), out.code), (Some(true), 3));
}

#[test]
fn a_cpu_mismatch_refuses_raw_eval_before_it_sends_the_text() {
    let w = World::new("evalmismatch");
    let s = stopped_header(connected_to(2, "CortexA72"));
    let out = call("t32ctl raw eval 'OS.PTD()'", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\nerror[cpu-mismatch]: connected CPU CortexA72 is not the profile's CortexA53\n"
    );
    assert_eq!(out.code, 5);
}

#[test]
fn text_above_2037_bytes_is_notation_and_connects_nowhere() {
    let w = World::new("evallong");
    let out = call(&format!("t32ctl raw cmd {}", "A".repeat(2038)), &deps(w.env(), vec![]));
    assert_eq!(out.text, "error[notation]: PRACTICE text is 2038 bytes; it must be 1 to 2037 bytes\n");
    assert_eq!(out.code, 2);
}

/// `raw cmd`'s command, then the PRACTICE state and stack it reads after it.
fn raw_cmd(text: &str, practice: u8, depth: i64) -> Script {
    let s = connected(2).expect_cmd(text).reply_ok().expect_req(Request::PracticeState).reply_state(practice);
    stopped_header(s.expect_fn("PRACTICE.StackDepth()").reply_decimal(depth))
}

#[test]
fn raw_cmd_reports_done_and_a_script_it_left_on_the_stack() {
    let w = World::new("cmd");
    let out = call("t32ctl raw cmd 'PRINT \"hello\"'", &deps(w.env(), vec![Ok(raw_cmd("PRINT \"hello\"", 0, 0))]));
    assert_eq!((out.text, out.code), ("stopped core=0 pc=0x40083e10\ncmd: done\n".to_string(), 0));
    let d = deps(w.env(), vec![Ok(raw_cmd("DO C:\\t\\x.cmm", 1, 1))]);
    let out = call("t32ctl raw cmd 'DO C:\\t\\x.cmm'", &d);
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         ! a PRACTICE script is on the stack after this command (depth 1, running); proc and raw script calls \
         wait for it\n\
         cmd: done\n"
    );
    assert_eq!(out.code, 0);
    let d = deps(w.env(), vec![Ok(raw_cmd("DO C:\\t\\x.cmm", 0, 2))]);
    let out = call("t32ctl raw cmd 'DO C:\\t\\x.cmm' --json", &d);
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["body"], serde_json::json!({"kind": "cmd", "practice": "halted", "depth": 2}));
    assert_eq!(v["notices"][0]["text"].as_str().map(|t| t.contains("(depth 2, halted)")), Some(true));
}

#[test]
fn a_command_that_blocks_past_the_call_budget_is_no_answer_and_says_powerview_may_still_run_it() {
    let w = World::new("cmdblock");
    let s = connected(2).expect_cmd("WAIT 60.s").silence(std::time::Duration::from_secs(11));
    let out = call("t32ctl raw cmd 'WAIT 60.s'", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        out.text,
        "error[no-answer]: WAIT 60.s: command: no answer for 11.00s (idle timeout, 0 keepalive frames seen)\n\
         \x20 PowerView may still run the command, and it accepts no new connection until the command ends\n"
    );
    assert_eq!(out.code, 5);
}

/// `read reg`'s `CORE()`, then one `Register()` per name with its value, on a stopped target.
fn regs(names: &[(&str, u64)]) -> Script {
    let s = connected(2).expect_fn("CORE()").reply_decimal(0);
    let s =
        names.iter().fold(s, |s, (n, v)| s.expect_fn(&format!("Register({n})")).reply_function(4, &format!("0x{v:X}")));
    stopped_header(s)
}

#[test]
fn read_reg_prints_the_named_registers_as_typed_with_16_hex_digits() {
    let w = World::new("reg");
    let out = call("t32ctl read reg x0 sp", &deps(w.env(), vec![Ok(regs(&[("X0", 0), ("SP", 0x4010_7e40)]))]));
    assert_eq!(out.text, "stopped core=0 pc=0x40083e10\nx0=0x0000000000000000 sp=0x0000000040107e40\n");
    assert_eq!(out.code, 0);
    let d = deps(w.env(), vec![Ok(regs(&[("ESR_EL1", 0x9600_0045)]))]);
    let out = call("t32ctl read reg esr_el1 --json", &d);
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(
        v["body"],
        serde_json::json!({"kind": "regs", "core": 0, "regs": [{"name": "esr_el1", "value": "0x0000000096000045"}]})
    );
}

#[test]
fn read_reg_without_a_name_reads_the_aarch64_general_purpose_set_four_per_line() {
    let w = World::new("regset");
    let mut names: Vec<String> = (0..=30).map(|i| format!("X{i}")).collect();
    names.extend(["SP", "PC", "CPSR"].map(String::from));
    let values: Vec<(&str, u64)> = names.iter().enumerate().map(|(i, n)| (n.as_str(), i as u64)).collect();
    let out = call("t32ctl read reg", &deps(w.env(), vec![Ok(regs(&values))]));
    let lines: Vec<&str> = out.text.lines().collect();
    assert_eq!(lines.len(), 1 + 9, "{}", out.text);
    assert_eq!(lines[1], "x0=0x0000000000000000 x1=0x0000000000000001 x2=0x0000000000000002 x3=0x0000000000000003");
    assert_eq!(lines[9], "pc=0x0000000000000020 cpsr=0x0000000000000021");
}

#[test]
fn a_register_powerview_refuses_is_exit_2_and_named() {
    let w = World::new("regrefused");
    let s = connected(2).expect_fn("CORE()").reply_decimal(0).expect_fn("Register(X0)").reply_function(4, "0x1");
    let s = stopped_header(s.expect_fn("Register(FOO)").reply_function(0, "syntax error"));
    let out = call("t32ctl read reg x0 foo", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         error[refused]: Register(FOO): function: PowerView refused: function failed (code 0): syntax error\n"
    );
    assert_eq!(out.code, 2);
    let out = call("t32ctl read reg x0+8", &deps(w.env(), vec![]));
    assert_eq!(
        out.text,
        "error[notation]: register name \"x0+8\" is not a letter followed by at most 31 letters, digits or _\n"
    );
    assert_eq!(out.code, 2);
}

#[test]
fn read_reg_refuses_a_down_or_running_target_before_any_register_read() {
    let w = World::new("regstate");
    let down = connected(0).expect_req(Request::TargetState).reply_state(0);
    let out = call("t32ctl read reg pc", &deps(w.env(), vec![Ok(down)]));
    assert_eq!(out.text, "down\nerror[target-down]: the target is down: registers need an attached target\n");
    assert_eq!(out.code, 3);
    let running = connected(3).expect_req(Request::TargetState).reply_state(3);
    let running = running.expect_fn("CORE.NUMBER()").reply_decimal(4);
    let out = call("t32ctl read reg pc", &deps(w.env(), vec![Ok(running)]));
    assert_eq!(
        out.text,
        "running cores=0-3\nerror[not-halted]: the target is running: registers need a stopped target\n"
    );
    assert_eq!(out.code, 3);
}

/// `PROFILE` with a memory map: ddr readable while running, sram, and uart0 with a deny hole.
const MAP: &str = "schema = 1\n[target]\nname = \"acme_evb\"\ncpu = \"CortexA53\"\ncores = 4\nword = 8\n\
    [[memory]]\nname = \"ddr\"\nstart = 0x40000000\nsize = 0x40000000\nkind = \"ram\"\nrunning = true\n\
    [[memory]]\nname = \"sram\"\nstart = 0x00100000\nsize = 0x00040000\nkind = \"ram\"\n\
    [[memory]]\nname = \"uart0\"\nstart = 0x09000000\nsize = 0x00001000\nkind = \"mmio\"\nwidth = 4\n\
    [[memory]]\nname = \"uart0-fifo\"\nstart = 0x09000000\nsize = 0x00000004\nkind = \"deny\"\n";

fn mapped(tag: &str) -> World {
    let w = World::new(tag);
    w.write("ws/board/support/t32ctl/profile.toml", MAP);
    w
}

fn le(cells: &[u64]) -> Vec<u8> {
    cells.iter().flat_map(|c| c.to_le_bytes()).collect()
}

fn mem_read(access: &str, addr: u64, len: u16) -> Request {
    Request::MemRead { access: access.into(), addr, len }
}

const STACK: [u64; 8] = [0x4010_7e60, 0x4008_1fa4, 0, 0x4008_f2d8, 0x4008_0b1c, 0x6000_0005, 0x4010_4000, 8];

/// `read mem @sp --len 64` as the contract prints it: the stack pointer, one page translated,
/// one read.
fn stack_read() -> Script {
    let s = connected(2).expect_fn("Register(SP)").reply_function(4, "0x40107E40");
    let s = s.expect_fn("MMU.PHYSICAL(D:0x40107000)").reply_text("A:0x40107000");
    stopped_header(s.expect_req(mem_read("D", 0x4010_7e40, 64)).reply_mem(&le(&STACK)))
}

#[test]
fn read_mem_resolves_a_register_place_translates_it_and_prints_cells_16_bytes_per_line() {
    let w = mapped("memstack");
    let out = call("t32ctl read mem @sp --len 64", &deps(w.env(), vec![Ok(stack_read())]));
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         mem 0x40107e40 len=64 width=8 class=D phys=0x40107e40 (region ram \"ddr\")\n\
         0x40107e40: 0x0000000040107e60 0x0000000040081fa4\n\
         0x40107e50: 0x0000000000000000 0x000000004008f2d8\n\
         0x40107e60: 0x0000000040080b1c 0x0000000060000005\n\
         0x40107e70: 0x0000000040104000 0x0000000000000008\n"
    );
    assert_eq!(out.code, 0);
    let out = call("t32ctl read mem @sp --len 64 --json", &deps(w.env(), vec![Ok(stack_read())]));
    let v: Value = serde_json::from_str(&out.text).unwrap();
    let cells: Vec<String> = STACK.iter().map(|c| format!("{c:#018x}")).collect();
    assert_eq!(
        v["body"],
        serde_json::json!({"kind": "mem", "place": "D:0x40107e40", "phys": "A:0x40107e40", "len": 64, "width": 8,
            "regions": [{"name": "ddr", "kind": "ram"}], "cells": cells})
    );
}

#[test]
fn an_mmio_read_carries_the_regions_width_and_defaults_to_one_cell() {
    let w = mapped("memmmio");
    let s = connected(2).expect_req(Request::MemReadWidth { access: "A".into(), addr: 0x0900_0004, len: 4, width: 4 });
    let out = call(
        "t32ctl read mem A:0x09000004",
        &deps(w.env(), vec![Ok(stopped_header(s.reply_mem(&[0x78, 0x56, 0x34, 0x12])))]),
    );
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         mem A:0x9000004 len=4 width=4 class=A (region mmio \"uart0\")\n\
         A:0x9000004: 0x12345678\n"
    );
    let out = call("t32ctl read mem A:0x09000004 --len 6", &deps(w.env(), vec![Ok(stopped_header(connected(2)))]));
    assert_eq!(
        (out.text.lines().nth(1), out.code),
        (Some("error[notation]: --len 6 is not a multiple of the access width 4"), 2)
    );
}

#[test]
fn a_bulk_read_goes_in_read_chunk_pieces() {
    let w = mapped("membulk");
    w.write("home/.config/t32ctl/config.toml", "schema = 1\n[tuning]\nread_chunk = 1024\n");
    let s = (0..4u64).fold(connected(2), |s, i| {
        s.expect_req(mem_read("A", 0x4000_0000 + i * 1024, 1024)).reply_mem(&[i as u8; 1024])
    });
    let out = call("t32ctl read mem A:0x40000000 --len 4K", &deps(w.env(), vec![Ok(stopped_header(s))]));
    let lines: Vec<&str> = out.text.lines().collect();
    assert_eq!(lines.len(), 2 + 256);
    assert_eq!(lines[1], "mem A:0x40000000 len=4K width=8 class=A (region ram \"ddr\")");
    assert_eq!(lines[2], "A:0x40000000: 0x0000000000000000 0x0000000000000000");
    assert_eq!(lines[257], "A:0x40000ff0: 0x0303030303030303 0x0303030303030303");
}

#[test]
fn a_translated_read_checks_and_reads_each_physical_piece() {
    let w = mapped("mempages");
    let s = connected(2)
        .expect_fn("MMU.PHYSICAL(D:0x40000000)")
        .reply_text("A:0x40100000")
        .expect_fn("MMU.PHYSICAL(D:0x40001000)")
        .reply_text("A:0x40200000")
        .expect_req(mem_read("D", 0x4000_0ff8, 8))
        .reply_mem(&[1; 8])
        .expect_req(mem_read("D", 0x4000_1000, 8))
        .reply_mem(&[2; 8]);
    let out = call("t32ctl read mem 0x40000ff8 --len 16 --width 4", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         mem 0x40000ff8 len=16 width=4 class=D phys=0x40100ff8 (region ram \"ddr\")\n\
         0x40000ff8: 0x01010101 0x01010101 0x02020202 0x02020202\n"
    );
    let s = connected(2).expect_fn("MMU.PHYSICAL(D:0x40000000)").reply_function(0, "no translation");
    let out = call("t32ctl read mem 0x40000010", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        out.text.lines().nth(1),
        Some(
            "error[memory-policy]: no physical address for D:0x40000000: MMU.PHYSICAL(D:0x40000000): function: \
             PowerView refused: function failed (code 0): no translation"
        )
    );
}

#[test]
fn a_refused_byte_is_named_and_nothing_is_read() {
    let w = mapped("memdeny");
    let out = call("t32ctl read mem A:0x09000000 --len 16", &deps(w.env(), vec![Ok(stopped_header(connected(2)))]));
    assert_eq!(out.text, "stopped core=0 pc=0x40083e10\nerror[memory-policy]: 0x09000000 in deny region uart0-fifo\n");
    assert_eq!(out.code, 2);
    let out = call("t32ctl read mem A:0x0a000000", &deps(w.env(), vec![Ok(stopped_header(connected(2)))]));
    assert_eq!(out.text.lines().nth(1), Some("error[memory-policy]: 0x0a000000 in no region"));
}

#[test]
fn checks_that_need_no_powerview_refuse_before_connecting() {
    let w = mapped("memlocal");
    for (line, want) in [
        (
            "t32ctl read mem C15:0x0",
            "error[memory-policy]: access class C15 is not one of the profile's classes A, D, P; such an access is raw",
        ),
        ("t32ctl read mem 0x40000000 --len 8K", "error[notation]: --len 8K is above 4K; above 4K needs --to NAME"),
        ("t32ctl read mem 0x40000000 --len 6 --width 4", "error[notation]: --len 6 is not a multiple of --width 4"),
        ("t32ctl read mem 0x40000000 --len 0", "error[notation]: --len 0: a read needs at least one byte"),
        (
            "t32ctl read mem 0x40000000 --len 2G --to ddr.bin",
            "error[notation]: --len 2G is above [calls] transfer_max 1G",
        ),
        (
            "t32ctl read mem worker_loop",
            "error[notation]: place \"worker_loop\": symbol places are planned, not in this t32ctl; use [CLASS:]0x<hex> or @<reg>[+0x<hex>|-0x<hex>]",
        ),
    ] {
        let out = call(line, &deps(w.env(), vec![]));
        assert_eq!((out.text.trim_end(), out.code), (want, 2), "{line}");
    }
    assert!(!w.0.join("home/.local/state/t32ctl/out").read_dir().is_ok_and(|mut d| d.next().is_some()));
}

#[test]
fn a_running_target_reads_regions_that_allow_it_with_the_e_class_and_refuses_the_rest() {
    let w = mapped("memrunning");
    let header =
        |s: Script| s.expect_req(Request::TargetState).reply_state(3).expect_fn("CORE.NUMBER()").reply_decimal(4);
    let s = connected(3).expect_req(mem_read("EA", 0x4000_0000, 8)).reply_mem(&[0; 8]);
    let out = call("t32ctl read mem A:0x40000000", &deps(w.env(), vec![Ok(header(s))]));
    assert_eq!(
        out.text,
        "running cores=0-3\nmem A:0x40000000 len=8 width=8 class=A (region ram \"ddr\")\nA:0x40000000: 0x0000000000000000\n"
    );
    let out = call("t32ctl read mem A:0x00100000", &deps(w.env(), vec![Ok(header(connected(3)))]));
    assert_eq!(
        (out.text.lines().nth(1), out.code),
        (Some("error[not-halted]: the target is running and region sram does not declare running = true"), 3)
    );
    let out = call("t32ctl read mem @sp", &deps(w.env(), vec![Ok(header(connected(3)))]));
    assert_eq!(
        (out.text.lines().nth(1), out.code),
        (Some("error[not-halted]: the target is running: @reg places need a stopped target"), 3)
    );
    let down = connected(0).expect_req(Request::TargetState).reply_state(0);
    let out = call("t32ctl read mem A:0x40000000", &deps(w.env(), vec![Ok(down)]));
    assert_eq!(
        (out.text.as_str(), out.code),
        ("down\nerror[target-down]: the target is down: memory needs an attached target\n", 3)
    );
}

/// The one run directory below `out/`, if any.
pub(crate) fn run_dirs(w: &World) -> Vec<PathBuf> {
    let out = w.0.join("home/.local/state/t32ctl/out");
    let Ok(dir) = out.read_dir() else { return Vec::new() };
    dir.map(|e| e.unwrap().path()).collect()
}

#[test]
fn read_mem_to_writes_a_new_file_in_a_fresh_output_directory() {
    let w = mapped("memto");
    let data: Vec<u8> = (0..32).collect();
    let s = connected(2).expect_req(mem_read("A", 0x4000_0000, 32)).reply_mem(&data);
    let out = call("t32ctl read mem A:0x40000000 --len 32 --to ddr.bin", &deps(w.env(), vec![Ok(stopped_header(s))]));
    let dirs = run_dirs(&w);
    assert_eq!(dirs.len(), 1);
    let id = dirs[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(id.len() == 6 && id.starts_with("m-") && id[2..].bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
    assert_eq!(fs::read(dirs[0].join("ddr.bin")).unwrap(), data);
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             mem A:0x40000000 len=32 width=8 class=A (region ram \"ddr\")\n\
             file: ~/.local/state/t32ctl/out/{id}/ddr.bin (32 in 0ms)\n"
        )
    );
    assert_eq!(out.code, 0);
}

#[test]
fn a_transfer_stopped_by_its_bound_is_exit_1_and_leaves_no_file() {
    let w = mapped("membound");
    w.write(
        "home/.config/t32ctl/config.toml",
        "schema = 1\n[tuning]\nread_chunk = 16\n[calls]\ntransfer_timeout = \"1s\"\n",
    );
    let s = connected(2).expect_req(mem_read("A", 0x4000_0000, 16)).reply_mem(&[0; 16]);
    let s = s.expect_req(mem_read("A", 0x4000_0010, 16)).silence(std::time::Duration::from_secs(2));
    let out = call("t32ctl read mem A:0x40000000 --len 64 --to x.bin", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(
        out.text,
        "mem A:0x40000000 len=64 width=8 class=A (region ram \"ddr\")\n\
         transfer stopped at the 1s bound after 0; the partial file was removed\n"
    );
    assert_eq!(out.code, 1);
    assert_eq!(run_dirs(&w), Vec::<PathBuf>::new(), "the empty run directory is removed");
    let s = connected(2).expect_req(mem_read("A", 0x4000_0000, 16)).reply_error(16, "");
    let out = call("t32ctl read mem A:0x40000000 --len 64 --to x.bin", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(out.code, 2, "{}", out.text);
    assert!(out.text.contains("error[refused]: read of A:0x40000000:"), "{}", out.text);
    assert_eq!(run_dirs(&w), Vec::<PathBuf>::new());
}

/// `PROFILE` with the contract's procedures (without `lease`) and their scripts.
pub(crate) const PROCS: &str = "\n[proc.attach]\nabout = \"Connect to the A53 cluster without resetting it\"\n\
    script = \"attach.cmm\"\ngrade = \"run\"\ntimeout = \"30s\"\neffects = [\"debugger-config\"]\n\
    \n[proc.selftest]\nabout = \"Read clock and PMIC status into selftest.txt\"\nscript = \"selftest.cmm\"\n\
    grade = \"read\"\ntimeout = \"30s\"\neffects = []\noutputs = [\"selftest.txt\"]\n\
    \n[proc.flash]\nabout = \"Write an image into eMMC and verify it by readback\"\nscript = \"flash_emmc.cmm\"\n\
    grade = \"burn\"\ntimeout = \"45m\"\neffects = [\"flash\", \"target-reset\", \"target-memory\", \"target-registers\"]\n\
    outputs = [\"verify.log\"]\n\
    \n[proc.flash.params.image]\ntype = \"file\"\nabout = \"image to write\"\ndefault = \"{build}/zephyr/zephyr.bin\"\n\
    \n[proc.flash.params.part]\ntype = \"enum\"\nvalues = [\"boot0\", \"boot1\", \"user\"]\n\
    about = \"eMMC hardware partition\"\ndefault = \"user\"\n\
    \n[proc.flash.params.offset]\ntype = \"integer\"\nabout = \"byte offset inside the partition\"\nmin = 0\ndefault = 0\n";

pub(crate) fn with_procs(tag: &str) -> World {
    let w = World::new(tag);
    w.write("ws/board/support/t32ctl/profile.toml", &format!("{PROFILE}{PROCS}"));
    for script in ["attach.cmm", "selftest.cmm", "flash_emmc.cmm"] {
        w.write(&format!("ws/board/support/t32ctl/{script}"), "ENDDO\n");
    }
    w
}

pub(crate) fn profile_hash(w: &World) -> String {
    profile::hash(&profile::walk(&w.0.join("ws/board/support/t32ctl")).unwrap()).to_string()
}

#[test]
fn read_procs_lists_the_procedures_without_connecting() {
    let w = with_procs("procs");
    let hash = profile_hash(&w);
    let out = call("t32ctl read procs", &deps(w.env(), vec![]));
    assert_eq!(
        out.text,
        format!(
            "proc      grade  timeout  about\n\
             attach    run    30s      Connect to the A53 cluster without resetting it\n\
             selftest  read   30s      Read clock and PMIC status into selftest.txt\n\
             flash     burn   45m      Write an image into eMMC and verify it by readback\n\
             board/support/t32ctl/ profile={}; parameters: t32ctl read procs NAME\n",
            &hash[..8]
        )
    );
    assert_eq!(out.code, 0);
    assert!(!w.pv_dir().exists(), "no state directory: nothing connects");
    let none = World::new("procs-none");
    let out = call("t32ctl read procs", &deps(none.env(), vec![]));
    assert_eq!(out.text, format!("no procedures in board/support/t32ctl/ profile={}\n", &profile_hash(&none)[..8]));
}

#[test]
fn read_procs_name_prints_the_declaration_and_its_usage() {
    let w = with_procs("procs-one");
    let hash = profile_hash(&w);
    let out = call("t32ctl read procs flash", &deps(w.env(), vec![]));
    assert_eq!(
        out.text,
        format!(
            "proc     flash  grade burn  timeout 45m\n\
             about    Write an image into eMMC and verify it by readback\n\
             params   --image FILE      image to write (default: {{build}}/zephyr/zephyr.bin)\n         \
             --part ENUM       eMMC hardware partition: boot0, boot1, user (default: user)\n         \
             --offset INTEGER  byte offset inside the partition, at least 0 (default: 0)\n\
             outputs  verify.log\n\
             effects  flash, target-reset, target-memory, target-registers\n\
             profile  board/support/t32ctl/ profile={}\n\
             usage    t32ctl burn proc flash [--image FILE] [--part boot0|boot1|user] [--offset N]\n",
            &hash[..8]
        )
    );
    let out = call("t32ctl read procs attach", &deps(w.env(), vec![]));
    let lines: Vec<&str> = out.text.lines().collect();
    assert_eq!(lines[2..5], ["params   none", "outputs  none", "effects  debugger-config"]);
    assert_eq!(lines[6], "usage    t32ctl run proc attach");
}

#[test]
fn read_procs_json_carries_every_declared_field() {
    let w = with_procs("procs-json");
    let out = call("t32ctl read procs flash --json", &deps(w.env(), vec![]));
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["header"], Value::Null);
    let files = profile::walk(&w.0.join("ws/board/support/t32ctl")).unwrap().len();
    assert_eq!(
        v["body"]["profile"],
        serde_json::json!({"dir": "board/support/t32ctl/", "hash": profile_hash(&w), "files": files})
    );
    assert_eq!(
        v["body"]["procs"][0],
        serde_json::json!({
            "name": "flash", "grade": "burn", "timeout_ms": 2_700_000,
            "about": "Write an image into eMMC and verify it by readback",
            "effects": ["flash", "target-reset", "target-memory", "target-registers"], "outputs": ["verify.log"],
            "params": [
                {"name": "image", "flag": "--image", "type": "file", "about": "image to write",
                 "default": "{build}/zephyr/zephyr.bin", "values": [], "min": null, "max": null, "required": false},
                {"name": "part", "flag": "--part", "type": "enum", "about": "eMMC hardware partition",
                 "default": "user", "values": ["boot0", "boot1", "user"], "min": null, "max": null, "required": false},
                {"name": "offset", "flag": "--offset", "type": "integer", "about": "byte offset inside the partition",
                 "default": "0", "values": [], "min": 0, "max": null, "required": false},
            ]
        })
    );
    let out = call("t32ctl read procs --json", &deps(w.env(), vec![]));
    let v: Value = serde_json::from_str(&out.text).unwrap();
    let names: Vec<&str> = v["body"]["procs"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["attach", "selftest", "flash"]);
}

#[test]
fn an_undeclared_procedure_is_unknown_proc_and_names_the_declared_ones() {
    let w = with_procs("procs-unknown");
    let out = call("t32ctl read procs flsh", &deps(w.env(), vec![]));
    assert_eq!(
        out.text,
        "error[unknown-proc]: the board profile declares no procedure \"flsh\"; t32ctl read procs lists them\n  \
         declared: attach, selftest, flash\n"
    );
    assert_eq!(out.code, 2);
}
