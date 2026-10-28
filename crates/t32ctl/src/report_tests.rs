// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::error::ErrorKind;

fn pv(practice: Practice, depth: i64, clients: Option<i64>) -> PvStatus {
    PvStatus { build: 168_999, base: Some(166_414), clients, practice, depth }
}

fn status(pv: PvStatus) -> Body {
    Body::Status(StatusBody { pv, binding: None, config: None, state_dir: None })
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
         pv      build=168999 clients=1 practice=idle\n"
    );
    assert_eq!(r.exit(), Exit::Ok);
    assert_eq!(
        r.json(),
        concat!(
            r#"{"schema":"t32ctl.report/1","command":"read status","exit":{"code":0,"class":"ok"},"#,
            r#""header":{"run":"stopped","core":0,"at":{"pc":"0x40083e10"}},"#,
            r#""notices":[{"text":"cleaned up after an interrupted call: 2 steps"}],"#,
            r#""body":{"kind":"status","pv":{"build":168999,"base":166414,"clients":1,"practice":"idle","depth":0}},"#,
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
    let line = |p: PvStatus| status(p).lines().join("\n");
    assert_eq!(line(pv(Practice::Halted, 1, None)), "pv      build=168999 clients=? practice=halted (depth 1)");
    assert_eq!(line(pv(Practice::Running, 2, Some(2))), "pv      build=168999 clients=2 practice=running (depth 2)");
    assert_eq!(line(pv(Practice::Other(7), 1, Some(1))), "pv      build=168999 clients=1 practice=state7 (depth 1)");
}

#[test]
fn full_status_adds_binding_config_and_state_lines_before_pv() {
    let body = Body::Status(StatusBody {
        pv: pv(Practice::Idle, 0, Some(1)),
        binding: Some(BindingView {
            build: Some(Sourced { path: "build".into(), source: "auto-detection: ./build/CMakeCache.txt".into() }),
            profile: Some(Sourced { path: "boards/acme/support/t32ctl/".into(), source: "BOARD_DIR".into() }),
            board: Some("acme_evb/a53".into()),
        }),
        config: Some(ConfigView {
            file: Some("~/.config/t32ctl/config.toml".into()),
            host: "auto".into(),
            attached: Some("127.0.0.1".into()),
            port: 20000,
        }),
        state_dir: Some("~/.local/state/t32ctl/pv/auto-20000/".into()),
    });
    assert_eq!(
        body.lines(),
        [
            "binding build=build (auto-detection: ./build/CMakeCache.txt), \
             profile=boards/acme/support/t32ctl/ (BOARD_DIR), BOARD=acme_evb/a53",
            "config  ~/.config/t32ctl/config.toml: host=auto (127.0.0.1) port=20000",
            "state   ~/.local/state/t32ctl/pv/auto-20000/",
            "pv      build=168999 clients=1 practice=idle",
        ]
    );
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        concat!(
            r#"{"kind":"status","pv":{"build":168999,"base":166414,"clients":1,"practice":"idle","depth":0},"#,
            r#""binding":{"build":{"path":"build","source":"auto-detection: ./build/CMakeCache.txt"},"#,
            r#""profile":{"path":"boards/acme/support/t32ctl/","source":"BOARD_DIR"},"board":"acme_evb/a53"},"#,
            r#""config":{"file":"~/.config/t32ctl/config.toml","host":"auto","attached":"127.0.0.1","port":20000},"#,
            r#""state_dir":"~/.local/state/t32ctl/pv/auto-20000/"}"#
        )
    );
    let bare = BindingView { build: None, profile: None, board: None };
    assert_eq!(bare.to_string(), "build none, profile none, BOARD none");
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
