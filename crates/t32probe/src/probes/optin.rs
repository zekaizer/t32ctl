// SPDX-License-Identifier: Apache-2.0

use crate::ctx::{ProbeCtx, ProbeResult};

pub fn dialog(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn multi_client(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn run_control(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn elf_unzip(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}
