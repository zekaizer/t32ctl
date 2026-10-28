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
    report::{self, BindingView, Body, ConfigView, StatusBody},
};

/// PowerView, and with `--full` the binding, the config and the state directory.
pub fn status(args: &ReadStatus, ctx: &mut Ctx<'_, Read>, setup: &Setup<'_>) -> Result<Body, CtlError> {
    let pv = ctx.status()?;
    let mut body = StatusBody { pv, binding: None, config: None, state_dir: None };
    if args.full {
        let env = setup.env;
        let shown = |s: &Option<Sourced<PathBuf>>, slash: &str| {
            s.as_ref().map(|s| report::Sourced {
                path: format!("{}{slash}", env.display(&s.value)),
                source: s.source.text().to_string(),
            })
        };
        let b = &setup.binding;
        body.binding =
            Some(BindingView { build: shown(&b.build, ""), profile: shown(&b.profile, "/"), board: b.board.clone() });
        body.config = Some(ConfigView {
            file: setup.config.file.as_deref().map(|f| env.display(f)),
            host: setup.config.host.clone(),
            attached: Some(ctx.host().to_string()),
            port: setup.config.port,
        });
        body.state_dir = Some(format!("{}/", env.display(&setup.pv_dir)));
    }
    Ok(Body::Status(body))
}
