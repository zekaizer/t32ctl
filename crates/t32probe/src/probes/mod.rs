// SPDX-License-Identifier: Apache-2.0
//! Probe bodies, grouped by what they exercise. The catalogue lives in `registry`.

pub mod optin;
pub mod scripts;
pub mod storage;
pub mod transport;

use crate::{ctx::ProbeFail, outcome::SkipReason};

pub(crate) fn pending() -> ProbeFail {
    ProbeFail::Skip(SkipReason::NotApplicable { why: "probe body not built yet".into() })
}
