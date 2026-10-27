// SPDX-License-Identifier: Apache-2.0

use crate::ctx::{ProbeCtx, ProbeResult};

pub fn connect(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn version(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn latency(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn clients(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn keepalive(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn reconnect(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}
