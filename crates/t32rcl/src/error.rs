// SPDX-License-Identifier: Apache-2.0
//! Typed errors. One enum for callers, grouped by what the caller must do next.

use std::fmt;

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
    /// Status byte `[0]` of the response, or 0 for a failed function or EXP call.
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
    /// Status 90 on a memory object access, or 6/15/16/22.
    MemoryAccess,
    /// Running, not running, reset, core not active, debug port failures.
    TargetState,
    /// 123: another client holds the API lock.
    Locked,
    /// 60 invalid license, 83 no Remote API, 147 no device, 254 attach missing.
    Attach,
    /// EXP reply without `PL` block; `EC`/`EM` carry the reason.
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
