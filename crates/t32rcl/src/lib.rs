// SPDX-License-Identifier: Apache-2.0
//! `t32rcl`: a TRACE32 Remote API (RCL) client over NETTCP.
//!
//! Wire layouts and framing are crate-private: a downstream crate cannot name a wire type.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg_attr(not(test), allow(dead_code))]
mod frame;
#[cfg_attr(not(test), allow(dead_code))]
mod wire;

pub mod error;
pub mod value;

pub use error::{LocalError, Op, RemoteError, RemoteKind, Violation};
pub use value::{
    AccessClass, CallBudget, DevicePrompt, Endpoint, FuncValue, MacroName, MacroSpelling, MemAddr, PracticeState,
    TargetState, Tuning,
};
