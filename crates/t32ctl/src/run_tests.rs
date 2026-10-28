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
