// SPDX-License-Identifier: Apache-2.0
//! Whole calls in-process against scripted PowerView conversations.

use std::path::Path;

use serde_json::Value;
use t32rcl::testkit::{ConnectFailure, ManualClock, Request, Script, ScriptedConnector};

use super::*;
use crate::cli::tests::words;

/// A home and a workspace in a fresh temp directory, removed when dropped.
struct World(PathBuf);

impl World {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("t32ctl-run-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ws")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        Self(root)
    }

    fn env(&self) -> Env {
        Env {
            cwd: self.0.join("ws"),
            home: Some(self.0.join("home")),
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

/// Attach, then the prelude's `T32_GetState`.
fn connected(state: u8) -> Script {
    Script::new().attach().expect_req(Request::TargetState).reply_state(state)
}

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
    let d = deps(w.env(), vec![Ok(stopped_header(pv(connected(2), Some(1), 0, 0)))]);
    let out = call("t32ctl read status", &d);
    assert_eq!(out.text, "stopped core=0 pc=0x40083e10\npv      build=168999 clients=1 practice=idle\n");
    assert_eq!(out.code, 0);
    assert!(w.pv_dir().is_dir(), "the state directory exists before the call connects");
}

#[test]
fn a_running_target_shows_its_cores_and_a_down_one_only_down() {
    let w = World::new("running");
    let running = pv(connected(3), Some(1), 0, 0).expect_req(Request::TargetState).reply_state(3);
    let d = deps(w.env(), vec![Ok(running.expect_fn("CORE.NUMBER()").reply_decimal(4))]);
    assert_eq!(call("t32ctl read status", &d).text.lines().next(), Some("running cores=0-3"));
    let down = pv(connected(0), Some(1), 0, 0).expect_req(Request::TargetState).reply_state(0);
    let d = deps(w.env(), vec![Ok(down)]);
    let out = call("t32ctl read status", &d);
    assert_eq!((out.text.lines().next(), out.code), (Some("down"), 0));
}

#[test]
fn a_refused_pc_shortens_the_header_and_never_fails_the_call() {
    let w = World::new("nopc");
    let s = pv(connected(2), Some(1), 0, 0).expect_req(Request::TargetState).reply_state(2);
    let s = s.expect_fn("CORE()").reply_decimal(1).expect_fn("Register(PC)").reply_function(0, "target system down");
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(s)]));
    assert_eq!((out.text.lines().next(), out.code), (Some("stopped core=1"), 0));
}

#[test]
fn clients_fall_back_to_the_api_spelling_then_print_a_question_mark() {
    let w = World::new("clients");
    let s = connected(2)
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
    assert_eq!(out.text.lines().nth(1), Some("pv      build=172803 clients=2 practice=idle"));
    let s = stopped_header(pv(connected(2), None, 0, 0));
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(s)]));
    assert_eq!(out.text.lines().nth(1), Some("pv      build=168999 clients=? practice=idle"));
}

#[test]
fn an_idle_practice_with_a_non_empty_stack_is_a_halted_script() {
    let w = World::new("halted");
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(pv(connected(2), Some(1), 0, 1)))]));
    assert_eq!(out.text.lines().nth(1), Some("pv      build=168999 clients=1 practice=halted (depth 1)"));
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(pv(connected(2), Some(1), 2, 1)))]));
    assert_eq!(out.text.lines().nth(1), Some("pv      build=168999 clients=1 practice=dialog (depth 1)"));
}

#[test]
fn full_status_adds_the_binding_the_config_and_the_state_directory() {
    let w = World::new("full");
    let board = w.0.join("ws/boards/acme/acme_evb");
    w.write("ws/build/CMakeCache.txt", &format!("BOARD_DIR:PATH={}\nBOARD:STRING=acme_evb/a53\n", board.display()));
    w.write("home/.config/t32ctl/config.toml", "schema = 1\n[connection]\nport = 20000\n");
    let env = Env { auto_hosts: vec!["127.0.0.1".into(), "172.29.32.1".into()], ..w.env() };
    let d = deps(env, vec![Err(ConnectFailure::Refused), Ok(stopped_header(pv(connected(2), Some(1), 0, 0)))]);
    let out = call("t32ctl read status --full", &d);
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         binding build=build (auto-detection: ./build/CMakeCache.txt), \
         profile=boards/acme/acme_evb/support/t32ctl/ (BOARD_DIR), BOARD=acme_evb/a53\n\
         config  ~/.config/t32ctl/config.toml: host=auto (172.29.32.1) port=20000\n\
         state   ~/.local/state/t32ctl/pv/auto-20000/\n\
         pv      build=168999 clients=1 practice=idle\n"
    );
    let d = deps(w.env(), vec![Ok(stopped_header(pv(connected(2), Some(1), 0, 0)))]);
    let out = call("t32ctl read status --full --build elsewhere", &d);
    assert!(out.text.contains("binding build=elsewhere (--build), profile none, BOARD none\n"), "{}", out.text);
}

#[test]
fn json_renders_the_same_report() {
    let w = World::new("json");
    let out =
        call("t32ctl read status --json", &deps(w.env(), vec![Ok(stopped_header(pv(connected(2), Some(1), 0, 0)))]));
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["schema"], "t32ctl.report/1");
    assert_eq!(v["command"], "read status");
    assert_eq!(v["exit"], serde_json::json!({"code": 0, "class": "ok"}));
    assert_eq!(v["header"], serde_json::json!({"run": "stopped", "core": 0, "at": {"pc": "0x40083e10"}}));
    assert_eq!(v["body"]["kind"], "status");
    assert_eq!(v["body"]["pv"]["clients"], 1);
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
    let s = pv(s.expect_req(Request::TargetState).reply_state(2), Some(1), 0, 0);
    let out = call("t32ctl read status", &deps(w.env(), vec![Ok(stopped_header(s))]));
    assert_eq!(
        out.text,
        "stopped core=0 pc=0x40083e10\n\
         ! cleaned up after an interrupted call: 1 step\n\
         ! residue: unwind PRACTICE stack (left by an earlier run); in PowerView type: END \
         (END also closes every dialog and OPEN file)\n\
         pv      build=168999 clients=1 practice=idle\n"
    );
    assert_eq!(out.code, 0);
    assert!(!w.pv_dir().join("journal/t32ctl-1-1.undo").exists());
}

#[test]
fn a_refused_read_is_exit_2_and_the_header_still_prints() {
    let w = World::new("refused");
    let s = connected(2)
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
