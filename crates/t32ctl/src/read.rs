// SPDX-License-Identifier: Apache-2.0
//! Bodies of the `read` leaves that connect.

use std::path::PathBuf;

use crate::{
    Setup,
    cli::ReadStatus,
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
