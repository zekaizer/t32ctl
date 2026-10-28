// SPDX-License-Identifier: Apache-2.0
//! The built binary: reports on stdout, nothing on stderr, the exit status by the report.

use std::{
    fs,
    io::Read,
    net::TcpListener,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// A home and a workspace with a board profile in a fresh temp directory, removed when dropped.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("t32ctl-bin-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ws/board/support/t32ctl")).expect("temp workspace");
        fs::create_dir_all(root.join("home")).expect("temp home");
        let profile = "schema = 1\n[target]\nname = \"b\"\ncpu = \"CortexA53\"\ncores = 1\nword = 8\n";
        fs::write(root.join("ws/board/support/t32ctl/profile.toml"), profile).expect("profile");
        Self(root)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(t: &Temp, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_t32ctl"));
    cmd.args(args)
        .env_clear()
        .env("HOME", t.0.join("home"))
        .env("T32CTL_PROFILE", "board/support/t32ctl")
        .current_dir(t.0.join("ws"));
    cmd
}

/// Point the config's `[connection]` at a loopback port.
fn connect_to(t: &Temp, port: u16) {
    let config = t.0.join("home/.config/t32ctl");
    fs::create_dir_all(&config).expect("config dir");
    let text = format!("schema = 1\n[connection]\nhost = \"127.0.0.1\"\nport = {port}\n");
    fs::write(config.join("config.toml"), text).expect("config");
}

fn t32ctl(t: &Temp, args: &[&str]) -> (String, i32) {
    let out = command(t, args).output().expect("t32ctl runs");
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    (String::from_utf8(out.stdout).expect("utf-8 report"), out.status.code().expect("exit status"))
}

#[test]
fn a_usage_error_is_exit_2() {
    let t = Temp::new("usage");
    let (text, code) = t32ctl(&t, &["r", "status"]);
    assert!(text.starts_with("error[usage]: unrecognized subcommand 'r'\n"), "{text}");
    assert_eq!(code, 2);
    let (text, code) = t32ctl(&t, &["read", "status", "--help"]);
    assert!(text.contains("Examples:\n  t32ctl read status\n"), "{text}");
    assert_eq!(code, 0);
}

#[test]
fn describe_needs_no_powerview() {
    let t = Temp::new("describe");
    let (text, code) = t32ctl(&t, &["read", "describe", "--format", "permissions"]);
    assert!(text.contains("\"Bash(t32ctl read:*)\""), "{text}");
    assert_eq!(code, 0);
}

#[test]
fn a_closed_port_is_unreachable_exit_5() {
    let t = Temp::new("closed");
    let port = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).expect("a free port").port();
    connect_to(&t, port);
    let (text, code) = t32ctl(&t, &["read", "status"]);
    assert!(text.starts_with(&format!("error[unreachable]: 127.0.0.1:{port}: connection refused\n")), "{text}");
    assert_eq!(code, 5);
    assert!(t.0.join(format!("home/.local/state/t32ctl/pv/127.0.0.1-{port}")).is_dir());
}

/// SIGINT, SIGTERM and SIGHUP (a closed terminal) cancel the call the same way: the report, exit 130.
#[test]
fn a_signal_during_attach_is_interrupted_exit_130() {
    for sig in ["INT", "TERM", "HUP"] {
        let t = Temp::new(&format!("sig{sig}"));
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        connect_to(&t, l.local_addr().expect("addr").port());
        let mut child = command(&t, &["read", "status"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("t32ctl starts");
        let (mut peer, _) = l.accept().expect("t32ctl connects");
        let mut attach = [0u8; 64];
        let _ = peer.read(&mut attach);
        let sent = Command::new("kill").args([&format!("-{sig}"), &child.id().to_string()]).status();
        assert!(sent.expect("kill runs").success());

        let give_up = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().expect("wait") {
                break status;
            }
            if Instant::now() > give_up {
                let _ = child.kill();
                panic!("t32ctl still runs 10 s after SIG{sig}");
            }
            thread::sleep(Duration::from_millis(20));
        };
        let mut text = String::new();
        child.stdout.take().expect("piped").read_to_string(&mut text).expect("utf-8 report");
        assert!(text.starts_with("error[interrupted]: "), "SIG{sig}: {text}");
        assert_eq!(status.code(), Some(130), "SIG{sig}: {text}");
    }
}
