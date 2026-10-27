// SPDX-License-Identifier: Apache-2.0
//! Errors of the staging layer. RCL errors pass through (`Rcl`) so callers still get `class()`.

use std::{error::Error as StdError, fmt, io, path::PathBuf};

use t32rcl::ErrorClass;

#[derive(Debug)]
#[non_exhaustive]
pub enum StageError {
    Rcl(t32rcl::Error),
    Path(PathError),
    Arg(ArgError),
    Vm(VmError),
    Lease(LeaseError),
    /// The write-ahead journal could not be written, so the side effect was not started.
    Journal {
        path: PathBuf,
        source: io::Error,
    },
    /// A PRACTICE script is on the stack. Staging never terminates scripts it did not start.
    Busy {
        depth: i64,
    },
    SizeMismatch {
        what: String,
        want: u64,
        got: u64,
    },
    /// PowerView returned text the caller cannot use.
    BadReply {
        what: &'static str,
        text: String,
    },
    /// A cleanup step ran but its check still finds the thing it should have removed.
    StillPresent {
        what: String,
    },
}

impl StageError {
    /// What the caller should do next. `Busy` and a held lease are retryable refusals.
    pub fn class(&self) -> ErrorClass {
        match self {
            StageError::Rcl(e) => e.class(),
            StageError::Path(_) | StageError::Arg(_) | StageError::Vm(_) => ErrorClass::Usage,
            _ => ErrorClass::Rejected,
        }
    }

    /// The RCL error underneath, if any.
    pub fn rcl(&self) -> Option<&t32rcl::Error> {
        match self {
            StageError::Rcl(e) => Some(e),
            _ => None,
        }
    }
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StageError::Rcl(e) => e.fmt(f),
            StageError::Path(e) => e.fmt(f),
            StageError::Arg(e) => e.fmt(f),
            StageError::Vm(e) => e.fmt(f),
            StageError::Lease(e) => e.fmt(f),
            StageError::Journal { path, source } => write!(f, "journal {}: {source}", path.display()),
            StageError::Busy { depth } => write!(f, "PowerView is busy: PRACTICE stack depth {depth}"),
            StageError::SizeMismatch { what, want, got } => {
                write!(f, "{what}: PowerView has {got} bytes, expected {want}")
            }
            StageError::BadReply { what, text } => write!(f, "unusable PowerView reply for {what}: {text:?}"),
            StageError::StillPresent { what } => write!(f, "{what} is still present after removal"),
        }
    }
}

impl StdError for StageError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            StageError::Rcl(e) => Some(e),
            StageError::Journal { source, .. } => Some(source),
            _ => None,
        }
    }
}

macro_rules! from {
    ($($t:ty => $v:ident),+) => {$(
        impl From<$t> for StageError {
            fn from(e: $t) -> Self {
                StageError::$v(e)
            }
        }
    )+};
}

from!(t32rcl::Error => Rcl, PathError => Path, ArgError => Arg, VmError => Vm, LeaseError => Lease);

impl From<t32rcl::LocalError> for StageError {
    fn from(e: t32rcl::LocalError) -> Self {
        StageError::Rcl(e.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathError {
    Empty,
    NotRelative(String),
    NotAbsolute(String),
    Char { path: String, bad: char },
    Component { path: String, component: String },
    Reserved(String),
    TooLong { path: String, len: usize, max: usize },
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Empty => f.write_str("empty path"),
            PathError::NotRelative(p) => write!(f, "path {p:?} is not relative with `/` separators"),
            PathError::NotAbsolute(p) => write!(f, "path {p:?} is not an absolute PowerView host path"),
            PathError::Char { path, bad } => {
                write!(f, "path {path:?} contains {bad:?}, which PRACTICE text cannot carry")
            }
            PathError::Component { path, component } => {
                write!(f, "path {path:?}: component {component:?} is `.`/`..`, empty, or ends in a dot or space")
            }
            PathError::Reserved(p) => write!(f, "path {p:?} uses a reserved Windows device name"),
            PathError::TooLong { path, len, max } => write!(f, "path {path:?} is {len} bytes, limit {max}"),
        }
    }
}

impl StdError for PathError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgError {
    Unsafe { index: usize, ch: char },
    TooLong { len: usize, max: usize },
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgError::Unsafe { index, ch } => {
                write!(f, "argument {index} contains {ch:?}, which cannot be passed safely")
            }
            ArgError::TooLong { len, max } => write!(f, "arguments are {len} bytes, limit {max}"),
        }
    }
}

impl StdError for ArgError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmError {
    WindowRange { base: u64, len: u64 },
    WindowTooSmall { need: u64, have: u64 },
    ZeroLength,
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmError::WindowRange { base, len } => write!(f, "VM: range {base:#x}+{len:#x} leaves the 32-bit VM: space"),
            VmError::WindowTooSmall { need, have } => write!(f, "need {need} bytes of VM: but the window holds {have}"),
            VmError::ZeroLength => f.write_str("a zero-length VM: region is not representable"),
        }
    }
}

impl StdError for VmError {}

#[derive(Debug)]
pub enum LeaseError {
    /// Another t32ctl or t32probe process on this machine holds the VM: window.
    Busy {
        holder: Option<String>,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LeaseError::Busy { holder: Some(h) } => write!(f, "VM: window is leased by another process ({h})"),
            LeaseError::Busy { holder: None } => f.write_str("VM: window is leased by another process"),
            LeaseError::Io { path, source } => write!(f, "lease file {}: {source}", path.display()),
        }
    }
}

impl StdError for LeaseError {}
