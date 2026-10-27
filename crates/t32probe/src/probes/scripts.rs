// SPDX-License-Identifier: Apache-2.0

use crate::ctx::{ProbeCtx, ProbeResult};

pub fn macro_probe(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn bundle(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn script_error(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}
