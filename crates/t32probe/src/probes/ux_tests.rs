// SPDX-License-Identifier: Apache-2.0
//! ux-lab against canned frames.

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    registry::ProbeId,
};

const MARK: &str = "t32probe-ux-r1";

/// A build that answers every call: the message line follows each command, `AREA.LINE(A000,0)` is
/// the marker, unnamed windows are `W0nn`.
fn conversation() -> Script {
    let s = conversation_until_open();
    let s = s.expect_cmd("VERSION.SOFTWARE").reply_ok().expect_fn("WINdow.LIST()").reply_text("W002,W001");
    let s = s.expect_fn("WINdow.COMMAND(W002)").reply_text("B::VERSION.SOFTWARE");
    let s = s.expect_cmd("WinCLEAR W002").reply_ok();
    let s = s.expect_fn("PRACTICE.CoMmanD.AVAILable(SYStem.LOG.List)").reply_bool(true);
    s.expect_fn("PRACTICE.CoMmanD.AVAILable(SystemLOG.List)").reply_bool(false)
}

/// Everything up to the window the probe opens.
fn conversation_until_open() -> Script {
    let s = Script::new().attach().expect_fn("WINdow.LIST()").reply_text("W001");
    let s = s.expect_req(Request::MessageString { size: 4096 }).reply_message(0, "");
    let s = s.expect_cmd(&format!("PRINT \"{MARK}\"")).reply_ok();
    let s = s.expect_req(Request::MessageString { size: 4096 }).reply_message(1, MARK);
    let s = s.expect_fn("AREA.LINE(A000,0)").reply_text(MARK).expect_fn("AREA.LINE(A000,-1)").reply_text("");
    let s = s.expect_cmd("T32PROBE_NOPE").reply_error(90, "unknown command");
    let s = s.expect_req(Request::MessageString { size: 4096 }).reply_message(2, "unknown command");
    let s = s.expect_cmd("ERROR.RESet").reply_ok();
    let mut s = s.expect_req(Request::MessageString { size: 4096 }).reply_message(2, "unknown command");
    for w in WINDOWS {
        let ask = |offset| Request::WindowContent { command: w.to_string(), requested: 4096, offset, code: 0x41 };
        s = if *w == "Break.List" {
            s.expect_req(ask(0)).reply_error(90, "unknown command")
        } else {
            s.expect_req(ask(0)).reply_window(b"line 1\r\nline 2\r\n").expect_req(ask(16)).reply_window(b"")
        };
    }
    s.expect_fn("WINdow.LIST()").reply_text("W001")
}

#[test]
fn ux_lab_records_the_message_line_area_window_text_window_names_and_the_log_spelling() {
    let mut session = connect(vec![Ok(conversation())]);
    let mut env = test_env("ux-lab");
    let r = run(&mut env, &mut session, &[ProbeId::UxLab.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    let text = |v: &str| Some(Value::Text(v.into()));
    for (k, v) in [
        ("msg", "follows"),
        ("msg_silent", "kept"),
        ("area", "0"),
        ("wincontent", "yes"),
        ("win_left", "none"),
        ("winname", "W002"),
        ("syslog", "SYStem.LOG"),
    ] {
        assert_eq!(o.lookup(k).cloned(), text(v), "{k}");
    }
    assert_eq!(o.lookup("msg.refused").cloned(), text("kind=2 \"unknown command\""));
    assert_eq!(o.lookup("win.SYStem.state").cloned(), text("lines=2 bytes=16 first=\"line 1\""));
    assert_eq!(o.lookup("win.Break.List").cloned(), text("err90"));
    assert!(!WINDOWS.contains(&"SYStem.LOG.List"), "the system log window may turn logging on");
    assert_eq!(o.lookup("winname.command").cloned(), text("B::VERSION.SOFTWARE"));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

#[test]
fn a_new_window_that_is_not_ours_is_left_open() {
    let mut s = conversation_until_open();
    s = s.expect_cmd("VERSION.SOFTWARE").reply_ok().expect_fn("WINdow.LIST()").reply_text("W002,W001");
    s = s.expect_fn("WINdow.COMMAND(W002)").reply_text("B::Register.view");
    s = s.expect_fn("PRACTICE.CoMmanD.AVAILable(SYStem.LOG.List)").reply_bool(true);
    let s = s.expect_fn("PRACTICE.CoMmanD.AVAILable(SystemLOG.List)").reply_bool(false);
    let mut session = connect(vec![Ok(s)]);
    let mut env = test_env("ux-lab-foreign");
    let r = run(&mut env, &mut session, &[ProbeId::UxLab.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("winname").cloned(), Some(Value::Text("W002 (not ours: B::Register.view, left open)".into())));
}
