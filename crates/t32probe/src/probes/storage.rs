// SPDX-License-Identifier: Apache-2.0

use crate::ctx::{ProbeCtx, ProbeResult};

pub fn tempdir(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn vm_rw(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn vm_file(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn gzip(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}

pub fn tar_download(_: &mut ProbeCtx<'_>) -> ProbeResult {
    Err(super::pending())
}
