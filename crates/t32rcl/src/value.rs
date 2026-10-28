// SPDX-License-Identifier: Apache-2.0
// Function result types from pyrcl 1.1.6 rcl.py:308-352 (MIT, (c) 2020 Lauterbach GmbH). See NOTICE.
//! Domain values. Parsing from wire bytes into these types happens in `wire`.

use std::{fmt, time::Duration};

use crate::error::LocalError;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    /// NETTCP default of `SETUP.API.RCL.Enable /PRotocol TCP`.
    pub const DEFAULT_PORT: u16 = 20000;

    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self { host: host.into(), port }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// How long one call may wait. Keepalive frames and partial frames reset `idle`, never `total`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallBudget {
    pub idle: Duration,
    pub total: Option<Duration>,
}

impl CallBudget {
    pub const DEFAULT: CallBudget = CallBudget { idle: Duration::from_secs(10), total: Some(Duration::from_secs(300)) };

    pub const fn idle(idle: Duration) -> Self {
        Self { idle, total: None }
    }

    pub const fn with_total(self, total: Duration) -> Self {
        Self { total: Some(total), ..self }
    }
}

impl Default for CallBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How EXP 2 names a global macro on the wire. Only `Ampersand` reaches the PRACTICE macro a
/// script uses; `Bare` addresses a separate store that also round-trips (build 168999,
/// `macro-visibility`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MacroSpelling {
    /// `name`, as pyrcl's `get_macro(name)` passes whatever the caller gave.
    Bare,
    /// `&name`, as PRACTICE text spells it.
    #[default]
    Ampersand,
}

impl MacroSpelling {
    pub fn tag(self) -> &'static str {
        match self {
            MacroSpelling::Bare => "bare",
            MacroSpelling::Ampersand => "amp",
        }
    }
}

/// Transfer tunables Phase 1 pins from a probe report. Defaults are the reference clients'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    read_chunk: usize,
    write_chunk: usize,
    macro_spelling: MacroSpelling,
}

impl Tuning {
    pub const MIN_CHUNK: usize = 8;
    /// Client-side ceiling: the `vm-rw` ladder stops here and reports `client-limit`.
    pub const MAX_CHUNK: usize = 0x3C00;
    /// pyrcl `_library.py`: reads `down_align(1024 - 10, 8)`, writes `1024`. ASSUMED(vm-rw):
    /// PowerView also accepts larger chunks; the probe's ladder finds up to which size.
    pub const REFERENCE: Tuning =
        Tuning { read_chunk: 1008, write_chunk: 1024, macro_spelling: MacroSpelling::Ampersand };

    pub fn new(read_chunk: usize, write_chunk: usize) -> Result<Self, LocalError> {
        for got in [read_chunk, write_chunk] {
            if !(Self::MIN_CHUNK..=Self::MAX_CHUNK).contains(&got) {
                return Err(LocalError::BadChunk { got, min: Self::MIN_CHUNK, max: Self::MAX_CHUNK });
            }
        }
        Ok(Self { read_chunk, write_chunk, macro_spelling: MacroSpelling::default() })
    }

    pub fn with_macro_spelling(self, macro_spelling: MacroSpelling) -> Self {
        Self { macro_spelling, ..self }
    }

    pub fn read_chunk(&self) -> usize {
        self.read_chunk
    }

    pub fn write_chunk(&self) -> usize {
        self.write_chunk
    }

    pub fn macro_spelling(&self) -> MacroSpelling {
        self.macro_spelling
    }
}

impl Default for Tuning {
    fn default() -> Self {
        Self::REFERENCE
    }
}

/// `T32_GetPracticeState`: 0 idle, 1 running, 2 dialog open. A script halted at an error or at
/// `STOP` reads as `Idle` although it is still on the PRACTICE stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PracticeState {
    Idle,
    Running,
    Dialog,
    Other(u8),
}

/// `T32_GetState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetState {
    Down,
    Halted,
    Stopped,
    Running,
    Other(u8),
}

impl TargetState {
    pub fn name(self) -> &'static str {
        match self {
            TargetState::Down => "down",
            TargetState::Halted => "halted",
            TargetState::Stopped => "stopped",
            TargetState::Running => "running",
            TargetState::Other(_) => "other",
        }
    }
}

/// Result of `ExecuteFunction`, parsed from the result type word and the text.
#[derive(Clone, Debug, PartialEq)]
pub enum FuncValue {
    Bool(bool),
    /// Decimal (`"12."`).
    Int(i64),
    /// Hex (`0x1F`) or binary (`0y101`).
    Uint(u64),
    Float(f64),
    /// Time in seconds (`"1.5s"`).
    Seconds(f64),
    /// String, ASCII constant, address, ranges, bitmask: kept as text.
    Text(String),
    Empty,
    /// A type word this crate does not interpret, or a text its type word does not parse.
    Raw {
        ty: u32,
        text: String,
    },
}

impl FuncValue {
    pub fn into_bool(self, expr: &str) -> Result<bool, crate::Error> {
        match self {
            FuncValue::Bool(b) => Ok(b),
            other => Err(other.mismatch("bool", expr)),
        }
    }

    pub fn into_int(self, expr: &str) -> Result<i64, crate::Error> {
        match self {
            FuncValue::Int(v) => Ok(v),
            FuncValue::Uint(v) => i64::try_from(v).map_err(|_| FuncValue::Uint(v).mismatch("int", expr)),
            other => Err(other.mismatch("int", expr)),
        }
    }

    pub fn into_text(self, expr: &str) -> Result<String, crate::Error> {
        match self {
            FuncValue::Text(t) => Ok(t),
            FuncValue::Empty => Ok(String::new()),
            other => Err(other.mismatch("text", expr)),
        }
    }

    fn mismatch(&self, wanted: &'static str, expr: &str) -> crate::Error {
        crate::Error::UnexpectedResult { op: crate::Op::Function, wanted, got: format!("{self} from {expr}") }
    }
}

impl fmt::Display for FuncValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FuncValue::Bool(b) => write!(f, "{b}"),
            FuncValue::Int(v) => write!(f, "{v}"),
            FuncValue::Uint(v) => write!(f, "{v:#x}"),
            FuncValue::Float(v) | FuncValue::Seconds(v) => write!(f, "{v}"),
            FuncValue::Text(t) => write!(f, "{t:?}"),
            FuncValue::Empty => f.write_str("<empty>"),
            FuncValue::Raw { ty, text } => write!(f, "type {ty:#x}: {text:?}"),
        }
    }
}

/// Memory access class without the trailing colon: `E`, `SD`, `C15`, `VM`.
/// PowerView does not validate it here; a wrong class fails on the access itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccessClass(String);

impl AccessClass {
    pub fn vm() -> Self {
        Self("VM".to_string())
    }

    /// ASCII alphanumerics, 1..=8 chars, upper-cased. A trailing `:` is accepted and dropped.
    pub fn parse(text: &str) -> Result<Self, LocalError> {
        let t = text.strip_suffix(':').unwrap_or(text);
        if t.is_empty() || t.len() > 8 || !t.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(LocalError::BadAccessClass(text.to_string()));
        }
        Ok(Self(t.to_ascii_uppercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Address with its access class and an optional access width in bytes. Core and space id stay
/// unset, as in the reference clients.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MemAddr {
    pub access: AccessClass,
    pub addr: u64,
    /// Bus access width of every access, as `T32_SetAddressObjWidth` sets it; `None` leaves the
    /// width to PowerView.
    pub width: Option<u16>,
}

impl MemAddr {
    pub fn new(access: AccessClass, addr: u64) -> Self {
        Self { access, addr, width: None }
    }

    pub fn vm(addr: u64) -> Self {
        Self { access: AccessClass::vm(), addr, width: None }
    }

    pub fn with_width(self, width: u16) -> Self {
        Self { width: Some(width), ..self }
    }

    /// Same class and width, address moved by `delta` bytes. `None` on overflow.
    pub fn offset(&self, delta: u64) -> Option<Self> {
        Some(Self { access: self.access.clone(), addr: self.addr.checked_add(delta)?, width: self.width })
    }
}

impl fmt::Display for MemAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{:#x}", self.access.as_str(), self.addr)
    }
}

/// Global PRACTICE macro name without `&`: `[A-Za-z_][A-Za-z0-9_]{0,63}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MacroName(String);

impl MacroName {
    pub fn new(name: &str) -> Result<Self, LocalError> {
        let mut bytes = name.bytes();
        let ok = bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && name.len() <= 64
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if ok { Ok(Self(name.to_string())) } else { Err(LocalError::BadMacroName(name.to_string())) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `&name`, for PRACTICE text.
    pub fn amp(&self) -> String {
        format!("&{}", self.0)
    }

    pub(crate) fn wire(&self, spelling: MacroSpelling) -> String {
        match spelling {
            MacroSpelling::Bare => self.0.clone(),
            MacroSpelling::Ampersand => self.amp(),
        }
    }
}

impl fmt::Display for MacroName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `B::`-style device prompt prepended to every command text. Needed only when the operator
/// changed the PowerView command-line prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevicePrompt(String);

impl DevicePrompt {
    pub fn parse(text: &str) -> Result<Self, LocalError> {
        let body = text.strip_suffix("::").unwrap_or("");
        if body.is_empty() || !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(LocalError::BadPrompt(text.to_string()));
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_classes_are_validated_at_construction() {
        assert!(MacroName::new("t32ctl_root").is_ok());
        for bad in ["", "1a", "&a", "a-b", &"x".repeat(65)] {
            assert!(MacroName::new(bad).is_err(), "{bad}");
        }
        assert_eq!(AccessClass::parse("vm:").map(|a| a.as_str().to_string()), Ok("VM".to_string()));
        assert!(AccessClass::parse("E:0x10").is_err());
        assert!(DevicePrompt::parse("B::").is_ok());
        assert!(DevicePrompt::parse("B:").is_err());
    }

    #[test]
    fn tuning_rejects_chunks_outside_the_frame_bound() {
        assert!(Tuning::new(1008, 1024).is_ok());
        assert!(Tuning::new(4, 1024).is_err());
        assert!(Tuning::new(1008, Tuning::MAX_CHUNK + 1).is_err());
    }
}
