// SPDX-License-Identifier: Apache-2.0
//! Typed errors. One enum for callers, grouped by what the caller must do next.
//!
//! Contract: after any error whose [`Error::class`] is [`ErrorClass::ConnectionFatal`] the
//! session is poisoned, because a late response would be matched to the next request. Every later
//! call returns [`Error::Poisoned`] without sending until `Session::try_revive` succeeds.

use std::{error::Error as StdError, fmt, io, time::Duration};

use crate::value::Endpoint;

/// The RCL request an error belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Op {
    Attach,
    Command,
    Function,
    EvalString,
    PracticeState,
    Stop,
    TargetState,
    MemRead,
    MemWrite,
    MacroGet,
    MacroSet,
    Message,
    WindowContent,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Op::Attach => "attach",
            Op::Command => "command",
            Op::Function => "function",
            Op::EvalString => "eval-string",
            Op::PracticeState => "practice-state",
            Op::Stop => "stop",
            Op::TargetState => "target-state",
            Op::MemRead => "mem-read",
            Op::MemWrite => "mem-write",
            Op::MacroGet => "macro-get",
            Op::MacroSet => "macro-set",
            Op::Message => "message",
            Op::WindowContent => "window-content",
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Wire-level misbehavior of the peer. Never built from a PowerView status code.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Violation {
    BadFrameType(u32),
    FrameTooLarge(u32),
    OutOfSync { expected: u8, got: u8 },
    ShortResponse { wanted: usize, got: usize },
    ExpMalformed(&'static str),
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::BadFrameType(t) => write!(f, "NETTCP frame type {t:#x} is neither RESP nor NOTIFY"),
            Violation::FrameTooLarge(n) => {
                write!(f, "NETTCP frame length {n} exceeds the {} byte limit", crate::frame::MAX_PAYLOAD)
            }
            Violation::OutOfSync { expected, got } => {
                write!(f, "response id {got} does not answer request id {expected}")
            }
            Violation::ShortResponse { wanted, got } => {
                write!(f, "response too short: wanted {wanted} bytes, got {got}")
            }
            Violation::ExpMalformed(what) => write!(f, "malformed EXP reply: {what}"),
        }
    }
}

impl std::error::Error for Violation {}

/// PowerView understood the request and refused it. The connection stays usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteError {
    pub op: Op,
    pub kind: RemoteKind,
    /// Status byte `[0]` of the response: 0 for a failed function, and for an EXP call that
    /// failed with status 0.
    pub code: u8,
    /// Text after the u32 length at `[6..10]`, the failed function's text, or the EXP `EM` block.
    pub message: Option<String>,
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: PowerView refused: {} (code {}", self.op, self.kind, self.code)?;
        if let Some(name) = crate::wire::status_name(self.code) {
            write!(f, " {name}")?;
        }
        f.write_str(")")?;
        if let Some(m) = &self.message {
            write!(f, ": {m}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RemoteError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoteKind {
    /// Status 90 on ExecuteCommand: PRACTICE rejected the command text.
    CommandFailed,
    /// Status 90 on ExecuteFunction, or result type 0.
    FunctionFailed,
    /// Status 90 or 113 on a memory object access, or 6/15/16/22.
    MemoryAccess,
    /// Running, not running, reset, core not active, debug port failures.
    TargetState,
    /// 123: another client holds the API lock.
    Locked,
    /// 60 invalid license, 83 no Remote API, 147 no device, 254 attach missing.
    Attach,
    /// An EXP call failed: no `PL` block, or status 90. `EC`/`EM` carry the reason.
    ExpFailed,
    Other,
}

impl fmt::Display for RemoteKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RemoteKind::CommandFailed => "command failed",
            RemoteKind::FunctionFailed => "function failed",
            RemoteKind::MemoryAccess => "memory access failed",
            RemoteKind::TargetState => "target state",
            RemoteKind::Locked => "api locked",
            RemoteKind::Attach => "attach problem",
            RemoteKind::ExpFailed => "extended call failed",
            RemoteKind::Other => "error",
        })
    }
}

/// Failures detected before anything is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalError {
    TextTooLong { len: usize, max: usize },
    EmbeddedNul,
    BadMacroName(String),
    BadAccessClass(String),
    BadPrompt(String),
    BadChunk { got: usize, min: usize, max: usize },
    ValueTooLarge { len: usize, max: usize },
    AddressOverflow,
}

impl fmt::Display for LocalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalError::TextTooLong { len, max } => write!(f, "PRACTICE text is {len} bytes, limit is {max}"),
            LocalError::EmbeddedNul => f.write_str("text contains a NUL byte"),
            LocalError::BadMacroName(n) => write!(f, "invalid macro name {n:?}"),
            LocalError::BadAccessClass(a) => write!(f, "invalid access class {a:?}"),
            LocalError::BadPrompt(p) => write!(f, "invalid device prompt {p:?}"),
            LocalError::BadChunk { got, min, max } => write!(f, "chunk size {got} outside {min}..={max}"),
            LocalError::ValueTooLarge { len, max } => write!(f, "value of {len} bytes exceeds the {max} byte limit"),
            LocalError::AddressOverflow => f.write_str("address range wraps past the end of the address space"),
        }
    }
}

impl std::error::Error for LocalError {}

/// What the caller must do about an error. Exit codes and probe policy key off this only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// Connection unusable. Revive before the next call.
    ConnectionFatal,
    /// PowerView understood the request and refused it. The connection is intact.
    Rejected,
    /// Nothing was sent: argument validation failed.
    Usage,
    /// Initial connect or attach failed.
    Unreachable,
    /// Ctrl-C or a deadline.
    Cancelled,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Connect(ConnectError),
    Timeout {
        op: Op,
        kind: TimeoutKind,
        waited: Duration,
        keepalives: u32,
    },
    Lost {
        op: Op,
        cause: LostCause,
    },
    Protocol {
        op: Op,
        violation: Violation,
    },
    Remote(RemoteError),
    /// PowerView answered with a result type the caller did not ask for.
    UnexpectedResult {
        op: Op,
        wanted: &'static str,
        got: String,
    },
    Poisoned(PoisonCause),
    Cancelled {
        op: Op,
        reason: CancelReason,
    },
    Local(LocalError),
}

impl Error {
    pub fn class(&self) -> ErrorClass {
        match self {
            Error::Connect(_) => ErrorClass::Unreachable,
            Error::Timeout { .. } | Error::Lost { .. } | Error::Protocol { .. } | Error::Poisoned(_) => {
                ErrorClass::ConnectionFatal
            }
            Error::Remote(_) | Error::UnexpectedResult { .. } => ErrorClass::Rejected,
            Error::Cancelled { .. } => ErrorClass::Cancelled,
            Error::Local(_) => ErrorClass::Usage,
        }
    }

    /// Stable lowercase tag for reports and JSON.
    pub fn tag(&self) -> &'static str {
        match self {
            Error::Connect(e) => e.tag(),
            Error::Timeout { .. } => "timeout",
            Error::Lost { .. } => "lost",
            Error::Protocol { .. } => "protocol",
            Error::Remote(_) => "remote",
            Error::UnexpectedResult { .. } => "unexpected-result",
            Error::Poisoned(_) => "poisoned",
            Error::Cancelled { .. } => "cancelled",
            Error::Local(_) => "usage",
        }
    }

    /// The remote refusal, if this is one.
    pub fn remote(&self) -> Option<&RemoteError> {
        match self {
            Error::Remote(r) => Some(r),
            _ => None,
        }
    }

    pub(crate) fn poison_cause(&self) -> Option<PoisonCause> {
        match self {
            Error::Timeout { .. } => Some(PoisonCause::Timeout),
            Error::Lost { .. } => Some(PoisonCause::Lost),
            Error::Protocol { .. } => Some(PoisonCause::Protocol),
            Error::Cancelled { .. } => Some(PoisonCause::Cancelled),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Connect(e) => e.fmt(f),
            Error::Timeout { op, kind, waited, keepalives } => {
                write!(f, "{op}: no answer for {waited:.2?} ({kind} timeout, {keepalives} keepalive frames seen)")
            }
            Error::Lost { op, cause } => write!(f, "{op}: connection lost ({cause})"),
            Error::Protocol { op, violation } => write!(f, "{op}: protocol violation: {violation}"),
            Error::Remote(e) => e.fmt(f),
            Error::UnexpectedResult { op, wanted, got } => write!(f, "{op}: wanted {wanted}, got {got}"),
            Error::Poisoned(cause) => write!(f, "session unusable after an earlier {cause}; revive it first"),
            Error::Cancelled { op, reason } => write!(f, "{op}: cancelled ({reason})"),
            Error::Local(e) => e.fmt(f),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Connect(e) => e.source(),
            Error::Protocol { violation, .. } => Some(violation),
            _ => None,
        }
    }
}

impl From<ConnectError> for Error {
    fn from(e: ConnectError) -> Self {
        Error::Connect(e)
    }
}

impl From<RemoteError> for Error {
    fn from(e: RemoteError) -> Self {
        Error::Remote(e)
    }
}

impl From<LocalError> for Error {
    fn from(e: LocalError) -> Self {
        Error::Local(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutKind {
    /// Silence longer than `CallBudget::idle`.
    Idle,
    /// `CallBudget::total` elapsed, keepalives or not.
    Total,
}

impl fmt::Display for TimeoutKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TimeoutKind::Idle => "idle",
            TimeoutKind::Total => "total",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    Interrupt,
    Deadline,
}

impl fmt::Display for CancelReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CancelReason::Interrupt => "interrupt",
            CancelReason::Deadline => "deadline",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LostCause {
    /// Peer closed (read returned 0).
    Eof,
    Reset,
    BrokenPipe,
    Io(io::ErrorKind),
}

impl LostCause {
    pub(crate) fn from_io(e: &io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => LostCause::Reset,
            io::ErrorKind::BrokenPipe => LostCause::BrokenPipe,
            io::ErrorKind::UnexpectedEof => LostCause::Eof,
            k => LostCause::Io(k),
        }
    }
}

impl fmt::Display for LostCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LostCause::Eof => f.write_str("closed by peer"),
            LostCause::Reset => f.write_str("reset by peer"),
            LostCause::BrokenPipe => f.write_str("broken pipe"),
            LostCause::Io(k) => write!(f, "io error: {k}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoisonCause {
    NeverConnected,
    Timeout,
    Lost,
    Protocol,
    Cancelled,
    ReconnectFailed,
}

impl fmt::Display for PoisonCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PoisonCause::NeverConnected => "missing connect",
            PoisonCause::Timeout => "timeout",
            PoisonCause::Lost => "connection loss",
            PoisonCause::Protocol => "protocol violation",
            PoisonCause::Cancelled => "cancellation",
            PoisonCause::ReconnectFailed => "failed reconnect",
        })
    }
}

/// Connect-time failure, classified so the operator can act on it.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConnectError {
    Resolve {
        endpoint: Endpoint,
        source: io::Error,
    },
    /// Nothing listens: PowerView not running, RCL not enabled, or wrong port.
    Refused {
        endpoint: Endpoint,
    },
    /// SYN unanswered: wrong host or a firewall drop.
    TimedOut {
        endpoint: Endpoint,
        after: Duration,
    },
    Unreachable {
        endpoint: Endpoint,
        kind: io::ErrorKind,
    },
    /// TCP is up, the attach reply is not RCL.
    NotRcl {
        endpoint: Endpoint,
        evidence: NotRclEvidence,
    },
    /// TCP is up, the attach got no reply: busy PowerView, modal dialog, or a silent service.
    NoAnswer {
        endpoint: Endpoint,
        waited: Duration,
    },
    ClosedDuringAttach {
        endpoint: Endpoint,
        cause: LostCause,
    },
    Rejected {
        endpoint: Endpoint,
        remote: RemoteError,
    },
    Cancelled {
        endpoint: Endpoint,
        reason: CancelReason,
    },
    Io {
        endpoint: Endpoint,
        source: io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotRclEvidence {
    /// First printable bytes the peer sent, e.g. `HTTP/1.1 400`.
    Banner(String),
    Violation(Violation),
}

impl fmt::Display for NotRclEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotRclEvidence::Banner(b) => write!(f, "banner {b:?}"),
            NotRclEvidence::Violation(v) => v.fmt(f),
        }
    }
}

impl ConnectError {
    pub fn tag(&self) -> &'static str {
        match self {
            ConnectError::Resolve { .. } => "resolve",
            ConnectError::Refused { .. } => "refused",
            ConnectError::TimedOut { .. } => "connect-timeout",
            ConnectError::Unreachable { .. } => "unreachable",
            ConnectError::NotRcl { .. } => "not-rcl",
            ConnectError::NoAnswer { .. } => "no-answer",
            ConnectError::ClosedDuringAttach { .. } => "closed-during-attach",
            ConnectError::Rejected { .. } => "attach-rejected",
            ConnectError::Cancelled { .. } => "cancelled",
            ConnectError::Io { .. } => "io",
        }
    }

    /// One line telling the operator what to check. Single owner of this text.
    pub fn hint(&self) -> &'static str {
        match self {
            ConnectError::Refused { .. } => {
                "PowerView not running, RCL not enabled (RCL=NETTCP in config.t32), or wrong --port"
            }
            ConnectError::TimedOut { .. } | ConnectError::Unreachable { .. } => {
                "wrong --host for this WSL networking mode, or a firewall drops the port"
            }
            ConnectError::NotRcl { .. } => "another service owns this port; check PowerView's RCL port",
            ConnectError::NoAnswer { .. } => "PowerView busy or blocked by a modal dialog; look at its window",
            ConnectError::ClosedDuringAttach { .. } | ConnectError::Rejected { .. } => {
                "PowerView closed or refused the session; demo mode (no license) blocks the Remote API"
            }
            ConnectError::Resolve { .. } => "host name does not resolve",
            ConnectError::Cancelled { .. } => "interrupted",
            ConnectError::Io { .. } => "socket error; see the message",
        }
    }

    pub fn endpoint(&self) -> &Endpoint {
        match self {
            ConnectError::Resolve { endpoint, .. }
            | ConnectError::Refused { endpoint }
            | ConnectError::TimedOut { endpoint, .. }
            | ConnectError::Unreachable { endpoint, .. }
            | ConnectError::NotRcl { endpoint, .. }
            | ConnectError::NoAnswer { endpoint, .. }
            | ConnectError::ClosedDuringAttach { endpoint, .. }
            | ConnectError::Rejected { endpoint, .. }
            | ConnectError::Cancelled { endpoint, .. }
            | ConnectError::Io { endpoint, .. } => endpoint,
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ep = self.endpoint();
        match self {
            ConnectError::Resolve { source, .. } => write!(f, "{ep}: cannot resolve host: {source}"),
            ConnectError::Refused { .. } => write!(f, "{ep}: connection refused"),
            ConnectError::TimedOut { after, .. } => write!(f, "{ep}: TCP connect timed out after {after:.2?}"),
            ConnectError::Unreachable { kind, .. } => write!(f, "{ep}: unreachable ({kind})"),
            ConnectError::NotRcl { evidence, .. } => write!(f, "{ep}: not an RCL peer ({evidence})"),
            ConnectError::NoAnswer { waited, .. } => write!(f, "{ep}: TCP open but no RCL answer within {waited:.2?}"),
            ConnectError::ClosedDuringAttach { cause, .. } => write!(f, "{ep}: peer {cause} during attach"),
            ConnectError::Rejected { remote, .. } => write!(f, "{ep}: attach rejected: {remote}"),
            ConnectError::Cancelled { reason, .. } => write!(f, "{ep}: connect cancelled ({reason})"),
            ConnectError::Io { source, .. } => write!(f, "{ep}: {source}"),
        }
    }
}

impl StdError for ConnectError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            ConnectError::Resolve { source, .. } | ConnectError::Io { source, .. } => Some(source),
            ConnectError::Rejected { remote, .. } => Some(remote),
            _ => None,
        }
    }
}
