// SPDX-License-Identifier: Apache-2.0
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use super::*;
use crate::{
    AccessClass, CancelReason, LostCause, MacroSpelling, RemoteError, RemoteKind, TimeoutKind, Violation,
    testkit::{ConnectFailure, Expect, ManualClock, Request, Script, ScriptedConnector},
};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn opts(clock: &ManualClock, connector: ScriptedConnector) -> ConnectOpts {
    ConnectOpts {
        budget: CallBudget::idle(ms(1000)).with_total(ms(60_000)),
        connector: Arc::new(connector),
        clock: Arc::new(clock.clone()),
        ..ConnectOpts::default()
    }
}

fn with(scripts: Vec<std::result::Result<Script, ConnectFailure>>, tweak: impl FnOnce(&mut ConnectOpts)) -> Session {
    let clock = ManualClock::new();
    let mut connector = ScriptedConnector::new(clock.clone());
    for s in scripts {
        connector = connector.push(s);
    }
    let mut o = opts(&clock, connector);
    tweak(&mut o);
    Session::connect(Endpoint::new("pv", 20000), o).expect("scripted attach")
}

fn session(script: Script) -> Session {
    with(vec![Ok(script)], |_| {})
}

#[test]
fn requests_round_trip_into_domain_values() {
    let mut s = session(
        Script::new()
            .attach()
            .expect_cmd("PRINT 1")
            .reply_ok()
            .expect_fn("STATE.RUN()")
            .reply_bool(true)
            .expect_req(Request::PracticeState)
            .reply_state(2)
            .expect_req(Request::Stop)
            .reply_ok()
            .expect_req(Request::TargetState)
            .reply_state(1),
    );
    s.cmd("PRINT 1").unwrap();
    assert_eq!(s.eval("STATE.RUN()").unwrap(), FuncValue::Bool(true));
    assert_eq!(s.practice_state().unwrap(), PracticeState::Dialog);
    s.stop_practice().unwrap();
    assert_eq!(s.target_state().unwrap(), TargetState::Halted);
    assert_eq!(s.health(), Health::Live);
}

#[test]
fn keepalives_extend_a_blocking_command_and_are_recorded() {
    let mut s = session(
        Script::new()
            .attach()
            .expect_cmd("WAIT 3.s")
            .silence(ms(600))
            .keepalive(1)
            .silence(ms(600))
            .keepalive(1)
            .silence(ms(600))
            .reply_ok(),
    );
    s.take_trace();
    s.cmd("WAIT 3.s").unwrap();
    let t = s.take_trace();
    assert_eq!(t.keepalives_total, 2);
    assert_eq!(t.keepalive_raw, vec![vec![0xFE, 0], vec![0xFE, 0]]);
    let at: Vec<_> = t.keepalives.iter().map(|k| k.since_request).collect();
    assert_eq!(at, [ms(600), ms(1200)]);
}

#[test]
fn silence_past_idle_times_out_poisons_and_sends_nothing_after() {
    let mut s = session(Script::new().attach().expect_cmd("WAIT 5.s").silence(ms(600)).silence(ms(600)));
    match s.cmd("WAIT 5.s") {
        Err(Error::Timeout { kind: TimeoutKind::Idle, waited, keepalives: 0, .. }) => assert_eq!(waited, ms(1200)),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.health(), Health::Poisoned(PoisonCause::Timeout));
    assert!(matches!(s.eval("TRUE()"), Err(Error::Poisoned(PoisonCause::Timeout))));
}

#[test]
fn keepalives_cannot_outlast_the_total_budget() {
    let mut script = Script::new().attach().expect_cmd("WAIT 9.s");
    for _ in 0..3 {
        script = script.silence(ms(600)).keepalive(1);
    }
    let mut s = session(script.silence(ms(600)));
    let mut scoped = s.with_budget(CallBudget::idle(ms(1000)).with_total(ms(2000)));
    match scoped.cmd("WAIT 9.s") {
        Err(Error::Timeout { kind: TimeoutKind::Total, keepalives: 3, .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn eof_reset_and_write_failure_lose_the_connection() {
    let cases = [
        (Script::new().attach().expect_cmd("X").eof(), LostCause::Eof),
        (Script::new().attach().expect_cmd("X").reset(), LostCause::Reset),
        (Script::new().attach().fail_write(std::io::ErrorKind::BrokenPipe), LostCause::BrokenPipe),
    ];
    for (script, want) in cases {
        let mut s = session(script);
        match s.cmd("X") {
            Err(Error::Lost { cause, .. }) => assert_eq!(cause, want),
            other => panic!("{other:?}"),
        }
        assert_eq!(s.health(), Health::Poisoned(PoisonCause::Lost));
    }
}

#[test]
fn foreign_frames_and_ids_are_protocol_violations_but_stale_ids_are_not() {
    let bad_type = [4, 0, 0, 0, 0x13, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
    let mut s = session(Script::new().attach().expect_cmd("X").raw(&bad_type, None));
    assert!(matches!(s.cmd("X"), Err(Error::Protocol { violation: Violation::BadFrameType(0x13), .. })));
    assert_eq!(s.health(), Health::Poisoned(PoisonCause::Protocol));

    let mut s = session(Script::new().attach().expect_cmd("X").reply_with_id(&[0, 77]));
    assert!(matches!(
        s.cmd("X"),
        Err(Error::Protocol { violation: Violation::OutOfSync { expected: 2, got: 77 }, .. })
    ));

    let mut s = session(
        Script::new().attach().expect_cmd("X").reply_with_id(&[0, 1]).notify(&[1, 2, 3, 4]).raw(&[], None).reply_ok(),
    );
    s.take_trace();
    s.cmd("X").unwrap();
    let t = s.take_trace();
    assert_eq!((t.stale, t.notifies, t.responses), (1, 1, 1));
}

#[test]
fn split_responses_reassemble() {
    let mut reply = vec![1, 0, 0, 0, 0x11, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
    reply[0] = 3;
    reply[10] = 9;
    let mut s = session(Script::new().attach().expect_req(Request::TargetState).raw(&reply, Some(3)));
    assert_eq!(s.target_state().unwrap(), TargetState::Other(9));
}

#[test]
fn a_deadline_cancels_a_blocked_call_within_one_slice() {
    let clock = ManualClock::new();
    let connector = ScriptedConnector::new(clock.clone()).push(Ok(Script::new()
        .attach()
        .expect_cmd("WAIT 5.s")
        .silence(ms(100))
        .silence(ms(100))
        .silence(ms(100))));
    let mut o = opts(&clock, connector);
    o.cancel = Cancel::new().with_deadline(clock.now() + ms(250));
    let mut s = Session::connect(Endpoint::new("pv", 1), o).unwrap();
    assert!(matches!(s.cmd("WAIT 5.s"), Err(Error::Cancelled { reason: CancelReason::Deadline, .. })));
    assert_eq!(s.health(), Health::Poisoned(PoisonCause::Cancelled));
}

#[test]
fn a_call_after_cancellation_sends_nothing_and_keeps_the_session_live() {
    let clock = ManualClock::new();
    let connector = ScriptedConnector::new(clock.clone()).push(Ok(Script::new().attach()));
    let mut o = opts(&clock, connector);
    let cancel = Cancel::new();
    o.cancel = cancel.clone();
    let mut s = Session::connect(Endpoint::new("pv", 1), o).unwrap();
    cancel.trigger();
    assert!(matches!(s.cmd("Go"), Err(Error::Cancelled { reason: CancelReason::Interrupt, .. })));
    assert_eq!(s.health(), Health::Live);
}

#[test]
fn revive_reconnects_once_per_poisoning() {
    let mut s = with(
        vec![
            Ok(Script::new().attach().expect_cmd("WAIT 5.s").silence(ms(1100))),
            Ok(Script::new().attach().expect_fn("TRUE()").reply_bool(true).expect_cmd("X").eof()),
            Err(ConnectFailure::Refused),
        ],
        |_| {},
    );
    assert!(s.cmd("WAIT 5.s").is_err());
    assert!(s.try_revive().unwrap());
    assert!(s.eval("TRUE()").unwrap().into_bool("TRUE()").unwrap());
    assert!(!s.try_revive().unwrap(), "a live session is not reconnected");
    assert!(s.cmd("X").is_err());
    assert!(matches!(s.try_revive(), Err(Error::Connect(ConnectError::Refused { .. }))));
    assert!(matches!(s.try_revive(), Err(Error::Poisoned(PoisonCause::ReconnectFailed))));
    assert!(matches!(s.eval("TRUE()"), Err(Error::Poisoned(PoisonCause::ReconnectFailed))));
}

#[test]
fn memory_transfers_split_at_the_chunk_boundaries() {
    for (len, chunks) in [(1usize, vec![1u16]), (7, vec![7]), (8, vec![8]), (9, vec![8, 1]), (17, vec![8, 8, 1])] {
        let mut script = Script::new().attach();
        let mut addr = 0x100;
        for n in &chunks {
            script = script
                .expect_req(Request::MemRead { access: "VM".into(), addr, len: *n })
                .reply_mem(&vec![0xAB; *n as usize]);
            addr += u64::from(*n);
        }
        let mut s = with(vec![Ok(script)], |o| o.tuning = Tuning::new(8, 8).unwrap());
        assert_eq!(s.mem_read(&MemAddr::vm(0x100), len).unwrap(), vec![0xAB; len], "len {len}");
    }
    let data: Vec<u8> = (0..9).collect();
    let mut s = with(
        vec![Ok(Script::new()
            .attach()
            .expect_req(Request::MemWrite { access: "VM".into(), addr: 0x200, data: data[..8].to_vec() })
            .reply_ok()
            .expect_req(Request::MemWrite { access: "VM".into(), addr: 0x208, data: data[8..].to_vec() })
            .reply_ok())],
        |o| o.tuning = Tuning::new(8, 8).unwrap(),
    );
    s.mem_write(&MemAddr::vm(0x200), &data).unwrap();
}

#[test]
fn a_read_with_an_access_width_keeps_it_on_every_chunk_and_cuts_chunks_at_whole_accesses() {
    let a = |addr| Request::MemReadWidth { access: "A".into(), addr, len: 8, width: 4 };
    let script = Script::new()
        .attach()
        .expect_req(a(0x0900_0000))
        .reply_mem(&[1; 8])
        .expect_req(a(0x0900_0008))
        .reply_mem(&[2; 8]);
    let mut s = with(vec![Ok(script)], |o| o.tuning = Tuning::new(10, 8).unwrap());
    let at = MemAddr::new(AccessClass::parse("A").unwrap(), 0x0900_0000).with_width(4);
    assert_eq!(s.mem_read(&at, 16).unwrap(), [[1; 8], [2; 8]].concat());
    assert_eq!(at.offset(8).map(|m| m.width), Some(Some(4)));
}

#[test]
fn remote_refusals_keep_the_session_live() {
    let mut s = session(
        Script::new()
            .attach()
            .expect_cmd("NOPE")
            .reply_error(90, "unknown command")
            .expect_fn("NOPE()")
            .reply_function(0, "syntax error")
            .expect(Expect::Any)
            .reply_error(16, "")
            .expect_req(Request::MacroGet { name: "&m".into() })
            .reply_exp_error(3, "no such macro"),
    );
    let kind = |r: Result<()>| r.unwrap_err().remote().map(|e| e.kind);
    assert_eq!(kind(s.cmd("NOPE")), Some(RemoteKind::CommandFailed));
    assert_eq!(kind(s.eval("NOPE()").map(drop)), Some(RemoteKind::FunctionFailed));
    assert_eq!(kind(s.mem_read(&MemAddr::vm(0), 4).map(drop)), Some(RemoteKind::MemoryAccess));
    let e = s.macro_get(&MacroName::new("m").unwrap()).unwrap_err();
    assert_eq!(
        e.remote().map(|r| (r.kind, r.message.clone())),
        Some((RemoteKind::ExpFailed, Some("no such macro (EXP code 3)".into())))
    );
    assert_eq!(s.health(), Health::Live);
}

#[test]
fn stop_practice_returns_status_1_as_ok() {
    let mut s = session(
        Script::new()
            .attach()
            .expect_req(Request::Stop)
            .reply_error(1, "")
            .expect_req(Request::Stop)
            .reply_ok()
            .expect_req(Request::Stop)
            .reply_error(2, ""),
    );
    assert_eq!(s.stop_practice().unwrap(), 1);
    assert_eq!(s.stop_practice().unwrap(), 0);
    assert_eq!(s.stop_practice().unwrap_err().remote().map(|e| e.code), Some(2));
    assert_eq!(s.health(), Health::Live);
}

#[test]
fn std_failed_on_a_memory_access_is_a_memory_access_refusal() {
    let mut s = session(
        Script::new()
            .attach()
            .expect(Expect::Any)
            .reply_error(113, "")
            .expect(Expect::Any)
            .reply_error(113, "")
            .expect_cmd("NOPE")
            .reply_error(113, ""),
    );
    let kind = |r: Result<()>| r.unwrap_err().remote().map(|e| e.kind);
    assert_eq!(kind(s.mem_read(&MemAddr::vm(0), 4).map(drop)), Some(RemoteKind::MemoryAccess));
    assert_eq!(kind(s.mem_write(&MemAddr::vm(0), &[1])), Some(RemoteKind::MemoryAccess));
    assert_eq!(kind(s.cmd("NOPE")), Some(RemoteKind::Other));
}

#[test]
fn a_failed_exp_call_with_a_status_byte_keeps_its_exp_code_and_message() {
    let mut s = session(
        Script::new()
            .attach()
            .expect_req(Request::MacroGet { name: "&m".into() })
            .reply_exp_failed(90, 3, "no such macro")
            .expect_req(Request::MacroSet { name: "&m".into(), value: "v".into() })
            .reply_exp_failed(90, 7, "locked"),
    );
    let got = |r: Result<()>| r.unwrap_err().remote().cloned();
    let want = |op, code, text: &str| RemoteError { op, kind: RemoteKind::ExpFailed, code, message: Some(text.into()) };
    let m = MacroName::new("m").unwrap();
    assert_eq!(got(s.macro_get(&m).map(drop)), Some(want(Op::MacroGet, 90, "no such macro (EXP code 3)")));
    assert_eq!(got(s.macro_set(&m, "v")), Some(want(Op::MacroSet, 90, "locked (EXP code 7)")));
    assert_eq!(s.health(), Health::Live);
}

#[test]
fn a_reply_carrying_the_largest_macro_value_is_received() {
    let value = "v".repeat(60_000);
    let mut s = session(
        Script::new()
            .attach()
            .expect_req(Request::MacroSet { name: "&m".into(), value: value.clone() })
            .reply_macro("")
            .expect_req(Request::MacroGet { name: "&m".into() })
            .reply_macro(&value),
    );
    let m = MacroName::new("m").unwrap();
    s.macro_set(&m, &value).unwrap();
    assert_eq!(s.macro_get(&m).unwrap(), value);
}

#[test]
fn macro_names_follow_the_tuned_spelling() {
    let mut s = with(
        vec![Ok(Script::new()
            .attach()
            .expect_req(Request::MacroSet { name: "&x".into(), value: "1".into() })
            .reply_macro("")
            .expect_req(Request::MacroGet { name: "&x".into() })
            .reply_macro("1"))],
        |o| o.tuning = Tuning::default().with_macro_spelling(MacroSpelling::Ampersand),
    );
    let x = MacroName::new("x").unwrap();
    s.macro_set(&x, "1").unwrap();
    assert_eq!(s.macro_get(&x).unwrap(), "1");
}

#[test]
fn eval_string_and_the_device_prompt() {
    let mut s = with(
        vec![Ok(Script::new()
            .attach()
            .expect_cmd("B::EVAL ERROR.MESSAGE()")
            .reply_ok()
            .expect_req(Request::EvalString)
            .reply_string("boom"))],
        |o| o.prompt = Some(DevicePrompt::parse("B::").unwrap()),
    );
    assert_eq!(s.eval_string("ERROR.MESSAGE()").unwrap(), "boom");
}

#[test]
fn eval_last_string_reads_the_slot_without_eval() {
    let mut s = session(Script::new().attach().expect_req(Request::EvalString).reply_string("t32q-ok"));
    assert_eq!(s.eval_last_string().unwrap(), "t32q-ok");
}

#[test]
fn macro_get_payload_returns_the_raw_pl_bytes() {
    let mut s =
        session(Script::new().attach().expect_req(Request::MacroGet { name: "&m".into() }).reply_macro("\"0\""));
    assert_eq!(s.macro_get_payload(&MacroName::new("m").unwrap()).unwrap(), b"\0\0\"0\"");
}

#[test]
fn build_falls_back_to_the_version_spelling_and_is_cached() {
    let mut s = session(
        Script::new()
            .attach()
            .expect_fn("SOFTWARE.BUILD()")
            .reply_function(0, "unknown function")
            .expect_fn("VERSION.BUILD()")
            .reply_decimal(172_803)
            .expect_fn("SOFTWARE.BUILD.BASE()")
            .reply_decimal(172_138),
    );
    let want = PowerViewBuild { build: 172_803, base: Some(172_138) };
    assert_eq!(s.build().unwrap(), want);
    assert_eq!(s.build().unwrap(), want);
}

#[test]
fn attach_failures_are_classified() {
    let clock = ManualClock::new();
    let rejected = ScriptedConnector::new(clock.clone())
        .push(Ok(Script::new().expect_req(Request::Attach).reply_error(60, "license")));
    let e = Session::connect(Endpoint::new("pv", 1), opts(&clock, rejected)).unwrap_err();
    assert_eq!(e.tag(), "attach-rejected");

    let silent = ScriptedConnector::new(clock.clone())
        .push(Ok(Script::new().expect_req(Request::Attach).silence(ms(3000)).silence(ms(3000))));
    let e = Session::connect(Endpoint::new("pv", 1), opts(&clock, silent)).unwrap_err();
    assert_eq!(e.tag(), "no-answer");

    let refused = ScriptedConnector::new(clock.clone()).push(Err(ConnectFailure::Refused));
    let e = Session::connect(Endpoint::new("pv", 1), opts(&clock, refused)).unwrap_err();
    assert_eq!(e.tag(), "refused");
}

/// Real loopback sockets used as byte sources, not servers with RCL semantics.
#[test]
fn tcp_connect_classifies_closed_silent_foreign_and_closing_peers() {
    let fast = |port| {
        let o = ConnectOpts { attach_idle: ms(300), connect_timeout: ms(1000), ..ConnectOpts::default() };
        Session::connect(Endpoint::new("127.0.0.1", port), o).map(drop).unwrap_err()
    };
    // `hold`: the peer keeps the connection open until the client has returned.
    let serve = |reply: Option<&'static [u8]>, hold: bool| {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (release, held) = mpsc::channel::<()>();
        let h = thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 64];
            let _ = c.read(&mut buf);
            if let Some(r) = reply {
                let _ = c.write_all(r);
            }
            if hold {
                let _ = held.recv();
            }
        });
        (port, move || {
            let _ = release.send(());
            h.join().unwrap();
        })
    };

    let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert_eq!(fast(closed).tag(), "refused");

    let (port, done) = serve(None, true);
    assert_eq!(fast(port).tag(), "no-answer");
    done();

    let (port, done) = serve(Some(b"HTTP/1.1 400 Bad Request\r\n\r\n"), true);
    match fast(port) {
        ConnectError::NotRcl { evidence: NotRclEvidence::Banner(b), .. } => assert!(b.starts_with("HTTP/1.1 400")),
        other => panic!("{other:?}"),
    }
    done();

    let (port, done) = serve(None, false);
    assert_eq!(fast(port).tag(), "closed-during-attach");
    done();
}

#[test]
fn the_message_line_comes_back_with_its_type() {
    let s =
        Script::new().attach().expect_req(Request::MessageString { size: 4096 }).reply_message(2, "unknown command");
    let mut session = session(s);
    assert_eq!(session.message().unwrap(), MessageLine { kind: 2, text: "unknown command".into() });
}

#[test]
fn window_content_is_read_in_chunks_until_an_empty_one() {
    let ask = |offset| Request::WindowContent { command: "AREA.view".into(), requested: 4096, offset, code: 0x41 };
    let s = Script::new().attach();
    let s = s.expect_req(ask(0)).reply_window(b"line 1\r\n").expect_req(ask(8)).reply_window(b"line 2\r\n");
    let s = s.expect_req(ask(16)).reply_window(b"");
    let mut session = session(s);
    let (text, cut) = session.window_content("AREA.view", PrintFormat::Ascii, 1 << 20).unwrap();
    assert_eq!((text.as_slice(), cut), (&b"line 1\r\nline 2\r\n"[..], false));
}

#[test]
fn window_content_stops_at_its_bound_and_says_so() {
    let ask = |offset| Request::WindowContent { command: "List".into(), requested: 4096, offset, code: 0x41 };
    let s = Script::new().attach().expect_req(ask(0)).reply_window(b"0123456789");
    let mut session = session(s);
    let (text, cut) = session.window_content("List", PrintFormat::Ascii, 6).unwrap();
    assert_eq!((text.as_slice(), cut), (&b"012345"[..], true));
}
