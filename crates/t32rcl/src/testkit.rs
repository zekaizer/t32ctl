// SPDX-License-Identifier: Apache-2.0
//! Hardware-free test support (feature `testkit`).
//!
//! Canned frames at the `Transport` boundary, not a PowerView stand-in: a `Script` is a strict
//! sequence of "expect this request / send these bytes" steps with no PRACTICE semantics.
//! Requests are decoded and responses encoded here, independently of `wire`, so a decoder bug
//! cannot be mirrored by the matching test encoder.

use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{Clock, ConnectError, Connector, Endpoint, Transport};

/// Clock that moves only when told to. `Script::silence` advances it.
#[derive(Debug, Clone)]
pub struct ManualClock {
    base: Instant,
    offset: Arc<Mutex<Duration>>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self { base: Instant::now(), offset: Arc::new(Mutex::new(Duration::ZERO)) }
    }

    pub fn advance(&self, d: Duration) {
        *self.offset.lock().expect("clock lock") += d;
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().expect("clock lock")
    }
}

/// A request as the scripted peer decoded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Attach,
    Command(String),
    Function(String),
    PracticeState,
    Stop,
    TargetState,
    EvalString,
    MemRead { access: String, addr: u64, len: u16 },
    MemWrite { access: String, addr: u64, data: Vec<u8> },
    MacroGet { name: String },
    MacroSet { name: String, value: String },
    Other { cmd: u8, opt: u8 },
}

/// What the next request must be.
#[derive(Debug, Clone)]
pub enum Expect {
    Exactly(Request),
    /// A command whose text starts with this prefix (volatile tails such as tokens).
    CommandStarting(String),
    /// A memory write to this address with any data of this length.
    MemWriteLen {
        access: String,
        addr: u64,
        len: usize,
    },
    Any,
}

impl Expect {
    fn accepts(&self, got: &Request) -> bool {
        match (self, got) {
            (Expect::Exactly(want), got) => want == got,
            (Expect::CommandStarting(p), Request::Command(t)) => t.starts_with(p.as_str()),
            (Expect::MemWriteLen { access, addr, len }, Request::MemWrite { access: a, addr: d, data }) => {
                access == a && addr == d && *len == data.len()
            }
            (Expect::Any, _) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
enum Step {
    Expect(Expect),
    /// RCL response message; byte `[1]` is replaced by the id of the last request.
    Reply(Vec<u8>),
    /// Bytes on the stream as given, at most `chunk` per read.
    Raw {
        bytes: Vec<u8>,
        chunk: Option<usize>,
    },
    Silence(Duration),
    Eof,
    Reset,
    FailWrite(io::ErrorKind),
}

/// Builder for one connection's conversation.
#[derive(Debug, Clone, Default)]
pub struct Script {
    steps: Vec<Step>,
}

fn rcl_text(text: &str) -> Vec<u8> {
    let mut v = text.as_bytes().to_vec();
    v.push(0);
    v
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn expect(mut self, e: Expect) -> Self {
        self.steps.push(Step::Expect(e));
        self
    }

    pub fn expect_req(self, r: Request) -> Self {
        self.expect(Expect::Exactly(r))
    }

    pub fn expect_cmd(self, text: &str) -> Self {
        self.expect_req(Request::Command(text.to_string()))
    }

    pub fn expect_fn(self, expr: &str) -> Self {
        self.expect_req(Request::Function(expr.to_string()))
    }

    fn reply(mut self, msg: Vec<u8>) -> Self {
        self.steps.push(Step::Reply(msg));
        self
    }

    /// `[0][id]`.
    pub fn reply_ok(self) -> Self {
        self.reply(vec![0, 0])
    }

    /// Attach expectation and its OK reply.
    pub fn attach(self) -> Self {
        self.expect_req(Request::Attach).reply_ok()
    }

    pub fn reply_function(self, result_type: u32, text: &str) -> Self {
        let body = rcl_text(text);
        let mut m = vec![0, 0];
        m.extend_from_slice(&result_type.to_le_bytes());
        m.extend_from_slice(&(body.len() as u32).to_le_bytes());
        m.extend_from_slice(&body);
        self.reply(m)
    }

    pub fn reply_bool(self, v: bool) -> Self {
        self.reply_function(1, if v { "TRUE()" } else { "FALSE()" })
    }

    pub fn reply_decimal(self, v: i64) -> Self {
        self.reply_function(8, &format!("{v}."))
    }

    pub fn reply_text(self, text: &str) -> Self {
        self.reply_function(0x40, text)
    }

    /// `[code][id]`, 4 bytes, `u32 n`, message.
    pub fn reply_error(self, code: u8, message: &str) -> Self {
        let mut m = vec![code, 0, 0, 0, 0, 0];
        m.extend_from_slice(&(message.len() as u32).to_le_bytes());
        m.extend_from_slice(message.as_bytes());
        self.reply(m)
    }

    pub fn reply_state(self, state: u8) -> Self {
        self.reply(vec![0, 0, state])
    }

    pub fn reply_mem(self, data: &[u8]) -> Self {
        let mut m = vec![0, 0];
        m.extend_from_slice(data);
        self.reply(m)
    }

    /// EvalGetString answer.
    pub fn reply_string(self, text: &str) -> Self {
        let mut m = vec![0, 0];
        m.extend_from_slice(&rcl_text(text));
        self.reply(m)
    }

    fn exp_reply(self, status: u8, blocks: &[(&[u8; 2], &[u8])]) -> Self {
        let mut body = Vec::new();
        for (tag, content) in blocks {
            body.extend_from_slice(*tag);
            body.extend_from_slice(&(content.len() as u16).to_le_bytes());
            body.extend_from_slice(content);
        }
        let mut m = vec![status, 0];
        m.extend_from_slice(&((body.len() + 4) as u16).to_le_bytes());
        m.extend_from_slice(&2u16.to_le_bytes());
        m.extend_from_slice(&body);
        self.reply(m)
    }

    /// EXP 2 answer: `PL` = 2 filler bytes + value. Also the answer to a macro set.
    pub fn reply_macro(self, value: &str) -> Self {
        let mut pl = vec![0, 0];
        pl.extend_from_slice(value.as_bytes());
        self.exp_reply(0, &[(b"PL", &pl)])
    }

    /// EXP failure: `EC` and `EM`, no `PL`, status byte 0.
    pub fn reply_exp_error(self, code: u16, message: &str) -> Self {
        self.reply_exp_failed(0, code, message)
    }

    /// EXP failure with a non-zero status byte: `EC` and `EM` still follow at `[6..]`.
    pub fn reply_exp_failed(self, status: u8, code: u16, message: &str) -> Self {
        self.exp_reply(status, &[(b"EC", &code.to_le_bytes()), (b"EM", message.as_bytes())])
    }

    /// `n` keepalive RESP frames, each `[0xFE, 0]`.
    pub fn keepalive(mut self, n: usize) -> Self {
        for _ in 0..n {
            self.steps.push(Step::Raw { bytes: frame(0x11, &[0xFE, 0]), chunk: None });
        }
        self
    }

    pub fn notify(mut self, payload: &[u8]) -> Self {
        self.steps.push(Step::Raw { bytes: frame(0x12, payload), chunk: None });
        self
    }

    /// Raw bytes, `chunk` bytes per read at most.
    pub fn raw(mut self, bytes: &[u8], chunk: Option<usize>) -> Self {
        self.steps.push(Step::Raw { bytes: bytes.to_vec(), chunk });
        self
    }

    /// A response message with an explicit id (no patching), framed.
    pub fn reply_with_id(self, msg: &[u8]) -> Self {
        self.raw(&frame(0x11, msg), None)
    }

    /// `read` times out after advancing the manual clock by `d`.
    pub fn silence(mut self, d: Duration) -> Self {
        self.steps.push(Step::Silence(d));
        self
    }

    pub fn eof(mut self) -> Self {
        self.steps.push(Step::Eof);
        self
    }

    pub fn reset(mut self) -> Self {
        self.steps.push(Step::Reset);
        self
    }

    /// The next `write_all` fails with this kind instead of being read.
    pub fn fail_write(mut self, kind: io::ErrorKind) -> Self {
        self.steps.push(Step::FailWrite(kind));
        self
    }
}

fn frame(ty: u32, msg: &[u8]) -> Vec<u8> {
    let mut out = (msg.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(msg);
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
    out
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Decode a request frame without using `wire`.
fn decode_request(bytes: &[u8]) -> (u8, Request) {
    assert!(bytes.len() >= 8 && bytes.len().is_multiple_of(8), "request frame of {} bytes", bytes.len());
    let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    assert_eq!(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]), 0x10, "frame type");
    let msg = &bytes[8..8 + len];
    let (cmd, opt, id) = (msg[1], msg[2], msg[3]);
    let p = if msg[0] == 0 { &msg[6..] } else { &msg[4..] };
    let addr = |p: &[u8]| {
        assert_eq!(le16(p, 2), 3, "address type A64");
        let addr = u64::from_le_bytes(p[4..12].try_into().expect("8 bytes"));
        assert_eq!(&p[12..14], b"AC");
        let n = le16(p, 14) as usize;
        let access = cstr(&p[16..16 + n]);
        assert_eq!(&p[16 + n..18 + n], b"XX");
        (access, addr, 18 + n)
    };
    let req = match (cmd, opt) {
        (0x71, _) => Request::Attach,
        (0x72, 0x04) => Request::Command(cstr(&p[4..])),
        (0x72, 0x05) => Request::Function(cstr(&p[4..])),
        (0x72, 0x03) => Request::PracticeState,
        (0x72, 0x00) => Request::Stop,
        (0x74, 0x10) => Request::TargetState,
        (0x74, 0x17) => Request::EvalString,
        (0x74, 0x35) => {
            let (access, addr, _) = addr(p);
            Request::MemRead { access, addr, len: le16(p, 0) }
        }
        (0x74, 0x36) => {
            let n = le16(p, 0) as usize;
            let (access, addr, end) = addr(p);
            Request::MemWrite { access, addr, data: p[end..end + n].to_vec() }
        }
        (0x74, 0xFE) => {
            assert_eq!(le16(p, 0), 2, "only EXP 2 is scripted");
            let n = le16(p, 4) as usize;
            let name = String::from_utf8_lossy(&p[6..6 + n]).into_owned();
            match le16(p, 2) {
                0 => Request::MacroGet { name },
                _ => {
                    let m = le16(p, 6 + n) as usize;
                    Request::MacroSet { name, value: String::from_utf8_lossy(&p[8 + n..8 + n + m]).into_owned() }
                }
            }
        }
        (cmd, opt) => Request::Other { cmd, opt },
    };
    (id, req)
}

/// Executes a `Script`. A wrong or extra request panics with the step index; steps left over
/// when the transport is dropped panic too.
#[derive(Debug)]
pub struct ScriptedTransport {
    steps: VecDeque<Step>,
    clock: ManualClock,
    pending: VecDeque<u8>,
    read_limit: Option<usize>,
    last_id: u8,
    index: usize,
}

impl ScriptedTransport {
    pub fn new(script: Script, clock: ManualClock) -> Self {
        Self { steps: script.steps.into(), clock, pending: VecDeque::new(), read_limit: None, last_id: 0, index: 0 }
    }

    fn pop(&mut self) -> Option<Step> {
        self.index += 1;
        self.steps.pop_front()
    }
}

impl Transport for ScriptedTransport {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        let (id, req) = decode_request(buf);
        match self.pop() {
            Some(Step::FailWrite(kind)) => Err(io::Error::from(kind)),
            Some(Step::Expect(e)) => {
                assert!(e.accepts(&req), "step {}: expected {e:?}, client sent {req:?}", self.index);
                self.last_id = id;
                Ok(())
            }
            other => panic!("step {}: client sent {req:?} while the script holds {other:?}", self.index),
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if !self.pending.is_empty() {
                let n = buf.len().min(self.pending.len()).min(self.read_limit.unwrap_or(usize::MAX));
                for (slot, b) in buf.iter_mut().zip(self.pending.drain(..n)) {
                    *slot = b;
                }
                return Ok(n);
            }
            match self.pop() {
                Some(Step::Reply(mut msg)) => {
                    msg[1] = self.last_id;
                    self.pending.extend(frame(0x11, &msg));
                    self.read_limit = None;
                }
                Some(Step::Raw { bytes, chunk }) => {
                    self.pending.extend(bytes);
                    self.read_limit = chunk;
                }
                Some(Step::Silence(d)) => {
                    self.clock.advance(d);
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
                Some(Step::Eof) => return Ok(0),
                Some(Step::Reset) => return Err(io::Error::from(io::ErrorKind::ConnectionReset)),
                other => panic!("step {}: client reads while the script holds {other:?}", self.index),
            }
        }
    }

    fn set_read_timeout(&mut self, _timeout: Duration) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for ScriptedTransport {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            assert!(self.steps.is_empty(), "script ended with unused steps: {:?}", self.steps);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConnectFailure {
    Refused,
    TimedOut,
}

/// Hands out one scripted transport (or one connect failure) per `open`, in order.
#[derive(Debug, Default)]
pub struct ScriptedConnector {
    queue: Mutex<VecDeque<Result<Script, ConnectFailure>>>,
    clock: ManualClock,
}

impl ScriptedConnector {
    pub fn new(clock: ManualClock) -> Self {
        Self { queue: Mutex::default(), clock }
    }

    pub fn push(self, next: Result<Script, ConnectFailure>) -> Self {
        self.queue.lock().expect("queue lock").push_back(next);
        self
    }
}

impl Connector for ScriptedConnector {
    fn open(&self, endpoint: &Endpoint, timeout: Duration) -> Result<Box<dyn Transport>, ConnectError> {
        let next = self.queue.lock().expect("queue lock").pop_front().expect("unexpected connect attempt");
        let endpoint = endpoint.clone();
        match next {
            Ok(script) => Ok(Box::new(ScriptedTransport::new(script, self.clock.clone()))),
            Err(ConnectFailure::Refused) => Err(ConnectError::Refused { endpoint }),
            Err(ConnectFailure::TimedOut) => Err(ConnectError::TimedOut { endpoint, after: timeout }),
        }
    }
}
