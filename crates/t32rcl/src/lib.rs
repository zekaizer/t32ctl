// SPDX-License-Identifier: Apache-2.0
//! `t32rcl`: a TRACE32 Remote API (RCL) client over NETTCP.
//!
//! ```no_run
//! use t32rcl::{ConnectOpts, Endpoint, Session};
//!
//! let mut s = Session::connect(Endpoint::new("127.0.0.1", 20000), ConnectOpts::default())?;
//! let build = s.build()?;
//! let running = s.eval("STATE.RUN()")?.into_bool("STATE.RUN()")?;
//! println!("build {build}, running {running}");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Scope: framing, attach, keepalive, ExecuteCommand and ExecuteFunction with typed results,
//! EvalGetString, PRACTICE state, `T32_Stop`, GetState, memory read and write for any access
//! class, global PRACTICE macros (EXP 2). Not here: files, PRACTICE script text, retry policy.
//! Wire layouts and framing are crate-private.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod frame;
mod link;
mod session;
mod wait;
mod wire;

pub mod error;
pub mod value;
pub mod version;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use error::{
    CancelReason, ConnectError, Error, ErrorClass, LocalError, LostCause, NotRclEvidence, Op, PoisonCause, RemoteError,
    RemoteKind, TimeoutKind, Violation,
};
pub use link::{Cancel, Clock, Connector, KeepaliveSample, SystemClock, TcpConnector, Trace, Transport};
pub use session::{BudgetScope, ConnectOpts, ConnectReport, Health, Session};
pub use value::{
    AccessClass, CallBudget, DevicePrompt, Endpoint, FuncValue, MacroName, MacroSpelling, MemAddr, MessageLine,
    PracticeState, PrintFormat, TargetState, Tuning,
};
pub use version::{Feature, MinSupport, PowerViewBuild, Support};

pub type Result<T, E = Error> = std::result::Result<T, E>;
