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
const PROFILE: &str = "schema = 1\n[target]\nname = \"acme_evb\"\ncpu = \"CortexA53\"\ncores = 4\nword = 8\n";

/// A home and a workspace in a fresh temp directory, removed when dropped. `T32CTL_PROFILE`
/// names `board/support/t32ctl`, which holds `PROFILE`.
struct World(PathBuf);

impl World {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("t32ctl-run-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ws")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let w = Self(root);
        w.write("ws/board/support/t32ctl/profile.toml", PROFILE);
        w
    }

    fn env(&self) -> Env {
        Env {
            cwd: self.0.join("ws"),
            home: Some(self.0.join("home")),
            profile: Some("board/support/t32ctl".into()),
            auto_hosts: vec!["127.0.0.1".into()],
            ..Env::default()
        }
    }

    fn pv_dir(&self) -> PathBuf {
        self.0.join("home/.local/state/t32ctl/pv/auto-20000")
    }

    fn write(&self, rel: &str, text: &str) {
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

fn deps(env: Env, connects: Vec<Result<Script, ConnectFailure>>) -> Deps {
    let clock = ManualClock::new();
    let connector = connects.into_iter().fold(ScriptedConnector::new(clock.clone()), |c, s| c.push(s));
    Deps { env, connector: Arc::new(connector), clock: Arc::new(clock), cancel: Cancel::new() }
}

fn call(line: &str, deps: &Deps) -> Output {
    let argv: Vec<OsString> = words(line).into_iter().map(OsString::from).collect();
    run(&argv, deps)
}

/// Attach, then the prelude: `T32_GetState`, and `SYStem.CPU()` unless the target is down.
fn connected(state: u8) -> Script {
    connected_to(state, "CortexA53")
}

fn connected_to(state: u8, cpu: &str) -> Script {
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
fn stopped_header(s: Script) -> Script {
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
         \x20 PowerView not running, RCL not enabled (RCL=NETTCP in config.t32), or wrong --port\n\
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
    let s = Script::new().attach().expect_fn("OS.FILE.readable(\"C:\\T\\t32ctl\\k\\a.bin\")").reply_bool(false);
    let s = s.expect_req(Request::TargetState).reply_state(2).expect_fn("SYStem.CPU()").reply_text("CortexA53");
    let s = pv(cores(s), Some(1), 0, 0);
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        out.text,
        format!(
            "stopped core=0 pc=0x40083e10\n\
             ! cleaned up after an interrupted call: 1 step\n\
             ! residue: unwind PRACTICE stack (left by an earlier run); in PowerView type: END \
             (END also closes every dialog and OPEN file)\n\
             {TARGET}pv      build=168999 clients=1 practice=idle\n"
        )
    );
    assert_eq!(out.code, 0);
    assert!(!w.pv_dir().join("journal/t32ctl-1-1.undo").exists());
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
