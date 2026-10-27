// SPDX-License-Identifier: Apache-2.0
// Ported from t32mcp src/t32api.rs (Apache-2.0, (c) 2026 Lauterbach GmbH) and pyrcl 1.1.6
// _rc/_library.py, _rc/_address.py, _rc/_practice.py, rcl.py (MIT, (c) 2020 Lauterbach GmbH).
// See NOTICE.
//! RCL message encodings. Crate-private: `session` turns every response into a domain value.
//!
//! # Request layout (little endian)
//!
//! ```text
//! short (msg_len <= 0xFF): [msg_len u8][cmd][opt][id] payload [0 if payload odd]
//! long  (msg_len  > 0xFF): [0x00][cmd][opt][id][msg_len+2 u16] payload [0 if payload odd]
//! ```
//! `msg_len` is `2 + payload.len()` unless a command forces a header size (`LenRule::Forced`,
//! memory objects). EXP uses the long header with the u16 equal to the payload length
//! (`LenRule::ExpRaw`). Source: t32api.rs `generic_api_call`/`exp_call`, _library.py
//! `generic_api_call`/`t32_exp`.
//!
//! | Op | cmd | opt | payload | response `[2..]` | source |
//! |---|---|---|---|---|---|
//! | Attach | 0x71 | 0x01 | none | none | `t32_attach` |
//! | ExecuteCommand | 0x72 | 0x04 | u32 4096, text, NUL | none | `t32_executecommand` |
//! | ExecuteFunction | 0x72 | 0x05 | u32 4096, text, NUL | u32 type, u32 size, text | `t32_executefunction` |
//! | GetPracticeState | 0x72 | 0x03 | none | u8 | `t32_getpracticestate` |
//! | Stop | 0x72 | 0x00 | none | none | `t32_stop` |
//! | GetState | 0x74 | 0x10 | none | u8 | `t32_getstate` |
//! | EvalGetString | 0x74 | 0x17 | none | text, NUL | `t32_evalgetstring` |
//! | MemRead | 0x74 | 0x35 | u16 len, addr object | data | `t32_readmemoryobj` |
//! | MemWrite | 0x74 | 0x36 | u16 len, addr object, data | none | `t32_writememoryobj` |
//! | Exp | 0x74 | 0xFE | u16 exp cmd, exp payload | EXP blocks | `t32_exp` |
//!
//! Address object: `u16 3` (A64), `u64 addr`, `"AC"`, `u16 n`, access class NUL-padded to even
//! `n`, `"XX"` (_address.py `serialize`).
//!
//! # Responses
//!
//! `[err u8][id u8][result...]`. `err != 0` with more than 10 bytes carries `u32 n` at `[6..10]`
//! and a message at `[10..10+n]`. EXP results are `u16 len, u16 cmd` and then blocks
//! `tag(2) u16 len content`: `EC` code, `EM` message, `PL` payload; a missing `PL` is an EXP
//! failure; parsing stops at `00 00` (t32api.rs `parse_exp_response`). EXP 2 (global PRACTICE
//! macro): get `u16 0, u16 n, name`; set `u16 1, u16 n, name, u16 m, value`; the `PL` of a get
//! is 2 bytes followed by the value (_practice.py).

use crate::{
    error::{LocalError, Op, RemoteError, RemoteKind, Violation},
    value::{FuncValue, MacroName, MacroSpelling, MemAddr, PracticeState, TargetState},
};

pub(crate) const RAPI_CMD_ATTACH: u8 = 0x71;
pub(crate) const RAPI_CMD_EXECUTE_PRACTICE: u8 = 0x72;
pub(crate) const RAPI_CMD_DEVICE_SPECIFIC: u8 = 0x74;

pub(crate) const PRACTICE_STOP: u8 = 0x00;
pub(crate) const PRACTICE_GETSTATE: u8 = 0x03;
pub(crate) const PRACTICE_EXECUTECOMMAND: u8 = 0x04;
pub(crate) const PRACTICE_EXECUTEFUNCTION: u8 = 0x05;

pub(crate) const DS_GETSTATE: u8 = 0x10;
pub(crate) const DS_EVAL_GETSTRING: u8 = 0x17;
pub(crate) const DS_MEMORY_OBJ_READ: u8 = 0x35;
pub(crate) const DS_MEMORY_OBJ_WRITE: u8 = 0x36;
pub(crate) const DS_EXP: u8 = 0xFE;

pub(crate) const EXP_PRACTICE_MACRO: u16 = 2;

const T32_DEV_ICD: u8 = 1;
/// Reply buffer size announced in ExecuteCommand/ExecuteFunction. Both reference clients send 4096.
const REPLY_BUFFER: u32 = 4096;
/// C driver `MAXPACKETSIZE` (2048) minus the header and terminator it adds.
pub(crate) const MAX_PRACTICE_TEXT: usize = 2048 - 10 - 1;
/// Largest message the long header can carry.
pub(crate) const MAX_MESSAGE: usize = 0xF000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LenRule {
    /// `2 + payload.len()`.
    Auto,
    /// Fixed header-size value, independent of the payload.
    Forced(usize),
    /// Long header, u16 = payload length exactly.
    ExpRaw,
}

/// A request without its id. `Link` stamps the id and frames it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Draft {
    pub cmd: u8,
    pub opt: u8,
    pub len: LenRule,
    pub payload: Vec<u8>,
}

impl Draft {
    fn new(cmd: u8, opt: u8, len: LenRule, payload: Vec<u8>) -> Self {
        Self { cmd, opt, len, payload }
    }

    /// RCL message bytes for request id `id`.
    pub fn seal(&self, id: u8) -> Result<Vec<u8>, LocalError> {
        let n = self.payload.len();
        let mut out = Vec::with_capacity(6 + n + 1);
        match self.len {
            LenRule::ExpRaw => {
                if n > MAX_MESSAGE {
                    return Err(LocalError::ValueTooLarge { len: n, max: MAX_MESSAGE });
                }
                out.extend_from_slice(&[0, self.cmd, self.opt, id]);
                out.extend_from_slice(&(n as u16).to_le_bytes());
            }
            LenRule::Auto | LenRule::Forced(_) => {
                let mut msg_len = if let LenRule::Forced(v) = self.len { v } else { 2 + n };
                if msg_len > 0xFF {
                    msg_len += 2;
                    if msg_len > MAX_MESSAGE {
                        return Err(LocalError::ValueTooLarge { len: msg_len, max: MAX_MESSAGE });
                    }
                    out.extend_from_slice(&[0, self.cmd, self.opt, id]);
                    out.extend_from_slice(&(msg_len as u16).to_le_bytes());
                } else {
                    out.extend_from_slice(&[msg_len as u8, self.cmd, self.opt, id]);
                }
            }
        }
        out.extend_from_slice(&self.payload);
        if n % 2 == 1 {
            out.push(0);
        }
        Ok(out)
    }
}

fn check_text(text: &str) -> Result<(), LocalError> {
    if text.as_bytes().contains(&0) {
        return Err(LocalError::EmbeddedNul);
    }
    if text.len() > MAX_PRACTICE_TEXT {
        return Err(LocalError::TextTooLong { len: text.len(), max: MAX_PRACTICE_TEXT });
    }
    Ok(())
}

fn addr_object(a: &MemAddr) -> Vec<u8> {
    let ac = a.access.as_str().as_bytes();
    let padded = (ac.len() + 1).next_multiple_of(2);
    let mut v = Vec::with_capacity(2 + 8 + 4 + padded + 2);
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&a.addr.to_le_bytes());
    v.extend_from_slice(b"AC");
    v.extend_from_slice(&(padded as u16).to_le_bytes());
    v.extend_from_slice(ac);
    v.resize(v.len() + (padded - ac.len()), 0);
    v.extend_from_slice(b"XX");
    v
}

pub(crate) mod req {
    use super::*;

    pub fn attach() -> Draft {
        Draft::new(RAPI_CMD_ATTACH, T32_DEV_ICD, LenRule::Auto, Vec::new())
    }

    fn practice_text(opt: u8, text: &str) -> Result<Draft, LocalError> {
        check_text(text)?;
        let mut p = REPLY_BUFFER.to_le_bytes().to_vec();
        p.extend_from_slice(text.as_bytes());
        p.push(0);
        Ok(Draft::new(RAPI_CMD_EXECUTE_PRACTICE, opt, LenRule::Auto, p))
    }

    pub fn execute_command(text: &str) -> Result<Draft, LocalError> {
        practice_text(PRACTICE_EXECUTECOMMAND, text)
    }

    pub fn execute_function(expr: &str) -> Result<Draft, LocalError> {
        practice_text(PRACTICE_EXECUTEFUNCTION, expr)
    }

    pub fn practice_state() -> Draft {
        Draft::new(RAPI_CMD_EXECUTE_PRACTICE, PRACTICE_GETSTATE, LenRule::Auto, Vec::new())
    }

    pub fn stop() -> Draft {
        Draft::new(RAPI_CMD_EXECUTE_PRACTICE, PRACTICE_STOP, LenRule::Auto, Vec::new())
    }

    pub fn target_state() -> Draft {
        Draft::new(RAPI_CMD_DEVICE_SPECIFIC, DS_GETSTATE, LenRule::Auto, Vec::new())
    }

    pub fn eval_get_string() -> Draft {
        Draft::new(RAPI_CMD_DEVICE_SPECIFIC, DS_EVAL_GETSTRING, LenRule::Auto, Vec::new())
    }

    pub fn mem_read(at: &MemAddr, len: u16) -> Draft {
        let obj = addr_object(at);
        let mut p = len.to_le_bytes().to_vec();
        p.extend_from_slice(&obj);
        Draft::new(RAPI_CMD_DEVICE_SPECIFIC, DS_MEMORY_OBJ_READ, LenRule::Forced(6 + obj.len()), p)
    }

    /// `data.len()` must fit a u16; `Session` chunks below `Tuning::MAX_CHUNK`.
    pub fn mem_write(at: &MemAddr, data: &[u8]) -> Draft {
        let obj = addr_object(at);
        let mut p = (data.len() as u16).to_le_bytes().to_vec();
        p.extend_from_slice(&obj);
        p.extend_from_slice(data);
        Draft::new(RAPI_CMD_DEVICE_SPECIFIC, DS_MEMORY_OBJ_WRITE, LenRule::Forced(6 + obj.len()), p)
    }

    fn exp(cmd: u16, payload: &[u8]) -> Draft {
        let mut p = cmd.to_le_bytes().to_vec();
        p.extend_from_slice(payload);
        Draft::new(RAPI_CMD_DEVICE_SPECIFIC, DS_EXP, LenRule::ExpRaw, p)
    }

    pub fn macro_get(name: &MacroName, spelling: MacroSpelling) -> Draft {
        let wire = name.wire(spelling);
        let n = wire.as_bytes();
        let mut p = Vec::with_capacity(4 + n.len());
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&(n.len() as u16).to_le_bytes());
        p.extend_from_slice(n);
        exp(EXP_PRACTICE_MACRO, &p)
    }

    pub fn macro_set(name: &MacroName, spelling: MacroSpelling, value: &str) -> Result<Draft, LocalError> {
        let wire = name.wire(spelling);
        let (n, v) = (wire.as_bytes(), value.as_bytes());
        let max = MAX_MESSAGE - 8 - n.len();
        if v.len() > max {
            return Err(LocalError::ValueTooLarge { len: v.len(), max });
        }
        let mut p = Vec::with_capacity(6 + n.len() + v.len());
        p.extend_from_slice(&1u16.to_le_bytes());
        p.extend_from_slice(&(n.len() as u16).to_le_bytes());
        p.extend_from_slice(n);
        p.extend_from_slice(&(v.len() as u16).to_le_bytes());
        p.extend_from_slice(v);
        Ok(exp(EXP_PRACTICE_MACRO, &p))
    }
}

/// Status byte, T32 name, default meaning. Values and names: pyrcl `_library.py` error table.
const STATUS_CODES: &[(u8, &str, RemoteKind)] = &[
    (2, "STD_RUNNING", RemoteKind::TargetState),
    (3, "STD_NOTRUNNING", RemoteKind::TargetState),
    (4, "STD_RESET", RemoteKind::TargetState),
    (6, "STD_ACCESSTIMEOUT", RemoteKind::MemoryAccess),
    (10, "STD_INVALID", RemoteKind::Other),
    (14, "STD_REGUNDEF", RemoteKind::Other),
    (15, "STD_VERIFY", RemoteKind::MemoryAccess),
    (16, "STD_BUSERROR", RemoteKind::MemoryAccess),
    (22, "STD_NOMEM", RemoteKind::MemoryAccess),
    (48, "STD_RESETDETECTED", RemoteKind::TargetState),
    (60, "STD_INVALIDLICENSE", RemoteKind::Attach),
    (64, "STD_CORENOTACTIVE", RemoteKind::TargetState),
    (83, "STD_NORAPI", RemoteKind::Attach),
    (90, "FN1", RemoteKind::Other),
    (91, "FN2", RemoteKind::Other),
    (92, "FN3", RemoteKind::Other),
    (93, "FN4", RemoteKind::Other),
    (113, "STD_FAILED", RemoteKind::Other),
    (123, "STD_LOCKED", RemoteKind::Locked),
    (128, "STD_POWERFAIL", RemoteKind::TargetState),
    (140, "STD_DEBUGPORTFAIL", RemoteKind::TargetState),
    (144, "STD_DEBUGPORTTIMEOUT", RemoteKind::TargetState),
    (147, "STD_NODEVICE", RemoteKind::Attach),
    (161, "STD_RESETFAIL", RemoteKind::TargetState),
    (162, "STD_EMUTIMEOUT", RemoteKind::TargetState),
    (164, "STD_NORTCK", RemoteKind::TargetState),
    (254, "STD_ATTACH", RemoteKind::Attach),
    (255, "STD_FATAL", RemoteKind::Other),
];

pub(crate) fn status_name(code: u8) -> Option<&'static str> {
    STATUS_CODES.iter().find(|(c, ..)| *c == code).map(|(_, n, _)| *n)
}

fn remote_kind(op: Op, code: u8) -> RemoteKind {
    // FN1 means "this request failed"; its meaning depends on the request.
    if code == 90 {
        return match op {
            Op::Command | Op::EvalString => RemoteKind::CommandFailed,
            Op::Function => RemoteKind::FunctionFailed,
            Op::MemRead | Op::MemWrite => RemoteKind::MemoryAccess,
            _ => RemoteKind::Other,
        };
    }
    STATUS_CODES.iter().find(|(c, ..)| *c == code).map_or(RemoteKind::Other, |(.., k)| *k)
}

/// Text from PowerView: NULs trimmed, decoded lossily. `bool` is true when bytes were replaced.
pub(crate) fn text(bytes: &[u8]) -> (String, bool) {
    let lossy = std::str::from_utf8(bytes).is_err();
    (String::from_utf8_lossy(bytes).trim_matches('\0').to_string(), lossy)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExpReply {
    pub error_code: Option<u64>,
    pub error_message: Option<String>,
    pub payload: Option<Vec<u8>>,
}

/// Either a failed function or a malformed reply.
#[derive(Debug)]
pub(crate) enum FunctionReply {
    Remote(RemoteError),
    Violation(Violation),
}

pub(crate) mod resp {
    use super::*;

    /// Split `[err][id][result...]`. `msg.len() >= 2` is guaranteed by `wait::classify`.
    pub fn status(op: Op, msg: &[u8]) -> Result<&[u8], RemoteError> {
        let err = msg[0];
        if err == 0 {
            return Ok(&msg[2..]);
        }
        let message = (msg.len() > 10)
            .then(|| {
                let n = u32::from_le_bytes([msg[6], msg[7], msg[8], msg[9]]) as usize;
                text(&msg[10..(10 + n).min(msg.len())]).0
            })
            .filter(|s| !s.is_empty());
        Err(RemoteError { op, kind: remote_kind(op, err), code: err, message })
    }

    /// Result type 0 is a failed function whose text is PowerView's message. The `bool` reports
    /// lossy text decoding.
    pub fn function(result: &[u8]) -> Result<(FuncValue, bool), FunctionReply> {
        if result.len() < 8 {
            return Err(FunctionReply::Violation(Violation::ShortResponse { wanted: 8, got: result.len() }));
        }
        let ty = u32::from_le_bytes([result[0], result[1], result[2], result[3]]);
        let n = u32::from_le_bytes([result[4], result[5], result[6], result[7]]) as usize;
        let (body, lossy) = text(&result[8..(8 + n).min(result.len())]);
        if ty == 0 {
            let message = (!body.is_empty()).then_some(body);
            return Err(FunctionReply::Remote(RemoteError {
                op: Op::Function,
                kind: RemoteKind::FunctionFailed,
                code: 0,
                message,
            }));
        }
        Ok((decode_value(ty, &body), lossy))
    }

    /// Type words: 1 bool, 2 binary `0y..`, 4 hex `0x..`, 8 decimal `12.`, 0x10 float (`SNAN` is
    /// NaN), 0x400 time `1.5s`, 0x8000 empty; 0x20, 0x40, 0x80, 0x100, 0x200, 0x800, 0x4000 stay
    /// text (rcl.py `_decode_eval_result`). A text its type word does not parse becomes `Raw`.
    pub fn decode_value(ty: u32, text: &str) -> FuncValue {
        let t = text.trim();
        let parsed = match ty {
            0x0001 => match t {
                "TRUE()" => Some(FuncValue::Bool(true)),
                "FALSE()" => Some(FuncValue::Bool(false)),
                _ => None,
            },
            0x0002 => t.strip_prefix("0y").and_then(|d| u64::from_str_radix(d, 2).ok()).map(FuncValue::Uint),
            0x0004 => t.strip_prefix("0x").and_then(|d| u64::from_str_radix(d, 16).ok()).map(FuncValue::Uint),
            0x0008 => t.strip_suffix('.').unwrap_or(t).parse().ok().map(FuncValue::Int),
            0x0010 => match t {
                "SNAN" | "-SNAN" => Some(FuncValue::Float(f64::NAN)),
                _ => t.parse().ok().map(FuncValue::Float),
            },
            0x0400 => t.strip_suffix('s').and_then(|v| v.parse().ok()).map(FuncValue::Seconds),
            0x0020 | 0x0040 | 0x0080 | 0x0100 | 0x0200 | 0x0800 | 0x4000 => Some(FuncValue::Text(text.to_string())),
            0x8000 => Some(FuncValue::Empty),
            _ => None,
        };
        parsed.unwrap_or_else(|| FuncValue::Raw { ty, text: text.to_string() })
    }

    pub fn practice_state(result: &[u8]) -> Result<PracticeState, Violation> {
        match result.first() {
            Some(0) => Ok(PracticeState::Idle),
            Some(1) => Ok(PracticeState::Running),
            Some(2) => Ok(PracticeState::Dialog),
            Some(&b) => Ok(PracticeState::Other(b)),
            None => Err(Violation::ShortResponse { wanted: 1, got: 0 }),
        }
    }

    pub fn target_state(result: &[u8]) -> Result<TargetState, Violation> {
        match result.first() {
            Some(0) => Ok(TargetState::Down),
            Some(1) => Ok(TargetState::Halted),
            Some(2) => Ok(TargetState::Stopped),
            Some(3) => Ok(TargetState::Running),
            Some(&b) => Ok(TargetState::Other(b)),
            None => Err(Violation::ShortResponse { wanted: 1, got: 0 }),
        }
    }

    /// The first `want` bytes of the result. Longer replies (padding) are cut.
    pub fn mem_read(result: &[u8], want: usize) -> Result<&[u8], Violation> {
        result.get(..want).ok_or(Violation::ShortResponse { wanted: want, got: result.len() })
    }

    /// `result` is `msg[2..]`: `[resp_len u16][resp_cmd u16][blocks...]`.
    pub fn exp(result: &[u8]) -> Result<ExpReply, Violation> {
        if result.len() < 4 {
            return Err(Violation::ExpMalformed("shorter than its 4-byte header"));
        }
        let mut reply = ExpReply { error_code: None, error_message: None, payload: None };
        let mut pos = 4;
        while let Some(tag) = result.get(pos..pos + 2) {
            if tag == [0, 0] {
                break;
            }
            let Some(size) = result.get(pos + 2..pos + 4) else { break };
            let start = pos + 4;
            let end = (start + u16::from_le_bytes([size[0], size[1]]) as usize).min(result.len());
            let content = &result[start..end];
            match tag {
                b"EC" => reply.error_code = Some(content.iter().rev().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))),
                b"EM" => reply.error_message = Some(text(content).0),
                b"PL" => reply.payload = Some(content.to_vec()),
                _ => break,
            }
            pos = end;
        }
        Ok(reply)
    }

    /// EXP 2 get: 2 bytes, then the value. NULs trimmed.
    pub fn macro_value(payload: &[u8]) -> Result<(String, bool), Violation> {
        let value = payload.get(2..).ok_or(Violation::ShortResponse { wanted: 2, got: payload.len() })?;
        Ok(text(value))
    }
}

#[cfg(test)]
mod golden {
    //! Request vectors produced by pyrcl 1.1.6's own `Library` serializers on a stub link
    //! (`tools/golden/gen.py`). Response vectors are laid out per t32api.rs and _library.py.
    use super::*;
    use crate::{frame, value::AccessClass};

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).expect("hex byte")).collect()
    }

    fn seal(d: Draft, id: u8) -> Vec<u8> {
        d.seal(id).expect("sealable")
    }

    #[test]
    fn attach_message_and_frame() {
        let msg = seal(req::attach(), 1);
        assert_eq!(msg, hex("02 71 01 01"));
        assert_eq!(frame::encode_request(&msg), hex("04 00 00 00 10 00 00 00 02 71 01 01 00 00 00 00"));
    }

    #[test]
    fn execute_command_and_function() {
        let cmd = seal(req::execute_command("PRINT 1").expect("valid"), 7);
        assert_eq!(cmd, hex("0e 72 04 07 00 10 00 00 50 52 49 4e 54 20 31 00"));
        let fnc = seal(req::execute_function("VERSION.BUILD()").expect("valid"), 1);
        assert_eq!(fnc, hex("16 72 05 01 00 10 00 00 56 45 52 53 49 4f 4e 2e 42 55 49 4c 44 28 29 00"));
    }

    #[test]
    fn simple_requests() {
        assert_eq!(seal(req::practice_state(), 1), hex("02 72 03 01"));
        assert_eq!(seal(req::stop(), 1), hex("02 72 00 01"));
        assert_eq!(seal(req::target_state(), 1), hex("02 74 10 01"));
        assert_eq!(seal(req::eval_get_string(), 1), hex("02 74 17 01"));
    }

    #[test]
    fn mem_read_e_1000_256() {
        let at = MemAddr::new(AccessClass::parse("E").expect("valid"), 0x1000);
        let msg = seal(req::mem_read(&at, 256), 7);
        assert_eq!(msg, hex("18 74 35 07 00 01 03 00 00 10 00 00 00 00 00 00 41 43 02 00 45 00 58 58"));
    }

    #[test]
    fn mem_write_odd_length_pads_the_message_not_the_length_field() {
        let msg = seal(req::mem_write(&MemAddr::vm(0x0100_0000), &[0, 1, 2, 3, 4]), 7);
        assert_eq!(
            msg,
            hex("1a 74 36 07 05 00 03 00 00 00 00 01 00 00 00 00 41 43 04 00 56 4d 00 00 58 58 00 01 02 03 04 00")
        );
    }

    #[test]
    fn exp_macro_get_and_set() {
        let hello = MacroName::new("hello").expect("valid");
        assert_eq!(
            seal(req::macro_get(&hello, MacroSpelling::Bare), 1),
            hex("00 74 fe 01 0b 00 02 00 00 00 05 00 68 65 6c 6c 6f 00")
        );
        let abc = MacroName::new("abc").expect("valid");
        assert_eq!(
            seal(req::macro_set(&abc, MacroSpelling::Bare, "xy").expect("valid"), 1),
            hex("00 74 fe 01 0d 00 02 00 01 00 03 00 61 62 63 02 00 78 79 00")
        );
        assert_eq!(
            seal(req::macro_get(&abc, MacroSpelling::Ampersand), 1),
            hex("00 74 fe 01 0a 00 02 00 00 00 04 00 26 61 62 63")
        );
    }

    #[test]
    fn long_command_switches_to_the_wide_header() {
        let msg = seal(req::execute_command(&"A".repeat(300)).expect("valid"), 1);
        assert_eq!(&msg[..8], &hex("00 72 04 01 35 01 00 10")[..]);
    }

    #[test]
    fn invalid_text_is_refused_before_sending() {
        assert!(matches!(
            req::execute_command(&"A".repeat(MAX_PRACTICE_TEXT + 1)),
            Err(LocalError::TextTooLong { .. })
        ));
        assert_eq!(req::execute_command("A\0B"), Err(LocalError::EmbeddedNul));
    }

    #[test]
    fn error_status_carries_the_message_at_offset_ten() {
        let msg = hex("5a 03 00 00 00 00 05 00 00 00 62 61 64 21 00");
        let e = resp::status(Op::Command, &msg).expect_err("status 90");
        assert_eq!(e.kind, RemoteKind::CommandFailed);
        assert_eq!(e.message.as_deref(), Some("bad!"));
        let e = resp::status(Op::MemRead, &hex("10 04")).expect_err("bus error");
        assert_eq!((e.kind, e.message), (RemoteKind::MemoryAccess, None));
        assert_eq!(resp::status(Op::Stop, &hex("00 04 aa")), Ok(&[0xaa][..]));
    }

    fn function_result(ty: u32, text: &str) -> Vec<u8> {
        let mut v = ty.to_le_bytes().to_vec();
        v.extend_from_slice(&(text.len() as u32 + 1).to_le_bytes());
        v.extend_from_slice(text.as_bytes());
        v.push(0);
        v
    }

    #[test]
    fn function_results_decode_by_type_word() {
        let cases = [
            (1, "TRUE()", FuncValue::Bool(true)),
            (1, "FALSE()", FuncValue::Bool(false)),
            (2, "0y101", FuncValue::Uint(5)),
            (4, "0x1F", FuncValue::Uint(0x1F)),
            (8, "172803.", FuncValue::Int(172_803)),
            (8, "-5.", FuncValue::Int(-5)),
            (0x10, "2.5", FuncValue::Float(2.5)),
            (0x400, "1.5s", FuncValue::Seconds(1.5)),
            (0x40, "C:\\Temp", FuncValue::Text("C:\\Temp".into())),
            (0x8000, "", FuncValue::Empty),
            (8, "nonsense", FuncValue::Raw { ty: 8, text: "nonsense".into() }),
            (0x9999, "x", FuncValue::Raw { ty: 0x9999, text: "x".into() }),
        ];
        for (ty, text, want) in cases {
            let (got, lossy) = resp::function(&function_result(ty, text)).expect("decodes");
            assert_eq!(got, want, "type {ty:#x} {text:?}");
            assert!(!lossy);
        }
        let (nan, _) = resp::function(&function_result(0x10, "SNAN")).expect("decodes");
        assert!(matches!(nan, FuncValue::Float(v) if v.is_nan()));
    }

    #[test]
    fn function_type_zero_is_a_failure_with_text() {
        match resp::function(&function_result(0, "unknown function")) {
            Err(FunctionReply::Remote(e)) => {
                assert_eq!(e.kind, RemoteKind::FunctionFailed);
                assert_eq!(e.message.as_deref(), Some("unknown function"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            resp::function(&[1, 0, 0]),
            Err(FunctionReply::Violation(Violation::ShortResponse { wanted: 8, got: 3 }))
        ));
    }

    #[test]
    fn state_and_memory_results() {
        assert_eq!(resp::practice_state(&[2]), Ok(PracticeState::Dialog));
        assert_eq!(resp::practice_state(&[7]), Ok(PracticeState::Other(7)));
        assert!(resp::practice_state(&[]).is_err());
        assert_eq!(resp::target_state(&[3]), Ok(TargetState::Running));
        assert!(resp::target_state(&[]).is_err());
        assert_eq!(resp::mem_read(&[1, 2, 3, 0], 3), Ok(&[1, 2, 3][..]));
        assert_eq!(resp::mem_read(&[1], 3), Err(Violation::ShortResponse { wanted: 3, got: 1 }));
    }

    #[test]
    fn non_utf8_text_is_decoded_lossily_and_flagged() {
        let mut v = 0x40u32.to_le_bytes().to_vec();
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&[b'C', 0xB0, 0]);
        let (value, lossy) = resp::function(&v).expect("decodes");
        assert!(lossy);
        assert_eq!(value, FuncValue::Text("C\u{FFFD}".into()));
    }

    #[test]
    fn exp_blocks() {
        let ok = hex("10 00 02 00 50 4c 05 00 00 00 61 62 63 00 00");
        let r = resp::exp(&ok).expect("parses");
        assert_eq!(r.payload.as_deref(), Some(&b"\0\0abc"[..]));
        assert_eq!(resp::macro_value(r.payload.as_deref().expect("payload")), Ok(("abc".to_string(), false)));

        let failed = hex("10 00 02 00 45 43 02 00 07 01 45 4d 04 00 6e 6f 70 65 00 00");
        let r = resp::exp(&failed).expect("parses");
        assert_eq!((r.error_code, r.error_message.as_deref(), r.payload), (Some(0x107), Some("nope"), None));

        assert!(resp::exp(&hex("10 00")).is_err());
        let unknown = resp::exp(&hex("10 00 02 00 5a 5a 01 00 00 50 4c 00 00")).expect("stops at unknown tag");
        assert_eq!(unknown.payload, None);
    }
}
