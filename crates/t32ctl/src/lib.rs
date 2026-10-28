// SPDX-License-Identifier: Apache-2.0
//! The library behind the `t32ctl` binary, so tests run whole calls in-process.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod config;
pub mod error;
pub mod notation;
pub mod report;
