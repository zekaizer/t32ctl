// SPDX-License-Identifier: Apache-2.0
//! The built binary: reports on stdout, nothing on stderr, the exit status by the report.

use std::{fs, net::TcpListener, path::PathBuf, process::Command};

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

fn t32ctl(t: &Temp, args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_t32ctl"))
        .args(args)
        .env_clear()
        .env("HOME", t.0.join("home"))
        .env("T32CTL_PROFILE", "board/support/t32ctl")
        .current_dir(t.0.join("ws"))
        .output()
        .expect("t32ctl runs");
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
    let config = t.0.join("home/.config/t32ctl");
    fs::create_dir_all(&config).expect("config dir");
    let text = format!("schema = 1\n[connection]\nhost = \"127.0.0.1\"\nport = {port}\n");
    fs::write(config.join("config.toml"), text).expect("config");
    let (text, code) = t32ctl(&t, &["read", "status"]);
    assert!(text.starts_with(&format!("error[unreachable]: 127.0.0.1:{port}: connection refused\n")), "{text}");
    assert_eq!(code, 5);
    assert!(t.0.join(format!("home/.local/state/t32ctl/pv/127.0.0.1-{port}")).is_dir());
}
