// SPDX-License-Identifier: Apache-2.0
//! Socket smoke tests of the real binary. Loopback listeners are byte sources, not servers
//! with RCL semantics. CI runs these against the musl release binary through `T32PROBE_BIN`.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

/// `T32PROBE_BIN` may be relative to the workspace root; cargo runs tests in the crate directory.
fn bin() -> PathBuf {
    match std::env::var_os("T32PROBE_BIN") {
        Some(p) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(p),
        None => PathBuf::from(env!("CARGO_BIN_EXE_t32probe")),
    }
}

fn probe(port: u16, tag: &str) -> Output {
    let state = std::env::temp_dir().join(format!("t32probe-smoke-{tag}-{}", std::process::id()));
    let out = Command::new(bin())
        .args(["--host", "127.0.0.1", "--port", &port.to_string(), "--only", "connect", "--timeout", "0.5"])
        .arg("--state-dir")
        .arg(&state)
        .output()
        .expect("t32probe runs");
    let _ = std::fs::remove_dir_all(&state);
    out
}

fn connect_line(out: &Output) -> String {
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .find(|l| l.starts_with("connect "))
        .unwrap_or_else(|| panic!("no connect line in:\n{text}"))
        .to_string()
}

fn serve(reply: Option<&'static [u8]>, linger: Duration) -> (u16, thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    let h = thread::spawn(move || {
        let (mut c, _) = l.accept().expect("accept");
        let mut buf = [0u8; 64];
        let _ = c.read(&mut buf);
        if let Some(r) = reply {
            let _ = c.write_all(r);
        }
        thread::sleep(linger);
    });
    (port, h)
}

#[test]
fn list_prints_the_catalogue() {
    let out = Command::new(bin()).arg("--list").output().expect("runs");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for id in ["connect", "tar-download", "dialog", "elf-unzip"] {
        assert!(text.contains(id), "{id} missing");
    }
}

#[test]
fn a_closed_port_is_refused_and_exits_1() {
    let port = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr").port();
    let out = probe(port, "closed");
    assert!(connect_line(&out).contains("class=refused"), "{}", connect_line(&out));
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("hint: PowerView not running"));
}

#[test]
fn a_silent_listener_is_no_answer() {
    let (port, h) = serve(None, Duration::from_secs(2));
    let out = probe(port, "silent");
    assert!(connect_line(&out).contains("class=no-answer"), "{}", connect_line(&out));
    assert_eq!(out.status.code(), Some(1));
    h.join().expect("listener");
}

#[test]
fn an_http_responder_is_not_rcl() {
    let (port, h) = serve(Some(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n"), Duration::from_millis(300));
    let out = probe(port, "http");
    let line = connect_line(&out);
    assert!(line.contains("class=not-rcl"), "{line}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("HTTP/1.1"), "the banner is shown");
    h.join().expect("listener");
}

#[test]
fn an_opt_in_probe_without_its_flag_is_a_usage_error() {
    let out = Command::new(bin()).args(["--only", "dialog"]).output().expect("runs");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("dialog needs --allow-dialog"));
}

/// `--only <probe>` without its flag: exit 2 and the flag named.
fn needs_gate(probe: &str, flag: &str) {
    let out = Command::new(bin()).args(["--only", probe]).output().expect("runs");
    assert_eq!(out.status.code(), Some(2), "{probe}");
    assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("{probe} needs {flag}")), "{probe}");
}

#[test]
fn target_read_and_load_code_need_their_gates() {
    needs_gate("target-read", "--allow-target-read --symbol <s>");
}

#[test]
fn sigint_during_attach_prints_the_partial_report_and_exits_130() {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    let state = std::env::temp_dir().join(format!("t32probe-smoke-sigint-{}", std::process::id()));
    let mut child = Command::new(bin())
        .args(["--host", "127.0.0.1", "--port", &port.to_string(), "--only", "connect", "--timeout", "30"])
        .arg("--state-dir")
        .arg(&state)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("t32probe starts");
    let (mut peer, _) = l.accept().expect("t32probe connects");
    let mut attach = [0u8; 64];
    let _ = peer.read(&mut attach);
    let sent = Command::new("kill").args(["-INT", &child.id().to_string()]).status().expect("kill runs");
    assert!(sent.success());

    let give_up = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > give_up {
            let _ = child.kill();
            panic!("t32probe still runs 10 s after SIGINT");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut text = String::new();
    child.stdout.take().expect("piped").read_to_string(&mut text).expect("utf-8 report");
    let _ = std::fs::remove_dir_all(&state);
    assert_eq!(status.code(), Some(130), "{text}");
    assert!(text.lines().any(|l| l.starts_with("connect      INTR")), "{text}");
    assert!(text.lines().last().is_some_and(|l| l.starts_with("summary:") && l.ends_with("exit=130")), "{text}");
}
