// SPDX-License-Identifier: Apache-2.0
use std::time::Duration;

use t32rcl::{Endpoint, LostCause, Op, PoisonCause, RemoteError, TimeoutKind, Violation};

use super::*;

/// t32rcl's status table: code and the `RemoteKind` it gives a command.
const STATUS: &[(u8, RemoteKind)] = &[
    (2, RemoteKind::TargetState),
    (3, RemoteKind::TargetState),
    (4, RemoteKind::TargetState),
    (6, RemoteKind::MemoryAccess),
    (10, RemoteKind::Other),
    (14, RemoteKind::Other),
    (15, RemoteKind::MemoryAccess),
    (16, RemoteKind::MemoryAccess),
    (22, RemoteKind::MemoryAccess),
    (48, RemoteKind::TargetState),
    (49, RemoteKind::Other),
    (57, RemoteKind::TargetState),
    (60, RemoteKind::Attach),
    (64, RemoteKind::TargetState),
    (67, RemoteKind::Other),
    (83, RemoteKind::Attach),
    (90, RemoteKind::CommandFailed),
    (91, RemoteKind::Other),
    (92, RemoteKind::Other),
    (93, RemoteKind::Other),
    (113, RemoteKind::Other),
    (123, RemoteKind::Locked),
    (128, RemoteKind::TargetState),
    (140, RemoteKind::TargetState),
    (144, RemoteKind::TargetState),
    (147, RemoteKind::Attach),
    (161, RemoteKind::TargetState),
    (162, RemoteKind::TargetState),
    (164, RemoteKind::TargetState),
    (200, RemoteKind::Other),
    (254, RemoteKind::Attach),
    (255, RemoteKind::Other),
];

fn remote(code: u8, kind: RemoteKind) -> Error {
    Error::Remote(RemoteError { op: Op::Command, kind, code, message: Some("why".into()) })
}

fn tag(e: &Error, context: Context) -> &'static str {
    from_rcl(e, context, "X").kind.tag()
}

#[test]
fn every_status_code_has_one_tag_by_code_and_context() {
    for &(code, kind) in STATUS {
        let e = remote(code, kind);
        let (read, stage) = match code {
            2 => ("not-halted", "not-halted"),
            3 | 4 | 48 | 64 => ("target-down", "target-down"),
            123 => ("rcl-locked", "rcl-locked"),
            57 | 128 | 140 | 144 | 161 | 162 | 164 => ("target-power", "target-power"),
            60 | 83 | 147 | 254 => ("unreachable", "unreachable"),
            _ => ("refused", "pv-error"),
        };
        assert_eq!(tag(&e, Context::Read), read, "code {code}");
        assert_eq!(tag(&e, Context::Text), read, "code {code}");
        assert_eq!(tag(&e, Context::Stage), stage, "code {code}");
    }
    let failed_function = Error::Remote(RemoteError {
        op: Op::Function,
        kind: RemoteKind::FunctionFailed,
        code: 0,
        message: Some("unknown function".into()),
    });
    assert_eq!(tag(&failed_function, Context::Read), "refused");
    assert_eq!(tag(&failed_function, Context::Stage), "pv-error");
}

#[test]
fn a_target_state_code_names_its_status_in_the_message() {
    let e = from_rcl(&remote(3, RemoteKind::TargetState), Context::Text, "Register(PC)");
    assert_eq!(e.kind, ErrorKind::TargetDown);
    assert_eq!(e.message, "Register(PC): command: PowerView refused: target state (code 3 STD_NOTRUNNING): why");
    assert_eq!(e.exit(), Exit::State);
}

#[test]
fn transport_failures_are_classified_by_variant() {
    let op = Op::Function;
    let timeout = Error::Timeout { op, kind: TimeoutKind::Idle, waited: Duration::from_secs(10), keepalives: 0 };
    assert_eq!(tag(&timeout, Context::Read), "no-answer");
    for e in [
        Error::Lost { op, cause: LostCause::Eof },
        Error::Protocol { op, violation: Violation::BadFrameType(7) },
        Error::Poisoned(PoisonCause::ReconnectFailed),
    ] {
        assert_eq!(tag(&e, Context::Read), "connection-lost", "{e}");
    }
    assert_eq!(tag(&Error::Cancelled { op, reason: CancelReason::Interrupt }, Context::Stage), "interrupted");
    let unexpected = Error::UnexpectedResult { op, wanted: "int", got: "text".into() };
    assert_eq!(tag(&unexpected, Context::Read), "pv-error");
}

#[test]
fn local_text_errors_are_notation_only_for_typed_text() {
    let long = Error::Local(LocalError::TextTooLong { len: 5000, max: 4096 });
    assert_eq!(tag(&long, Context::Text), "notation");
    assert_eq!(tag(&Error::Local(LocalError::EmbeddedNul), Context::Text), "notation");
    assert_eq!(tag(&long, Context::Stage), "pv-error");
    assert_eq!(tag(&Error::Local(LocalError::AddressOverflow), Context::Text), "pv-error");
}

#[test]
fn connect_failures_name_what_to_check() {
    let endpoint = Endpoint::new("127.0.0.1", 20000);
    let refused = from_connect(&ConnectError::Refused { endpoint: endpoint.clone() });
    assert_eq!(refused.kind, ErrorKind::Unreachable);
    assert_eq!(refused.message, "127.0.0.1:20000: connection refused");
    assert_eq!(
        refused.details,
        ["PowerView not running, RCL not enabled (RCL=NETTCP in config.t32), or wrong [connection] port in \
          t32ctl's config.toml"]
    );
    let dropped = from_connect(&ConnectError::TimedOut { endpoint: endpoint.clone(), after: Duration::from_secs(3) });
    assert_eq!(
        dropped.details,
        ["wrong [connection] host in t32ctl's config.toml for this WSL networking mode, or a firewall drops the \
          port"]
    );
    let silent = ConnectError::NoAnswer { endpoint: endpoint.clone(), waited: Duration::from_secs(5) };
    assert_eq!(from_connect(&silent).kind, ErrorKind::NoAnswer);
    let stopped = ConnectError::Cancelled { endpoint: endpoint.clone(), reason: CancelReason::Interrupt };
    assert_eq!(from_connect(&stopped).kind, ErrorKind::Interrupted);
    assert_eq!(tag(&Error::Connect(silent), Context::Read), "no-answer");
}

#[test]
fn the_message_leads_with_the_request() {
    let e = from_rcl(&remote(90, RemoteKind::CommandFailed), Context::Text, "PRINT x");
    assert_eq!(e.message, "PRINT x: command: PowerView refused: command failed (code 90 FN1): why");
    assert_eq!(e.to_string(), "error[refused]: PRINT x: command: PowerView refused: command failed (code 90 FN1): why");
    assert_eq!(e.exit(), Exit::Fix);
}

#[test]
fn every_tag_maps_to_its_exit_and_every_exit_has_a_code_and_a_next_step() {
    let exits: Vec<(&str, u8)> = ErrorKind::ALL.iter().map(|k| (k.tag(), k.exit().code())).collect();
    assert_eq!(
        exits,
        [
            ("usage", 2),
            ("notation", 2),
            ("memory-policy", 2),
            ("refused", 2),
            ("grade-mismatch", 2),
            ("unknown-proc", 2),
            ("proc-params", 2),
            ("bad-bundle", 2),
            ("file-exists", 2),
            ("not-halted", 3),
            ("target-down", 3),
            ("pv-script-running", 4),
            ("vm-busy", 4),
            ("rcl-locked", 4),
            ("unreachable", 5),
            ("no-answer", 5),
            ("dialog", 5),
            ("script-vanished", 5),
            ("pv-script-halted", 5),
            ("cpu-mismatch", 5),
            ("profile-invalid", 5),
            ("no-profile", 5),
            ("target-power", 5),
            ("connection-lost", 5),
            ("pv-error", 5),
            ("residue", 5),
            ("state-dir", 5),
            ("interrupted", 130),
        ]
    );
    let codes: Vec<(u8, &str)> = Exit::ALL.iter().map(|e| (e.code(), e.class())).collect();
    assert_eq!(
        codes,
        [(0, "ok"), (1, "outcome"), (2, "fix"), (3, "state"), (4, "wait"), (5, "human"), (130, "interrupted")]
    );
    assert!(Exit::ALL.iter().all(|e| e.next().ends_with('.')));
    assert!(ErrorKind::ALL.iter().all(|k| k.meaning().ends_with('.')));
    let notation: CtlError = crate::notation::NotationError("size \"x\": use bytes".into()).into();
    assert_eq!(notation, CtlError::new(ErrorKind::Notation, "size \"x\": use bytes"));
}

#[test]
fn every_staging_failure_has_one_tag() {
    use t32stage::{ArgError, BundleError, LeaseError, PathError, StageError, VmError};
    let io = || std::io::Error::other("disk full");
    let cases: Vec<(StageError, &str)> = vec![
        (StageError::Lease(LeaseError::Busy { holder: Some("t32probe (pid 7)".into()) }), "vm-busy"),
        (StageError::Lease(LeaseError::Io { path: "l".into(), source: io() }), "state-dir"),
        (StageError::Journal { path: "j".into(), source: io() }, "state-dir"),
        (StageError::Busy { depth: -1 }, "pv-script-running"),
        (StageError::Busy { depth: 2 }, "pv-script-halted"),
        (StageError::UnwindFailed { depth: 1 }, "residue"),
        (StageError::Arg(ArgError::TooLong { len: 5000, max: 4096 }), "notation"),
        (StageError::Bundle(BundleError::TooLarge { files: 600, bytes: 1 }), "profile-invalid"),
        (StageError::Source(io()), "proc-params"),
        (StageError::Path(PathError::Char { path: "C:\\김".into(), bad: '김' }), "pv-error"),
        (StageError::Vm(VmError::WindowTooSmall { need: 2, have: 1 }), "pv-error"),
        (StageError::SizeMismatch { what: "x".into(), want: 2, got: 1 }, "pv-error"),
        (StageError::BadReply { what: "OS.FILE.SIZE", text: "?".into() }, "pv-error"),
        (StageError::StillPresent { what: "x".into() }, "pv-error"),
        (StageError::UnpackIncomplete { key: "k".into(), detail: "marker missing".into() }, "pv-error"),
        (StageError::Gzip(io()), "pv-error"),
        (StageError::Rcl(remote(123, RemoteKind::Locked)), "rcl-locked"),
        (StageError::Rcl(remote(90, RemoteKind::CommandFailed)), "pv-error"),
        (StageError::Rcl(Error::Cancelled { op: Op::Command, reason: CancelReason::Interrupt }), "interrupted"),
    ];
    for (e, want) in cases {
        assert_eq!(from_stage(&e, "the run").kind.tag(), want, "{e}");
    }
    let busy = from_stage(&StageError::Lease(LeaseError::Busy { holder: Some("t32probe (pid 7)".into()) }), "x");
    assert_eq!(busy.message, "x: VM: window is leased by another process (t32probe (pid 7))");
    let root = from_stage(&StageError::Path(PathError::Char { path: "C:\\김".into(), bad: '김' }), "opening the stage");
    assert!(root.message.contains("[staging] root"), "{}", root.message);
    let window = from_stage(&StageError::Vm(VmError::WindowTooSmall { need: 2, have: 1 }), "the upload");
    assert!(window.message.contains("[staging] vm_len"), "{}", window.message);
}
