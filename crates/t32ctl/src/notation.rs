// SPDX-License-Identifier: Apache-2.0
//! Argument notation. Every argument is parsed once, at the boundary, by one of these types, and
//! every value t32ctl prints is in a form the same type parses: `parse(display(x)) == x`.

use std::{borrow::Cow, fmt, str::FromStr, time::Duration};

use serde::Serialize;
use t32rcl::AccessClass;
use t32stage::{RegName, RelPath};

/// One notation form, as `read describe` lists it.
#[derive(Debug, Serialize)]
pub struct Form {
    pub form: &'static str,
    pub examples: &'static [&'static str],
    pub meaning: &'static str,
}

/// The forms this t32ctl parses. Symbol and source-line places are planned.
pub const FORMS: &[Form] = &[
    Form {
        form: "hex address",
        examples: &["0x40080a98"],
        meaning: "`0x` is required; a bare number is never an address",
    },
    Form {
        form: "access class",
        examples: &["A:0x40000000"],
        meaning: "one of the profile's classes before `:`; without one, the profile's default class",
    },
    Form {
        form: "register-relative",
        examples: &["@sp", "@x1+0x10", "@x1-0x8"],
        meaning: "the register's current value, with an optional `0x` offset",
    },
    Form {
        form: "size",
        examples: &["24", "0x40", "4K", "64M", "1G"],
        meaning: "bytes; `K`, `M` and `G` are KiB, MiB and GiB",
    },
    Form {
        form: "duration",
        examples: &["500ms", "30s", "41.3s", "5m", "2m40s", "1h30m"],
        meaning: "each duration has its range; a value outside it is refused, never clamped",
    },
    Form {
        form: "integer",
        examples: &["42", "0x2a", "-1"],
        meaning: "decimal or `0x` hex, 64-bit signed; printed in decimal",
    },
    Form {
        form: "output name",
        examples: &["ddr.bin"],
        meaning: "a plain file name without `/`, not starting with `.`",
    },
    Form {
        form: "PRACTICE text",
        examples: &["'OS.PTD()'"],
        meaning: "one line of 1 to 2037 bytes, sent literally: PowerView expands no macros in it",
    },
];

/// An argument outside t32ctl notation or its bound. The message names the accepted forms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotationError(pub String);

impl fmt::Display for NotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotationError {}

fn err<T>(message: String) -> Result<T, NotationError> {
    Err(NotationError(message))
}

/// A byte count: `24`, `0x40`, `4K`, `64M`, `1G` (K, M, G are KiB, MiB, GiB).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Size(pub u64);

impl Size {
    pub fn bytes(self) -> u64 {
        self.0
    }
}

impl FromStr for Size {
    type Err = NotationError;

    fn from_str(s: &str) -> Result<Self, NotationError> {
        let forms = || format!("size {s:?}: use bytes (24), 0x hex (0x40), or a K, M, G suffix (4K, 64M, 1G)");
        if let Some(hex) = s.strip_prefix("0x") {
            return match parse_hex(hex) {
                Some(v) => Ok(Size(v)),
                None => err(forms()),
            };
        }
        let (digits, unit) = match s.as_bytes().last() {
            Some(b'K') => (&s[..s.len() - 1], 1u64 << 10),
            Some(b'M') => (&s[..s.len() - 1], 1 << 20),
            Some(b'G') => (&s[..s.len() - 1], 1 << 30),
            _ => (s, 1),
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return err(forms());
        }
        match digits.parse::<u64>().ok().and_then(|n| n.checked_mul(unit)) {
            Some(v) => Ok(Size(v)),
            None => err(format!("size {s:?} does not fit 64 bits")),
        }
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0;
        for (shift, unit) in [(30, 'G'), (20, 'M'), (10, 'K')] {
            if n != 0 && n.trailing_zeros() >= shift {
                return write!(f, "{}{unit}", n >> shift);
            }
        }
        write!(f, "{n}")
    }
}

/// 1 to 16 hex digits.
fn parse_hex(digits: &str) -> Option<u64> {
    let ok = (1..=16).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| u64::from_str_radix(digits, 16).ok()).flatten()
}

/// A duration in milliseconds within `MIN_MS..=MAX_MS`. Out of range is a notation error, never
/// clamped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dur<const MIN_MS: u64, const MAX_MS: u64>(u64);

/// `raw script --timeout` and a procedure's declared timeout.
pub type ScriptTimeout = Dur<1_000, 7_200_000>;
/// `[calls] transfer_timeout`.
pub type TransferTimeout = Dur<1_000, 3_600_000>;

impl<const MIN_MS: u64, const MAX_MS: u64> Dur<MIN_MS, MAX_MS> {
    pub fn from_ms(ms: u64) -> Result<Self, NotationError> {
        if (MIN_MS..=MAX_MS).contains(&ms) {
            Ok(Self(ms))
        } else {
            err(format!("duration {} is outside {}..{}", display_ms(ms), display_ms(MIN_MS), display_ms(MAX_MS)))
        }
    }

    pub fn as_ms(self) -> u64 {
        self.0
    }

    pub fn as_duration(self) -> Duration {
        Duration::from_millis(self.0)
    }
}

impl<const MIN_MS: u64, const MAX_MS: u64> FromStr for Dur<MIN_MS, MAX_MS> {
    type Err = NotationError;

    fn from_str(s: &str) -> Result<Self, NotationError> {
        Self::from_ms(parse_ms(s)?)
    }
}

impl<const MIN_MS: u64, const MAX_MS: u64> fmt::Display for Dur<MIN_MS, MAX_MS> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&display_ms(self.0))
    }
}

/// `500ms`, or hours, minutes and seconds in that order, each at most once (`1h30m`, `2m40s`),
/// where only seconds take a fraction of up to three digits (`41.3s`).
pub fn parse_ms(s: &str) -> Result<u64, NotationError> {
    let forms = || NotationError(format!("duration {s:?}: use 500ms, 30s, 41.3s, 5m, 2m40s or 1h30m"));
    let too_long = || NotationError(format!("duration {s:?} is too long"));
    if let Some(ms) = s.strip_suffix("ms") {
        return digits(ms).ok_or_else(forms)?.parse::<u64>().map_err(|_| too_long());
    }
    let mut rest = s;
    let mut total: u64 = 0;
    for (unit, ms_per) in [('h', 3_600_000u64), ('m', 60_000)] {
        if let Some(i) = rest.find(unit) {
            let n: u64 = digits(&rest[..i]).ok_or_else(forms)?.parse().map_err(|_| too_long())?;
            total = n.checked_mul(ms_per).and_then(|v| v.checked_add(total)).ok_or_else(too_long)?;
            rest = &rest[i + 1..];
        }
    }
    if let Some(secs) = rest.strip_suffix('s') {
        let (whole, frac) = secs.split_once('.').unwrap_or((secs, ""));
        let whole: u64 = digits(whole).ok_or_else(forms)?.parse().map_err(|_| too_long())?;
        let frac_ms = match frac {
            "" if !secs.contains('.') => 0,
            f if (1..=3).contains(&f.len()) => {
                digits(f).ok_or_else(forms)?.parse::<u64>().map_err(|_| forms())? * 10u64.pow(3 - f.len() as u32)
            }
            _ => return Err(forms()),
        };
        total = whole
            .checked_mul(1000)
            .and_then(|v| v.checked_add(frac_ms))
            .and_then(|v| v.checked_add(total))
            .ok_or_else(too_long)?;
        rest = "";
    }
    if !rest.is_empty() || s.is_empty() {
        return Err(forms());
    }
    Ok(total)
}

fn digits(s: &str) -> Option<&str> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then_some(s)
}

/// Milliseconds below 1 s; seconds with one decimal unless whole below 1 min; hours, minutes and
/// whole seconds without zero parts from 1 min. Rounded half up to that precision.
pub fn display_ms(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let tenths = ms.saturating_add(50) / 100;
    if tenths < 600 {
        return if tenths.is_multiple_of(10) {
            format!("{}s", tenths / 10)
        } else {
            format!("{}.{}s", tenths / 10, tenths % 10)
        };
    }
    let secs = ms.saturating_add(500) / 1000;
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let mut out = String::new();
    for (n, unit) in [(h, 'h'), (m, 'm'), (s, 's')] {
        if n != 0 {
            out.push_str(&format!("{n}{unit}"));
        }
    }
    out
}

const PLACE_FORMS: &str = "use [CLASS:]0x<hex> or @<reg>[+0x<hex>|-0x<hex>]";

/// A place in memory: `[CLASS:]0x<hex>` or `@<reg>[+0x<hex>|-0x<hex>]`. Symbol and source-line
/// places are planned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// `class` is `None` when the caller left it to the profile's default class.
    Addr { class: Option<AccessClass>, addr: u64 },
    /// The register's current value plus `offset`.
    Reg { reg: RegName, offset: i64 },
}

impl FromStr for Place {
    type Err = NotationError;

    fn from_str(s: &str) -> Result<Self, NotationError> {
        if let Some(rest) = s.strip_prefix('@') {
            let at = rest.find(['+', '-']).unwrap_or(rest.len());
            let reg = RegName::new(&rest[..at]).map_err(|e| NotationError(format!("place {s:?}: {e}")))?;
            let offset = match rest[at..].split_at_checked(1) {
                None => 0,
                Some((sign, tail)) => {
                    let magnitude = tail.strip_prefix("0x").and_then(parse_hex).and_then(|v| i64::try_from(v).ok());
                    let Some(v) = magnitude else {
                        return err(format!("place {s:?}: a register offset is +0x<hex> or -0x<hex>"));
                    };
                    if sign == "-" { -v } else { v }
                }
            };
            return Ok(Place::Reg { reg, offset });
        }
        let (class, addr) = match s.split_once(':') {
            Some((left, _)) if left.contains(['.', '/']) => {
                return err(format!("place {s:?}: source-line places are planned, not in this t32ctl; {PLACE_FORMS}"));
            }
            Some((left, right)) => {
                let class = AccessClass::parse(left).map_err(|_| {
                    NotationError(format!("place {s:?}: {left:?} is not an access class; {PLACE_FORMS}"))
                })?;
                (Some(class), right)
            }
            None => (None, s),
        };
        if let Some(v) = addr.strip_prefix("0x").and_then(parse_hex) {
            return Ok(Place::Addr { class, addr: v });
        }
        if !addr.is_empty() && addr.bytes().all(|b| b.is_ascii_digit()) {
            return err(format!("place {s:?}: a bare number is never an address; {PLACE_FORMS}"));
        }
        let symbol = addr.split(['+', '-']).next().unwrap_or("");
        if symbol.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && symbol.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return err(format!("place {s:?}: symbol places are planned, not in this t32ctl; {PLACE_FORMS}"));
        }
        err(format!("place {s:?}: {PLACE_FORMS}"))
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Place::Addr { class: Some(c), addr } => write!(f, "{}:{addr:#x}", c.as_str()),
            Place::Addr { class: None, addr } => write!(f, "{addr:#x}"),
            Place::Reg { reg, offset: 0 } => write!(f, "@{reg}"),
            Place::Reg { reg, offset } if *offset < 0 => write!(f, "@{reg}-{:#x}", offset.unsigned_abs()),
            Place::Reg { reg, offset } => write!(f, "@{reg}+{offset:#x}"),
        }
    }
}

/// A PowerView window name (`WinPOS`), as `--name` takes it: `[A-Za-z0-9_]{1,64}`, so it goes
/// into `WINdow.COMMAND(<name>)` as it is. Case-sensitive, like PowerView's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowName(String);

impl WindowName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for WindowName {
    type Err = NotationError;

    fn from_str(s: &str) -> Result<Self, NotationError> {
        if s.is_empty() || s.len() > 64 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return err(format!("window name {s:?}: 1 to 64 letters, digits or _"));
        }
        Ok(Self(s.to_string()))
    }
}

/// A register name for clap: `[A-Za-z][A-Za-z0-9_]{0,31}`.
pub fn reg_name(s: &str) -> Result<RegName, NotationError> {
    RegName::new(s).map_err(|e| NotationError(e.to_string()))
}

/// A plain file name for `--to` and declared outputs: no `/`, not starting with `.`, and a name
/// PowerView can carry (`RelPath`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OutName(RelPath);

impl OutName {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn rel(&self) -> &RelPath {
        &self.0
    }
}

impl FromStr for OutName {
    type Err = NotationError;

    fn from_str(s: &str) -> Result<Self, NotationError> {
        if s.contains('/') || s.starts_with('.') {
            return err(format!("output name {s:?}: a plain file name without / that does not start with ."));
        }
        RelPath::new(s).map(OutName).map_err(|e| NotationError(format!("output name {s:?}: {e}")))
    }
}

impl fmt::Display for OutName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One line of PRACTICE text for `raw cmd` and `raw eval`, sent literally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PracticeText(String);

impl PracticeText {
    /// The longest command PowerView accepts over RCL (build 168999).
    pub const MAX: usize = 2037;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for PracticeText {
    type Err = NotationError;

    fn from_str(s: &str) -> Result<Self, NotationError> {
        if s.is_empty() || s.len() > Self::MAX {
            return err(format!("PRACTICE text is {} bytes; it must be 1 to {} bytes", s.len(), Self::MAX));
        }
        if let Some(c) = s.chars().find(|c| c.is_control()) {
            return err(format!("PRACTICE text contains the control character {c:?}; it must be one line"));
        }
        Ok(Self(s.to_string()))
    }
}

impl fmt::Display for PracticeText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A signed 64-bit integer: `42`, `0x2a`, `-1`. Displays in decimal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IntValue(pub i64);

impl FromStr for IntValue {
    type Err = NotationError;

    fn from_str(s: &str) -> Result<Self, NotationError> {
        let parsed = match s.strip_prefix("0x") {
            Some(hex) => parse_hex(hex).and_then(|v| i64::try_from(v).ok()),
            None => {
                let body = s.strip_prefix('-').unwrap_or(s);
                digits(body).and_then(|_| s.parse::<i64>().ok())
            }
        };
        parsed.map(IntValue).ok_or_else(|| NotationError(format!("integer {s:?}: use 42, 0x2a or -1, within 64 bits")))
    }
}

impl fmt::Display for IntValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// `s` as one POSIX shell word: as is when every byte is in `[A-Za-z0-9_./:+=,@%-]`, else in
/// single quotes.
pub fn shell_word(s: &str) -> Cow<'_, str> {
    let plain = !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./:+=,@%-".contains(&b));
    if plain { Cow::Borrowed(s) } else { Cow::Owned(format!("'{}'", s.replace('\'', r"'\''"))) }
}

#[cfg(test)]
#[path = "notation_tests.rs"]
mod tests;
