// SPDX-License-Identifier: Apache-2.0
//! Exit codes by what the caller does next, the error tags, and the one classification of every
//! failure below t32ctl: by variant, status code and operation context, never by message text.

use std::fmt;

use t32rcl::{CancelReason, ConnectError, Error, LocalError, RemoteKind};
use t32stage::{LeaseError, StageError};

use crate::{
    notation::NotationError,
    report::{Body, Hint},
};

/// What the caller does next. The code is the process exit status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Exit {
    Ok,
    Outcome,
    Fix,
    State,
    Wait,
    Human,
    Interrupted,
}

impl Exit {
    pub const ALL: [Exit; 7] =
        [Exit::Ok, Exit::Outcome, Exit::Fix, Exit::State, Exit::Wait, Exit::Human, Exit::Interrupted];

    pub fn code(self) -> u8 {
        match self {
            Exit::Ok => 0,
            Exit::Outcome => 1,
            Exit::Fix => 2,
            Exit::State => 3,
            Exit::Wait => 4,
            Exit::Human => 5,
            Exit::Interrupted => 130,
        }
    }

    pub fn class(self) -> &'static str {
        match self {
            Exit::Ok => "ok",
            Exit::Outcome => "outcome",
            Exit::Fix => "fix",
            Exit::State => "state",
            Exit::Wait => "wait",
            Exit::Human => "human",
            Exit::Interrupted => "interrupted",
        }
    }

    /// The "Do next" column of the exit-code table.
    pub fn next(self) -> &'static str {
        match self {
            Exit::Ok => "Continue.",
            Exit::Outcome => {
                "The command ran, but the target or script ended otherwise than asked. Read the report; do not \
                 retry blindly."
            }
            Exit::Fix => {
                "The command text is wrong: syntax, notation, policy, grade word, an unknown name, or PowerView \
                 refused it. Change the command; the same text fails the same way."
            }
            Exit::State => {
                "The target is not in the state the command needs: the debug port is down, or the target runs. Do \
                 what the report names, then rerun."
            }
            Exit::Wait => {
                "Another call or a PowerView script is using the debugger; the report says which. Do other work or \
                 wait once; never loop."
            }
            Exit::Human => "Stop and tell your human what the report says.",
            Exit::Interrupted => {
                "The call was interrupted; the report says what was cleaned up. Observe before acting again."
            }
        }
    }
}

/// The stable tag of an error, as `error[<tag>]` prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Usage,
    Notation,
    MemoryPolicy,
    Refused,
    GradeMismatch,
    UnknownProc,
    ProcParams,
    BadBundle,
    FileExists,
    NotHalted,
    TargetDown,
    PvScriptRunning,
    VmBusy,
    RclLocked,
    Unreachable,
    NoAnswer,
    Dialog,
    ScriptVanished,
    PvScriptHalted,
    CpuMismatch,
    ProfileInvalid,
    NoProfile,
    TargetPower,
    ConnectionLost,
    PvError,
    Residue,
    StateDir,
    Interrupted,
}

impl ErrorKind {
    /// In the order of the error-tag table.
    pub const ALL: [ErrorKind; 28] = [
        ErrorKind::Usage,
        ErrorKind::Notation,
        ErrorKind::MemoryPolicy,
        ErrorKind::Refused,
        ErrorKind::GradeMismatch,
        ErrorKind::UnknownProc,
        ErrorKind::ProcParams,
        ErrorKind::BadBundle,
        ErrorKind::FileExists,
        ErrorKind::NotHalted,
        ErrorKind::TargetDown,
        ErrorKind::PvScriptRunning,
        ErrorKind::VmBusy,
        ErrorKind::RclLocked,
        ErrorKind::Unreachable,
        ErrorKind::NoAnswer,
        ErrorKind::Dialog,
        ErrorKind::ScriptVanished,
        ErrorKind::PvScriptHalted,
        ErrorKind::CpuMismatch,
        ErrorKind::ProfileInvalid,
        ErrorKind::NoProfile,
        ErrorKind::TargetPower,
        ErrorKind::ConnectionLost,
        ErrorKind::PvError,
        ErrorKind::Residue,
        ErrorKind::StateDir,
        ErrorKind::Interrupted,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            ErrorKind::Usage => "usage",
            ErrorKind::Notation => "notation",
            ErrorKind::MemoryPolicy => "memory-policy",
            ErrorKind::Refused => "refused",
            ErrorKind::GradeMismatch => "grade-mismatch",
            ErrorKind::UnknownProc => "unknown-proc",
            ErrorKind::ProcParams => "proc-params",
            ErrorKind::BadBundle => "bad-bundle",
            ErrorKind::FileExists => "file-exists",
            ErrorKind::NotHalted => "not-halted",
            ErrorKind::TargetDown => "target-down",
            ErrorKind::PvScriptRunning => "pv-script-running",
            ErrorKind::VmBusy => "vm-busy",
            ErrorKind::RclLocked => "rcl-locked",
            ErrorKind::Unreachable => "unreachable",
            ErrorKind::NoAnswer => "no-answer",
            ErrorKind::Dialog => "dialog",
            ErrorKind::ScriptVanished => "script-vanished",
            ErrorKind::PvScriptHalted => "pv-script-halted",
            ErrorKind::CpuMismatch => "cpu-mismatch",
            ErrorKind::ProfileInvalid => "profile-invalid",
            ErrorKind::NoProfile => "no-profile",
            ErrorKind::TargetPower => "target-power",
            ErrorKind::ConnectionLost => "connection-lost",
            ErrorKind::PvError => "pv-error",
            ErrorKind::Residue => "residue",
            ErrorKind::StateDir => "state-dir",
            ErrorKind::Interrupted => "interrupted",
        }
    }

    pub fn exit(self) -> Exit {
        match self {
            ErrorKind::Usage
            | ErrorKind::Notation
            | ErrorKind::MemoryPolicy
            | ErrorKind::Refused
            | ErrorKind::GradeMismatch
            | ErrorKind::UnknownProc
            | ErrorKind::ProcParams
            | ErrorKind::BadBundle
            | ErrorKind::FileExists => Exit::Fix,
            ErrorKind::NotHalted | ErrorKind::TargetDown => Exit::State,
            ErrorKind::PvScriptRunning | ErrorKind::VmBusy | ErrorKind::RclLocked => Exit::Wait,
            ErrorKind::Unreachable
            | ErrorKind::NoAnswer
            | ErrorKind::Dialog
            | ErrorKind::ScriptVanished
            | ErrorKind::PvScriptHalted
            | ErrorKind::CpuMismatch
            | ErrorKind::ProfileInvalid
            | ErrorKind::NoProfile
            | ErrorKind::TargetPower
            | ErrorKind::ConnectionLost
            | ErrorKind::PvError
            | ErrorKind::Residue
            | ErrorKind::StateDir => Exit::Human,
            ErrorKind::Interrupted => Exit::Interrupted,
        }
    }

    /// The "Meaning" column of the error-tag table.
    pub fn meaning(self) -> &'static str {
        match self {
            ErrorKind::Usage => "The command line does not parse; the message is clap's.",
            ErrorKind::Notation => {
                "An argument is not in t32ctl notation or exceeds its bound; the message names the accepted forms."
            }
            ErrorKind::MemoryPolicy => {
                "A byte of the access is outside the memory map, its region refuses this access or width, or the \
                 access class is not accepted; the message names the first refused byte."
            }
            ErrorKind::Refused => "PowerView refused the command, function, or memory read; its message is shown.",
            ErrorKind::GradeMismatch => {
                "The grade word differs from the grade the profile declares for this procedure."
            }
            ErrorKind::UnknownProc => {
                "The board profile declares no procedure by that name; `t32ctl read procs` lists them."
            }
            ErrorKind::ProcParams => {
                "Procedure parameters do not match the declaration, or a file parameter cannot be read."
            }
            ErrorKind::BadBundle => {
                "The script directory cannot be bundled: a symlink or special file, a name PRACTICE cannot carry, \
                 two names that differ only in case, or above the bundle limits; the file is named."
            }
            ErrorKind::FileExists => "`raw script --out` never overwrites; choose a new directory.",
            ErrorKind::NotHalted => {
                "The command needs a stopped target, or every touched region must declare `running = true`."
            }
            ErrorKind::TargetDown => {
                "The debug port is down: attach first, with the board's `attach` procedure or in PowerView."
            }
            ErrorKind::PvScriptRunning => {
                "A PRACTICE script runs in PowerView: another t32ctl call's, or someone else's."
            }
            ErrorKind::VmBusy => {
                "Another t32ctl or t32probe process on this PowerView's state directory holds the `VM:` staging \
                 window."
            }
            ErrorKind::RclLocked => "PowerView's API lock is held by another RCL client (status 123).",
            ErrorKind::Unreachable => "PowerView cannot be reached; the details name what to check.",
            ErrorKind::NoAnswer => "PowerView accepted TCP but does not answer: a modal dialog, or a blocking command.",
            ErrorKind::Dialog => "A script opened a dialog; it was stopped, and the window may still be open.",
            ErrorKind::ScriptVanished => {
                "A script run ended without the runner reaching its end: something outside t32ctl cleared the \
                 PRACTICE stack."
            }
            ErrorKind::PvScriptHalted => {
                "A PRACTICE script t32ctl did not start has halted on the stack; someone must end it in PowerView."
            }
            ErrorKind::CpuMismatch => "The connected CPU is not the profile's CPU: wrong board or wrong profile.",
            ErrorKind::ProfileInvalid => {
                "A profile, binding, or config file has an error; file, line, and column are shown."
            }
            ErrorKind::NoProfile => "No target profile was found for this build; the search path is shown.",
            ErrorKind::TargetPower => "Target power or debug port failure reported by PowerView.",
            ErrorKind::ConnectionLost => "The connection died and one reconnect did not help.",
            ErrorKind::PvError => {
                "PowerView failed a step t32ctl generated (a staging command, a size check, a reply t32ctl cannot \
                 use); the step and PowerView's message are shown."
            }
            ErrorKind::Residue => {
                "A script t32ctl started could not be unwound; the report names the PowerView command to type."
            }
            ErrorKind::StateDir => "The state or output directory cannot be written.",
            ErrorKind::Interrupted => "Ctrl-C, SIGTERM or SIGHUP; cleanup ran with its own deadline.",
        }
    }
}

/// A failed call: its tag, one line of message, detail lines, and the commands to run next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CtlError {
    pub kind: ErrorKind,
    pub message: String,
    pub details: Vec<String>,
    /// `next:` hints the report prints after the error.
    pub next: Vec<Hint>,
    /// What the call did before it failed: a script's result whose outputs could not be fetched,
    /// a run a dialog stopped.
    pub body: Option<Box<Body>>,
}

impl CtlError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), details: Vec::new(), next: Vec::new(), body: None }
    }

    pub fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }

    pub fn with_next(mut self, next: Vec<Hint>) -> Self {
        self.next = next;
        self
    }

    pub fn with_body(mut self, body: Body) -> Self {
        self.body = Some(Box::new(body));
        self
    }

    pub fn exit(&self) -> Exit {
        self.kind.exit()
    }
}

impl fmt::Display for CtlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.kind.tag(), self.message)
    }
}

impl std::error::Error for CtlError {}

impl From<NotationError> for CtlError {
    fn from(e: NotationError) -> Self {
        CtlError::new(ErrorKind::Notation, e.0)
    }
}

/// What kind of request failed: a read the caller asked for, PRACTICE text the caller typed, or a
/// step t32ctl generated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Read,
    Text,
    Stage,
}

/// Status codes PowerView reports for target power and debug port failures.
const TARGET_POWER: [u8; 7] = [57, 128, 140, 144, 161, 162, 164];
/// `STD_RUNNING`.
const RUNNING: u8 = 2;
/// `STD_NOTRUNNING`, `STD_RESET`, `STD_RESETDETECTED`, `STD_CORENOTACTIVE`.
const DOWN: [u8; 4] = [3, 4, 48, 64];
const LOCKED: u8 = 123;

/// Classify a failed RCL request. `what` names the request (`SOFTWARE.BUILD()`) and leads the
/// message.
pub fn from_rcl(e: &Error, context: Context, what: &str) -> CtlError {
    let kind = match e {
        Error::Connect(c) => return from_connect(c),
        Error::Timeout { .. } => ErrorKind::NoAnswer,
        Error::Lost { .. } | Error::Protocol { .. } | Error::Poisoned(_) => ErrorKind::ConnectionLost,
        Error::Cancelled { reason: CancelReason::Interrupt, .. } => ErrorKind::Interrupted,
        // A caller that sets a deadline turns its expiry into an outcome before classifying.
        Error::Cancelled { reason: CancelReason::Deadline, .. } => ErrorKind::NoAnswer,
        Error::Remote(r) if r.code == LOCKED => ErrorKind::RclLocked,
        Error::Remote(r) if r.code == RUNNING => ErrorKind::NotHalted,
        Error::Remote(r) if DOWN.contains(&r.code) => ErrorKind::TargetDown,
        Error::Remote(r) if TARGET_POWER.contains(&r.code) => ErrorKind::TargetPower,
        Error::Remote(r) if r.kind == RemoteKind::Attach => ErrorKind::Unreachable,
        Error::Remote(_) => match context {
            Context::Read | Context::Text => ErrorKind::Refused,
            Context::Stage => ErrorKind::PvError,
        },
        Error::UnexpectedResult { .. } => ErrorKind::PvError,
        Error::Local(LocalError::TextTooLong { .. } | LocalError::EmbeddedNul) if context == Context::Text => {
            ErrorKind::Notation
        }
        _ => ErrorKind::PvError,
    };
    CtlError::new(kind, format!("{what}: {e}"))
}

/// Classify one failed connect attempt; the details name what to check.
pub fn from_connect(e: &ConnectError) -> CtlError {
    let kind = match e {
        ConnectError::NoAnswer { .. } => ErrorKind::NoAnswer,
        ConnectError::Cancelled { .. } => ErrorKind::Interrupted,
        _ => ErrorKind::Unreachable,
    };
    CtlError::new(kind, e.to_string()).with_details(vec![connect_hint(e).to_string()])
}

/// t32rcl's hint names `t32probe`'s `--host` and `--port`; t32ctl takes both from the config.
fn connect_hint(e: &ConnectError) -> &'static str {
    match e {
        ConnectError::Refused { .. } => {
            "PowerView not running, RCL not enabled (RCL=NETTCP in config.t32), or wrong [connection] port in \
             ~/.config/t32ctl/config.toml"
        }
        ConnectError::TimedOut { .. } | ConnectError::Unreachable { .. } => {
            "wrong [connection] host in ~/.config/t32ctl/config.toml for this WSL networking mode, or a firewall \
             drops the port"
        }
        _ => e.hint(),
    }
}

/// Classify a failed staging step. `step` names it and leads the message; a bundle error is the
/// profile directory's.
pub fn from_stage(e: &StageError, step: &str) -> CtlError {
    let kind = match e {
        StageError::Rcl(rcl) => return from_rcl(rcl, Context::Stage, step),
        StageError::Lease(LeaseError::Busy { .. }) => ErrorKind::VmBusy,
        StageError::Lease(LeaseError::Io { .. }) | StageError::Journal { .. } => ErrorKind::StateDir,
        StageError::Busy { depth: -1 } => {
            let message = format!("{step}: a PRACTICE script is running in PowerView; nothing was started");
            return CtlError::new(ErrorKind::PvScriptRunning, message);
        }
        StageError::Busy { depth } => {
            let message = format!(
                "{step}: a PRACTICE script t32ctl did not start has halted on the stack (depth {depth}); someone \
                 must end it in PowerView"
            );
            return CtlError::new(ErrorKind::PvScriptHalted, message);
        }
        StageError::UnwindFailed { .. } => ErrorKind::Residue,
        StageError::Arg(_) => ErrorKind::Notation,
        StageError::Bundle(_) => ErrorKind::ProfileInvalid,
        StageError::Source(_) => ErrorKind::ProcParams,
        StageError::Path(_) => {
            return CtlError::new(ErrorKind::PvError, format!("{step}: {e}; set [staging] root in the config"));
        }
        StageError::Vm(_) => {
            return CtlError::new(ErrorKind::PvError, format!("{step}: {e}; check [staging] vm_len in the config"));
        }
        _ => ErrorKind::PvError,
    };
    CtlError::new(kind, format!("{step}: {e}"))
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
