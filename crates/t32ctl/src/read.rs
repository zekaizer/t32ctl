// SPDX-License-Identifier: Apache-2.0
//! Bodies of the `read` leaves that connect.

use std::path::PathBuf;

use t32stage::RegName;

use crate::{
    Setup,
    cli::{ReadReg, ReadStatus},
    config::Sourced,
    ctx::Ctx,
    error::CtlError,
    grade::Read,
    report::{self, BindingView, Body, ConfigView, ProfileView, StatusBody},
};

/// The target, the profile and PowerView; `--full` adds the profile hash, the binding, the config
/// and the state directory.
pub fn status(args: &ReadStatus, ctx: &mut Ctx<'_, Read>, setup: &Setup<'_>) -> Result<Body, CtlError> {
    let (target, pv) = ctx.status()?;
    let env = setup.env;
    let p = &setup.profile;
    let profile = ProfileView { dir: format!("{}/", env.display(&p.dir)), hash: None, files: None };
    let mut body = StatusBody { target, profile, pv, binding: None, config: None, state_dir: None };
    if args.full {
        let shown = |s: &Option<Sourced<PathBuf>>| {
            s.as_ref().map(|s| report::Sourced { path: env.display(&s.value), source: s.source.text().to_string() })
        };
        let b = &setup.binding;
        body.profile.hash = Some(p.hash.to_string());
        body.profile.files = Some(p.files.len());
        body.binding = Some(BindingView {
            build: shown(&b.build),
            profile_from: b.profile.as_ref().map_or_else(String::new, |s| s.source.text().to_string()),
            variant: p.variant.clone(),
            board: b.board.clone(),
        });
        body.config = Some(ConfigView {
            file: setup.config.file.as_deref().map(|f| env.display(f)),
            host: setup.config.host.clone(),
            attached: Some(ctx.host().to_string()),
            port: setup.config.port,
        });
        body.state_dir = Some(format!("{}/", env.display(&setup.pv_dir)));
    }
    Ok(Body::Status(Box::new(body)))
}

/// The AArch64 general-purpose set `read reg` reads without a name.
const GENERAL: [&str; 34] = [
    "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14", "x15", "x16", "x17",
    "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30", "sp", "pc", "cpsr",
];

/// The named registers, or the general-purpose set.
pub fn reg(args: &ReadReg, ctx: &mut Ctx<'_, Read>, _setup: &Setup<'_>) -> Result<Body, CtlError> {
    let names = if args.names.is_empty() {
        GENERAL.iter().map(|n| RegName::new(n).expect("a general-purpose register name is valid")).collect()
    } else {
        args.names.clone()
    };
    ctx.read_regs(&names).map(Body::Regs)
}
