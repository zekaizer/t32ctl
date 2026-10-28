// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::error::ErrorKind;

fn pv(practice: Practice, depth: i64, clients: Option<i64>) -> PvStatus {
    PvStatus { build: 168_999, base: Some(166_414), clients, practice, depth }
}

fn target(cpu: &str, cpu_check: CpuMatch, cores: i64) -> TargetView {
    TargetView {
        name: "acme_evb".into(),
        cpu: cpu.into(),
        profile_cpu: "CortexA53".into(),
        cpu_check,
        cores,
        profile_cores: 4,
    }
}

fn profile() -> ProfileView {
    ProfileView { dir: "boards/acme/acme_evb/support/t32ctl/".into(), hash: None, files: None }
}

fn status(pv: PvStatus) -> Body {
    Body::Status(Box::new(StatusBody {
        target: target("CortexA53", CpuMatch::Match, 4),
        profile: profile(),
        pv,
        binding: None,
        config: None,
        state_dir: None,
    }))
}

#[test]
fn a_status_report_prints_header_notices_and_body_in_order() {
    let mut r = Report::new("read status");
    r.header = Some(Header::Stopped { core: Some(0), at: Some(At { pc: Hex(0x4008_3e10) }) });
    r.notices.push(Notice::new("cleaned up after an interrupted call: 2 steps"));
    r.body = Some(status(pv(Practice::Idle, 0, Some(1))));
    assert_eq!(
        r.text(),
        "stopped core=0 pc=0x40083e10\n\
         ! cleaned up after an interrupted call: 2 steps\n\
         target  acme_evb cpu=CortexA53 (matches profile) cores=4\n\
         profile boards/acme/acme_evb/support/t32ctl/\n\
         pv      build=168999 clients=1 practice=idle\n"
    );
    assert_eq!(r.exit(), Exit::Ok);
    assert_eq!(
        r.json(),
        concat!(
            r#"{"schema":"t32ctl.report/1","command":"read status","exit":{"code":0,"class":"ok"},"#,
            r#""header":{"run":"stopped","core":0,"at":{"pc":"0x40083e10"}},"#,
            r#""notices":[{"text":"cleaned up after an interrupted call: 2 steps"}],"#,
            r#""body":{"kind":"status","target":{"name":"acme_evb","cpu":"CortexA53","profile_cpu":"CortexA53","#,
            r#""cpu_check":"match","cores":4,"profile_cores":4},"#,
            r#""profile":{"dir":"boards/acme/acme_evb/support/t32ctl/"},"#,
            r#""pv":{"build":168999,"base":166414,"clients":1,"practice":"idle","depth":0}},"#,
            r#""truncated":null,"error":null,"next":[]}"#,
            "\n"
        )
    );
}

#[test]
fn headers_print_the_run_state_and_drop_what_powerview_refused() {
    let text = |h: Header| h.to_string();
    let json = |h: Header| serde_json::to_string(&h).unwrap();
    assert_eq!(text(Header::Down), "down");
    assert_eq!(json(Header::Down), r#"{"run":"down"}"#);
    assert_eq!(text(Header::Stopped { core: Some(2), at: None }), "stopped core=2");
    assert_eq!(json(Header::Stopped { core: Some(2), at: None }), r#"{"run":"stopped","core":2}"#);
    assert_eq!(text(Header::Stopped { core: None, at: None }), "stopped");
    assert_eq!(text(Header::Running { cores: Some(4) }), "running cores=0-3");
    assert_eq!(json(Header::Running { cores: Some(4) }), r#"{"run":"running","cores":4}"#);
    assert_eq!(text(Header::Running { cores: Some(1) }), "running cores=0");
    assert_eq!(text(Header::Running { cores: None }), "running");
    assert_eq!(text(Header::Other { state: 1 }), "state=1");
    assert_eq!(json(Header::Other { state: 1 }), r#"{"run":"other","state":1}"#);
}

#[test]
fn the_pv_line_shows_unknown_clients_and_a_non_empty_stack() {
    let line = |p: PvStatus| status(p).lines()[2].clone();
    assert_eq!(line(pv(Practice::Halted, 1, None)), "pv      build=168999 clients=? practice=halted (depth 1)");
    assert_eq!(line(pv(Practice::Running, 2, Some(2))), "pv      build=168999 clients=2 practice=running (depth 2)");
    assert_eq!(line(pv(Practice::Other(7), 1, Some(1))), "pv      build=168999 clients=1 practice=state7 (depth 1)");
}

#[test]
fn the_target_line_says_whether_the_cpu_matches_and_shows_the_profiles_cores_when_they_differ() {
    assert_eq!(target("CortexA53", CpuMatch::Match, 4).to_string(), "acme_evb cpu=CortexA53 (matches profile) cores=4");
    assert_eq!(
        target("CortexA72", CpuMatch::Mismatch, 2).to_string(),
        "acme_evb cpu=CortexA72 (profile: CortexA53, mismatch) cores=2 (profile: 4)"
    );
    assert_eq!(
        target("CortexA53", CpuMatch::Skipped, 4).to_string(),
        "acme_evb cpu=CortexA53 (not checked while down) cores=4"
    );
    assert_eq!(
        serde_json::to_string(&target("CortexA72", CpuMatch::Mismatch, 2)).unwrap(),
        r#"{"name":"acme_evb","cpu":"CortexA72","profile_cpu":"CortexA53","cpu_check":"mismatch","cores":2,"profile_cores":4}"#
    );
    assert_eq!(serde_json::to_string(&CpuMatch::Skipped).unwrap(), r#""skipped""#);
}

#[test]
fn full_status_adds_the_hash_binding_config_and_state_lines_before_pv() {
    let body = Body::Status(Box::new(StatusBody {
        target: target("CortexA53", CpuMatch::Match, 4),
        profile: ProfileView { hash: Some("1c07e9b2".to_string() + &"0".repeat(56)), files: Some(6), ..profile() },
        pv: pv(Practice::Idle, 0, Some(1)),
        binding: Some(BindingView {
            build: Some(Sourced { path: "build".into(), source: "auto-detection: ./build/CMakeCache.txt".into() }),
            profile_from: "BOARD_DIR".into(),
            variant: None,
            board: Some("acme_evb/a53".into()),
        }),
        config: Some(ConfigView {
            file: Some("~/.config/t32ctl/config.toml".into()),
            host: "auto".into(),
            attached: Some("127.0.0.1".into()),
            port: 20000,
        }),
        state_dir: Some("~/.local/state/t32ctl/pv/auto-20000/".into()),
    }));
    assert_eq!(
        body.lines(),
        [
            "target  acme_evb cpu=CortexA53 (matches profile) cores=4",
            "profile boards/acme/acme_evb/support/t32ctl/ profile=1c07e9b2 (6 files)",
            "binding build=build (auto-detection: ./build/CMakeCache.txt), profile from BOARD_DIR, \
             variant base (BOARD=acme_evb/a53)",
            "config  ~/.config/t32ctl/config.toml: host=auto (127.0.0.1) port=20000",
            "state   ~/.local/state/t32ctl/pv/auto-20000/",
            "pv      build=168999 clients=1 practice=idle",
        ]
    );
    let json = serde_json::to_string(&body).unwrap();
    assert!(
        json.contains(&format!(
            r#""profile":{{"dir":"boards/acme/acme_evb/support/t32ctl/","hash":"1c07e9b2{}","files":6}},"#,
            "0".repeat(56)
        )),
        "{json}"
    );
    assert!(
        json.ends_with(concat!(
            r#""binding":{"build":{"path":"build","source":"auto-detection: ./build/CMakeCache.txt"},"#,
            r#""profile_from":"BOARD_DIR","variant":null,"board":"acme_evb/a53"},"#,
            r#""config":{"file":"~/.config/t32ctl/config.toml","host":"auto","attached":"127.0.0.1","port":20000},"#,
            r#""state_dir":"~/.local/state/t32ctl/pv/auto-20000/"}"#
        )),
        "{json}"
    );
    let one = ProfileView { hash: Some("a".repeat(64)), files: Some(1), ..profile() };
    assert_eq!(one.to_string(), "boards/acme/acme_evb/support/t32ctl/ profile=aaaaaaaa (1 file)");
    let bare = BindingView { build: None, profile_from: "T32CTL_PROFILE".into(), variant: None, board: None };
    assert_eq!(bare.to_string(), "build none, profile from T32CTL_PROFILE, variant base (no BOARD)");
    let variant =
        BindingView { variant: Some("acme_evb/a53/up".into()), board: Some("acme_evb/a53/up".into()), ..bare };
    assert_eq!(variant.to_string(), "build none, profile from T32CTL_PROFILE, variant acme_evb/a53/up");
    let defaults = ConfigView { file: None, host: "10.0.0.2".into(), attached: Some("10.0.0.2".into()), port: 20001 };
    assert_eq!(defaults.to_string(), "none (defaults): host=10.0.0.2 port=20001");
}

#[test]
fn an_error_prints_its_tag_message_and_details_and_decides_the_exit() {
    let mut r = Report::new("read status");
    r.error = Some(
        CtlError::new(ErrorKind::Unreachable, "127.0.0.1:20000: connection refused")
            .with_details(vec!["PowerView not running".into(), "tried 127.0.0.1:20000".into()]),
    );
    assert_eq!(
        r.text(),
        "error[unreachable]: 127.0.0.1:20000: connection refused\n  PowerView not running\n  tried 127.0.0.1:20000\n"
    );
    assert_eq!(r.exit(), Exit::Human);
    assert_eq!(
        r.json(),
        concat!(
            r#"{"schema":"t32ctl.report/1","command":"read status","exit":{"code":5,"class":"human"},"#,
            r#""header":null,"notices":[],"body":null,"truncated":null,"#,
            r#""error":{"kind":"unreachable","message":"127.0.0.1:20000: connection refused","#,
            r#""details":["PowerView not running","tried 127.0.0.1:20000"]},"next":[]}"#,
            "\n"
        )
    );
}

#[test]
fn hints_are_complete_commands_quoted_for_the_shell() {
    let mut r = Report::new("read proc");
    let hint = Hint(["burn", "proc", "flash", "--part", "user", "--build", "my build"].map(String::from).to_vec());
    assert_eq!(hint.render(), "t32ctl burn proc flash --part user --build 'my build'");
    r.next.push(hint);
    assert_eq!(r.text(), "next: t32ctl burn proc flash --part user --build 'my build'\n");
    assert!(r.json().ends_with("\"next\":[\"t32ctl burn proc flash --part user --build 'my build'\"]}\n"));
}

fn run_body(outcome: Outcome, cached: bool) -> RunBody {
    RunBody {
        subject: Subject::Script {
            entry: "tools/t32/a.cmm".into(),
            bundle: Bundled { hash: None, root: Some("tools/t32".into()), files: 1, cached },
        },
        result: Some(RunResult {
            outcome,
            rc: t32stage::ScriptRc::Unset,
            message: String::new(),
            error: None,
            line: None,
            after_ms: 1500,
            burn: false,
        }),
        outputs: OutputsView { dir: None, files: Vec::new(), missing: Vec::new() },
    }
}

#[test]
fn a_run_names_each_outcome_in_json_and_an_uploaded_snapshot_in_text() {
    let outcomes = [
        (Outcome::Finished, "finished", Exit::Ok),
        (Outcome::Failed, "failed", Exit::Outcome),
        (Outcome::Halted, "halted", Exit::Outcome),
        (Outcome::TimedOut { timeout_ms: 1000 }, "timed-out", Exit::Outcome),
        (Outcome::UploadStopped { flag: "--image".into(), bound_ms: 600_000 }, "upload-stopped", Exit::Outcome),
    ];
    for (outcome, name, exit) in outcomes {
        let body = run_body(outcome, true);
        assert_eq!(body.exit(), exit, "{name}");
        let v = serde_json::to_value(Body::Run(Box::new(body))).unwrap();
        assert_eq!(v["result"]["outcome"], name);
        assert_eq!(v["result"]["after_ms"], 1500);
    }
    let lines = Body::Run(Box::new(run_body(Outcome::Finished, false))).lines();
    assert_eq!(
        lines,
        ["script tools/t32/a.cmm (bundle tools/t32, 1 file, uploaded)", "result: finished after 1.5s", "outputs: none"]
    );
}
