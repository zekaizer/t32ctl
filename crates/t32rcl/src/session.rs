// SPDX-License-Identifier: Apache-2.0
//! The public face of the crate: one `Session` per PowerView connection.
//!
//! Hidden from callers: framing, request ids, header forms, padding, keepalive and stale
//! responses, memory chunking, the address object, EXP blocks, the device prompt, poisoning.

use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Duration,
};

use crate::{
    Result,
    error::{ConnectError, Error, ErrorClass, LocalError, NotRclEvidence, Op, PoisonCause, RemoteError, RemoteKind},
    link::{Cancel, Clock, Connector, Link, SystemClock, TcpConnector, Trace},
    value::{CallBudget, DevicePrompt, Endpoint, FuncValue, MacroName, MemAddr, PracticeState, TargetState, Tuning},
    version::{BASE_FUNCTIONS, BUILD_FUNCTIONS, PowerViewBuild},
    wire::{self, req, resp},
};

/// Everything `Session::connect` needs besides the endpoint.
#[derive(Clone, Debug)]
pub struct ConnectOpts {
    pub connect_timeout: Duration,
    /// Silence tolerated while waiting for the attach reply.
    pub attach_idle: Duration,
    pub budget: CallBudget,
    pub tuning: Tuning,
    pub prompt: Option<DevicePrompt>,
    pub connector: Arc<dyn Connector>,
    pub clock: Arc<dyn Clock>,
    pub cancel: Cancel,
}

impl Default for ConnectOpts {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            attach_idle: Duration::from_secs(5),
            budget: CallBudget::DEFAULT,
            tuning: Tuning::default(),
            prompt: None,
            connector: Arc::new(TcpConnector),
            clock: Arc::new(SystemClock),
            cancel: Cancel::new(),
        }
    }
}

/// Timings of the last successful connect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectReport {
    pub tcp: Duration,
    /// Attach request sent to attach reply received.
    pub attach: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Live,
    Poisoned(PoisonCause),
}

#[derive(Debug)]
pub struct Session {
    endpoint: Endpoint,
    opts: ConnectOpts,
    link: Option<Link>,
    /// Set while there is no usable link of our own making: never connected, or revive failed.
    stuck: Option<PoisonCause>,
    revive_attempted: bool,
    report: ConnectReport,
    build: Option<PowerViewBuild>,
}

impl Session {
    /// TCP connect and attach. Failures are classified (see `ConnectError`).
    pub fn connect(endpoint: Endpoint, opts: ConnectOpts) -> std::result::Result<Self, ConnectError> {
        let mut s = Self::unconnected(endpoint, opts);
        s.open_link()?;
        Ok(s)
    }

    /// A session that has not connected yet. `t32probe` starts with one so that its `connect`
    /// probe measures the first connect like any other step.
    pub fn unconnected(endpoint: Endpoint, opts: ConnectOpts) -> Self {
        Self {
            endpoint,
            opts,
            link: None,
            stuck: Some(PoisonCause::NeverConnected),
            revive_attempted: false,
            report: ConnectReport::default(),
            build: None,
        }
    }

    /// The only place that builds a `Link`.
    fn open_link(&mut self) -> std::result::Result<(), ConnectError> {
        self.link = None;
        let clock = Arc::clone(&self.opts.clock);
        let endpoint = self.endpoint.clone();
        let t0 = clock.now();
        let transport = self.opts.connector.open(&endpoint, self.opts.connect_timeout)?;
        let tcp = clock.now() - t0;
        let mut link = Link::new(transport, Arc::clone(&clock), self.opts.cancel.clone());
        let t1 = clock.now();
        let budget = CallBudget::idle(self.opts.attach_idle).with_total(self.opts.attach_idle);
        let reply = match link.exchange(Op::Attach, &req::attach(), budget) {
            Ok(reply) => reply,
            Err(e) => return Err(attach_failure(endpoint, e, link.banner())),
        };
        resp::status(Op::Attach, &reply).map_err(|remote| ConnectError::Rejected { endpoint, remote })?;
        self.report = ConnectReport { tcp, attach: clock.now() - t1 };
        self.link = Some(link);
        self.stuck = None;
        self.revive_attempted = false;
        self.build = None;
        Ok(())
    }

    /// Reconnect at most once per poisoning. `Ok(true)` if a new connection was made, `Ok(false)`
    /// if the session was live. After a failed attempt every call returns `Poisoned` without
    /// touching the network, so no caller can loop on a dead PowerView.
    pub fn try_revive(&mut self) -> Result<bool> {
        if self.health() == Health::Live {
            return Ok(false);
        }
        if self.revive_attempted {
            return Err(Error::Poisoned(PoisonCause::ReconnectFailed));
        }
        self.revive_attempted = true;
        match self.open_link() {
            Ok(()) => Ok(true),
            Err(e) => {
                self.stuck = Some(PoisonCause::ReconnectFailed);
                Err(e.into())
            }
        }
    }

    pub fn health(&self) -> Health {
        match (self.stuck, &self.link) {
            (Some(c), _) => Health::Poisoned(c),
            (None, Some(l)) => l.poisoned().map_or(Health::Live, Health::Poisoned),
            (None, None) => Health::Poisoned(PoisonCause::NeverConnected),
        }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn connect_report(&self) -> ConnectReport {
        self.report
    }

    /// Applies to the current link and to every reconnect.
    pub fn set_cancel(&mut self, cancel: Cancel) {
        if let Some(l) = self.link.as_mut() {
            l.set_cancel(cancel.clone());
        }
        self.opts.cancel = cancel;
    }

    pub fn set_tuning(&mut self, tuning: Tuning) {
        self.opts.tuning = tuning;
    }

    pub fn tuning(&self) -> Tuning {
        self.opts.tuning
    }

    /// Counters since the previous call, attach exchange included.
    pub fn take_trace(&mut self) -> Trace {
        self.link.as_mut().map(Link::take_trace).unwrap_or_default()
    }

    /// Replace the call budget until the guard drops.
    pub fn with_budget(&mut self, budget: CallBudget) -> BudgetScope<'_> {
        let saved = std::mem::replace(&mut self.opts.budget, budget);
        BudgetScope { session: self, saved }
    }

    /// `T32_ExecuteCommand`, with the device prompt prepended. Returns when PowerView has run the
    /// command; for a blocking command (`WAIT`) that is after its keepalive frames.
    pub fn cmd(&mut self, text: &str) -> Result<()> {
        let text = match &self.opts.prompt {
            Some(p) => format!("{}{text}", p.as_str()),
            None => text.to_string(),
        };
        let reply = self.exchange(Op::Command, &req::execute_command(&text)?)?;
        resp::status(Op::Command, &reply)?;
        Ok(())
    }

    /// `T32_ExecuteFunction`. A failed function (status 90 or result type 0) is
    /// `Error::Remote` with `RemoteKind::FunctionFailed`.
    pub fn eval(&mut self, expr: &str) -> Result<FuncValue> {
        let reply = self.exchange(Op::Function, &req::execute_function(expr)?)?;
        let result = resp::status(Op::Function, &reply)?;
        match resp::function(result) {
            Ok((v, lossy)) => {
                self.note_lossy(lossy);
                Ok(v)
            }
            Err(wire::FunctionReply::Remote(r)) => Err(r.into()),
            Err(wire::FunctionReply::Violation(v)) => Err(self.violation(Op::Function, v)),
        }
    }

    /// `EVAL <expr>` followed by `T32_EvalGetString`: the path pyrcl takes for `ERROR.*`
    /// functions (`_functions.py` `error_message`).
    pub fn eval_string(&mut self, expr: &str) -> Result<String> {
        self.cmd(&format!("EVAL {expr}"))?;
        let reply = self.exchange(Op::EvalString, &req::eval_get_string())?;
        let result = resp::status(Op::EvalString, &reply)?;
        let (text, lossy) = wire::text(result);
        self.note_lossy(lossy);
        Ok(text)
    }

    pub fn practice_state(&mut self) -> Result<PracticeState> {
        let reply = self.exchange(Op::PracticeState, &req::practice_state())?;
        let result = resp::status(Op::PracticeState, &reply)?;
        resp::practice_state(result).map_err(|v| self.violation(Op::PracticeState, v))
    }

    /// `T32_Stop`: halt the running PRACTICE script, also out of a dialog wait. The script stays
    /// on the PRACTICE stack; unwinding it is the caller's decision.
    pub fn stop_practice(&mut self) -> Result<()> {
        let reply = self.exchange(Op::Stop, &req::stop())?;
        resp::status(Op::Stop, &reply)?;
        Ok(())
    }

    pub fn target_state(&mut self) -> Result<TargetState> {
        let reply = self.exchange(Op::TargetState, &req::target_state())?;
        let result = resp::status(Op::TargetState, &reply)?;
        resp::target_state(result).map_err(|v| self.violation(Op::TargetState, v))
    }

    /// Read `len` bytes in `tuning().read_chunk()` requests. Any access class.
    pub fn mem_read(&mut self, at: &MemAddr, len: usize) -> Result<Vec<u8>> {
        at.offset(len as u64).ok_or(LocalError::AddressOverflow)?;
        let chunk = self.opts.tuning.read_chunk();
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = chunk.min(len - out.len());
            let addr = at.offset(out.len() as u64).ok_or(LocalError::AddressOverflow)?;
            let reply = self.exchange(Op::MemRead, &req::mem_read(&addr, n as u16))?;
            let result = resp::status(Op::MemRead, &reply)?;
            match resp::mem_read(result, n) {
                Ok(bytes) => out.extend_from_slice(bytes),
                Err(v) => return Err(self.violation(Op::MemRead, v)),
            }
        }
        Ok(out)
    }

    /// Write `data` in `tuning().write_chunk()` requests. Not atomic: a failure after the first
    /// chunk leaves a partial write.
    pub fn mem_write(&mut self, at: &MemAddr, data: &[u8]) -> Result<()> {
        at.offset(data.len() as u64).ok_or(LocalError::AddressOverflow)?;
        for (i, chunk) in data.chunks(self.opts.tuning.write_chunk()).enumerate() {
            let addr = at.offset((i * self.opts.tuning.write_chunk()) as u64).ok_or(LocalError::AddressOverflow)?;
            let reply = self.exchange(Op::MemWrite, &req::mem_write(&addr, chunk))?;
            resp::status(Op::MemWrite, &reply)?;
        }
        Ok(())
    }

    pub fn macro_get(&mut self, name: &MacroName) -> Result<String> {
        let reply = self.exp(Op::MacroGet, req::macro_get(name, self.opts.tuning.macro_spelling()))?;
        let Some(payload) = reply.payload.as_deref() else { return Err(exp_failure(Op::MacroGet, &reply)) };
        match resp::macro_value(payload) {
            Ok((value, lossy)) => {
                self.note_lossy(lossy);
                Ok(value)
            }
            Err(v) => Err(self.violation(Op::MacroGet, v)),
        }
    }

    pub fn macro_set(&mut self, name: &MacroName, value: &str) -> Result<()> {
        let reply = self.exp(Op::MacroSet, req::macro_set(name, self.opts.tuning.macro_spelling(), value)?)?;
        if reply.payload.is_some() { Ok(()) } else { Err(exp_failure(Op::MacroSet, &reply)) }
    }

    /// Build numbers, cached per connection. Each spelling in `BUILD_FUNCTIONS` is tried in order;
    /// the base build is optional.
    pub fn build(&mut self) -> Result<PowerViewBuild> {
        if let Some(b) = self.build {
            return Ok(b);
        }
        let build = self.first_number(&BUILD_FUNCTIONS)?.ok_or_else(|| Error::UnexpectedResult {
            op: Op::Function,
            wanted: "a build number",
            got: "no build function answered".to_string(),
        })?;
        let base = self.first_number(&BASE_FUNCTIONS)?;
        let b = PowerViewBuild { build, base };
        self.build = Some(b);
        Ok(b)
    }

    /// First function in `exprs` that yields a u32. A refusal moves on; any other error returns.
    fn first_number(&mut self, exprs: &[&str]) -> Result<Option<u32>> {
        for expr in exprs {
            match self.eval(expr).and_then(|v| v.into_int(expr)) {
                Ok(v) => {
                    if let Ok(v) = u32::try_from(v) {
                        return Ok(Some(v));
                    }
                }
                Err(e) if e.class() == ErrorClass::Rejected => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    fn exchange(&mut self, op: Op, draft: &wire::Draft) -> Result<Vec<u8>> {
        let budget = self.opts.budget;
        match (self.stuck, self.link.as_mut()) {
            (None, Some(link)) => link.exchange(op, draft, budget),
            (stuck, _) => Err(Error::Poisoned(stuck.unwrap_or(PoisonCause::NeverConnected))),
        }
    }

    fn exp(&mut self, op: Op, draft: wire::Draft) -> Result<wire::ExpReply> {
        let reply = self.exchange(op, &draft)?;
        let result = resp::status(op, &reply)?;
        resp::exp(result).map_err(|v| self.violation(op, v))
    }

    /// A malformed response poisons the link like a framing error does.
    fn violation(&mut self, op: Op, violation: crate::Violation) -> Error {
        if let Some(l) = self.link.as_mut() {
            l.poison(PoisonCause::Protocol);
        }
        Error::Protocol { op, violation }
    }

    fn note_lossy(&mut self, lossy: bool) {
        if let (true, Some(l)) = (lossy, self.link.as_mut()) {
            l.note_lossy_text();
        }
    }
}

fn attach_failure(endpoint: Endpoint, e: Error, banner: Option<String>) -> ConnectError {
    match e {
        Error::Timeout { waited, .. } => ConnectError::NoAnswer { endpoint, waited },
        Error::Lost { cause, .. } => ConnectError::ClosedDuringAttach { endpoint, cause },
        Error::Cancelled { reason, .. } => ConnectError::Cancelled { endpoint, reason },
        Error::Protocol { violation, .. } => {
            let evidence = banner.map_or(NotRclEvidence::Violation(violation), NotRclEvidence::Banner);
            ConnectError::NotRcl { endpoint, evidence }
        }
        Error::Remote(remote) => ConnectError::Rejected { endpoint, remote },
        other => ConnectError::Io { endpoint, source: std::io::Error::other(other.to_string()) },
    }
}

fn exp_failure(op: Op, reply: &wire::ExpReply) -> Error {
    let message = match (&reply.error_message, reply.error_code) {
        (Some(m), Some(c)) => Some(format!("{m} (EXP code {c})")),
        (Some(m), None) => Some(m.clone()),
        (None, Some(c)) => Some(format!("EXP code {c}")),
        (None, None) => None,
    };
    RemoteError { op, kind: RemoteKind::ExpFailed, code: 0, message }.into()
}

/// Restores the previous `CallBudget` when dropped.
#[derive(Debug)]
pub struct BudgetScope<'a> {
    session: &'a mut Session,
    saved: CallBudget,
}

impl Deref for BudgetScope<'_> {
    type Target = Session;
    fn deref(&self) -> &Session {
        self.session
    }
}

impl DerefMut for BudgetScope<'_> {
    fn deref_mut(&mut self) -> &mut Session {
        self.session
    }
}

impl Drop for BudgetScope<'_> {
    fn drop(&mut self) {
        self.session.opts.budget = self.saved;
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
