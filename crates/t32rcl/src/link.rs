// SPDX-License-Identifier: Apache-2.0
// Receive loop ported from t32mcp src/t32api.rs (Apache-2.0, (c) 2026 Lauterbach GmbH) and
// pyrcl 1.1.6 _rc/hlinknet.py (MIT, (c) 2020 Lauterbach GmbH). See NOTICE.
//! Blocking I/O around the sans-IO core: one request in flight, deadlines, keepalive, poison.

use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    error::{CancelReason, ConnectError, Error, LostCause, Op, PoisonCause, Violation},
    frame::{self, FrameKind, Framer},
    value::{CallBudget, Endpoint},
    wait::{self, Incoming, Waiter},
    wire::Draft,
};

/// A byte stream with a settable read timeout: `TcpStream` in production, scripted in tests.
/// Contract: `read` returns `Ok(0)` only at EOF, and `Err(TimedOut | WouldBlock)` when the read
/// timeout elapsed without data.
pub trait Transport: Send + std::fmt::Debug {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()>;
}

/// Opens transports. `Session::try_revive` calls it again; tests hand out scripted transports.
pub trait Connector: Send + Sync + std::fmt::Debug {
    fn open(&self, endpoint: &Endpoint, timeout: Duration) -> Result<Box<dyn Transport>, ConnectError>;
}

/// `TcpStream` with `TCP_NODELAY`, connected with `connect_timeout` per resolved address.
#[derive(Debug, Default)]
pub struct TcpConnector;

#[derive(Debug)]
struct TcpTransport(TcpStream);

/// Bounds a write to a peer that stopped reading.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

impl Transport for TcpTransport {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }

    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.0.set_read_timeout(Some(timeout))
    }
}

impl Connector for TcpConnector {
    fn open(&self, endpoint: &Endpoint, timeout: Duration) -> Result<Box<dyn Transport>, ConnectError> {
        let addrs: Vec<SocketAddr> = (endpoint.host.as_str(), endpoint.port)
            .to_socket_addrs()
            .map_err(|source| ConnectError::Resolve { endpoint: endpoint.clone(), source })?
            .collect();
        let mut last = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(stream) => {
                    let io = |source| ConnectError::Io { endpoint: endpoint.clone(), source };
                    stream.set_nodelay(true).map_err(io)?;
                    stream.set_write_timeout(Some(WRITE_TIMEOUT)).map_err(io)?;
                    return Ok(Box::new(TcpTransport(stream)));
                }
                Err(e) => last = Some(classify_connect(endpoint, timeout, e)),
            }
        }
        Err(last.unwrap_or_else(|| ConnectError::Resolve {
            endpoint: endpoint.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, "no address"),
        }))
    }
}

pub(crate) fn classify_connect(endpoint: &Endpoint, timeout: Duration, e: io::Error) -> ConnectError {
    let endpoint = endpoint.clone();
    match e.kind() {
        io::ErrorKind::ConnectionRefused => ConnectError::Refused { endpoint },
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ConnectError::TimedOut { endpoint, after: timeout },
        kind @ (io::ErrorKind::HostUnreachable
        | io::ErrorKind::NetworkUnreachable
        | io::ErrorKind::AddrNotAvailable
        | io::ErrorKind::NetworkDown) => ConnectError::Unreachable { endpoint, kind },
        _ => ConnectError::Io { endpoint, source: e },
    }
}

/// Time source. Only `Link` reads it, so deadline tests need no sleeping.
pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now(&self) -> Instant;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Cooperative stop for Ctrl-C and deadlines. Clones share the flag; the deadline belongs to
/// one clone. `Link` checks it before sending and between read slices, so a blocked call returns
/// within `CANCEL_SLICE` and no request leaves after the stop.
#[derive(Clone, Debug, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

impl Cancel {
    /// A fresh flag nobody else can trigger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Share an existing flag, e.g. one registered with a signal handler.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self { flag, deadline: None }
    }

    pub fn trigger(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn with_deadline(&self, deadline: Instant) -> Self {
        Self { flag: Arc::clone(&self.flag), deadline: Some(deadline) }
    }

    pub fn reason(&self, now: Instant) -> Option<CancelReason> {
        if self.flag.load(Ordering::SeqCst) {
            Some(CancelReason::Interrupt)
        } else if self.deadline.is_some_and(|d| now >= d) {
            Some(CancelReason::Deadline)
        } else {
            None
        }
    }

    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.deadline.map(|d| d.saturating_duration_since(now))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeepaliveSample {
    pub op: Op,
    pub since_request: Duration,
}

/// Counters since the last `take_trace`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    pub requests: u32,
    pub responses: u32,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    /// At most `MAX_SAMPLES`.
    pub keepalives: Vec<KeepaliveSample>,
    pub keepalives_total: u32,
    /// First keepalive payloads verbatim, at most `MAX_RAW`.
    pub keepalive_raw: Vec<Vec<u8>>,
    pub notifies: u32,
    pub stale: u32,
    pub runts: u32,
    /// Text results that were not valid UTF-8 and were decoded lossily.
    pub lossy_text: u32,
}

const MAX_SAMPLES: usize = 256;
const MAX_RAW: usize = 4;

/// Bytes taken from the transport per read; the framer reassembles longer frames.
const READ_SLICE: usize = 0x4000;

/// Longest a blocking read lasts before `Cancel` is checked again.
pub(crate) const CANCEL_SLICE: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) struct Link {
    transport: Box<dyn Transport>,
    framer: Framer,
    ids: wait::IdGen,
    clock: Arc<dyn Clock>,
    cancel: Cancel,
    trace: Trace,
    poisoned: Option<PoisonCause>,
}

impl Link {
    pub fn new(transport: Box<dyn Transport>, clock: Arc<dyn Clock>, cancel: Cancel) -> Self {
        Self {
            transport,
            framer: Framer::default(),
            ids: wait::IdGen::default(),
            clock,
            cancel,
            trace: Trace::default(),
            poisoned: None,
        }
    }

    pub fn poisoned(&self) -> Option<PoisonCause> {
        self.poisoned
    }

    /// Poison after a malformed response that `exchange` itself accepted.
    pub fn poison(&mut self, cause: PoisonCause) {
        self.poisoned.get_or_insert(cause);
    }

    pub fn set_cancel(&mut self, cancel: Cancel) {
        self.cancel = cancel;
    }

    pub fn take_trace(&mut self) -> Trace {
        std::mem::take(&mut self.trace)
    }

    pub fn note_lossy_text(&mut self) {
        self.trace.lossy_text += 1;
    }

    pub fn banner(&self) -> Option<String> {
        self.framer.banner()
    }

    /// Send one request and return the matching response `[err][id][result...]`.
    ///
    /// Contract: at most one request in flight. Every error except `Local` poisons the link, and
    /// so does `Cancelled` once the request was sent. A call made after cancellation sends nothing.
    pub fn exchange(&mut self, op: Op, draft: &Draft, budget: CallBudget) -> Result<Vec<u8>, Error> {
        if let Some(cause) = self.poisoned {
            return Err(Error::Poisoned(cause));
        }
        if let Some(reason) = self.cancel.reason(self.clock.now()) {
            return Err(Error::Cancelled { op, reason });
        }
        let id = self.ids.next();
        let message = draft.seal(id)?;
        let result = self.round_trip(op, id, &message, budget);
        if let Err(e) = &result
            && let Some(cause) = e.poison_cause()
        {
            self.poison(cause);
        }
        result
    }

    fn round_trip(&mut self, op: Op, id: u8, message: &[u8], budget: CallBudget) -> Result<Vec<u8>, Error> {
        let bytes = frame::encode_request(message);
        self.transport.write_all(&bytes).map_err(|e| lost(op, &e))?;
        self.trace.requests += 1;
        self.trace.bytes_tx += bytes.len() as u64;

        let started = self.clock.now();
        let mut waiter = Waiter::new(budget, started);
        let mut buf = [0u8; READ_SLICE];
        loop {
            while let Some(f) = self.framer.next_frame().map_err(|violation| Error::Protocol { op, violation })? {
                if f.kind == FrameKind::Notify {
                    self.trace.notifies += 1;
                    continue;
                }
                match wait::classify(id, &f.payload) {
                    Incoming::Match => {
                        self.trace.responses += 1;
                        return Ok(f.payload);
                    }
                    Incoming::Keepalive => {
                        let now = self.clock.now();
                        waiter.on_keepalive(now);
                        self.record_keepalive(op, now - started, f.payload);
                    }
                    Incoming::Runt => self.trace.runts += 1,
                    Incoming::Stale => self.trace.stale += 1,
                    Incoming::Ahead { got } => {
                        return Err(Error::Protocol { op, violation: Violation::OutOfSync { expected: id, got } });
                    }
                }
            }

            let now = self.clock.now();
            if let Some(reason) = self.cancel.reason(now) {
                return Err(Error::Cancelled { op, reason });
            }
            let mut slice = waiter.next_read_timeout(now).map_err(|x| Error::Timeout {
                op,
                kind: x.kind,
                waited: x.waited,
                keepalives: waiter.keepalives,
            })?;
            slice = slice.min(CANCEL_SLICE);
            if let Some(left) = self.cancel.remaining(now) {
                slice = slice.min(left);
            }
            // A zero read timeout is an error for `TcpStream`.
            self.transport.set_read_timeout(slice.max(Duration::from_millis(1))).map_err(|e| lost(op, &e))?;
            match self.transport.read(&mut buf) {
                Ok(0) => return Err(Error::Lost { op, cause: LostCause::Eof }),
                Ok(n) => {
                    self.trace.bytes_rx += n as u64;
                    waiter.on_activity(self.clock.now());
                    self.framer.push(&buf[..n]);
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(lost(op, &e)),
            }
        }
    }

    fn record_keepalive(&mut self, op: Op, since_request: Duration, raw: Vec<u8>) {
        self.trace.keepalives_total += 1;
        if self.trace.keepalives.len() < MAX_SAMPLES {
            self.trace.keepalives.push(KeepaliveSample { op, since_request });
        }
        if self.trace.keepalive_raw.len() < MAX_RAW {
            self.trace.keepalive_raw.push(raw);
        }
    }
}

fn lost(op: Op, e: &io::Error) -> Error {
    Error::Lost { op, cause: LostCause::from_io(e) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_errors_are_classified_by_kind() {
        let ep = Endpoint::new("h", 1);
        let t = Duration::from_secs(3);
        let kind = |k| classify_connect(&ep, t, io::Error::from(k)).tag();
        assert_eq!(kind(io::ErrorKind::ConnectionRefused), "refused");
        assert_eq!(kind(io::ErrorKind::TimedOut), "connect-timeout");
        assert_eq!(kind(io::ErrorKind::HostUnreachable), "unreachable");
        assert_eq!(kind(io::ErrorKind::NetworkUnreachable), "unreachable");
        assert_eq!(kind(io::ErrorKind::PermissionDenied), "io");
    }

    #[test]
    fn cancel_reports_interrupt_before_deadline() {
        let now = Instant::now();
        let c = Cancel::new().with_deadline(now + Duration::from_secs(1));
        assert_eq!(c.reason(now), None);
        assert_eq!(c.reason(now + Duration::from_secs(1)), Some(CancelReason::Deadline));
        c.trigger();
        assert_eq!(c.reason(now), Some(CancelReason::Interrupt));
        assert_eq!(Cancel::new().reason(now), None, "a fresh Cancel does not share the flag");
    }
}
