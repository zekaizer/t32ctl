// SPDX-License-Identifier: Apache-2.0
//! Board procedures: `read procs` and the `<grade> proc` leaves.

use crate::{
    Setup,
    cli::ReadProcs,
    error::{CtlError, ErrorKind},
    profile::{ParamType, ProcDecl, Profile, policy::effective_grade},
    report::{Body, ParamView, ProcView, ProcsBody, ProfileRef},
};

/// `unknown-proc`, with the names the profile declares.
pub fn unknown(profile: &Profile, name: &str) -> CtlError {
    let declared: Vec<&str> = profile.procs.iter().map(|p| p.name.as_str()).collect();
    let declared = if declared.is_empty() { "none".to_string() } else { declared.join(", ") };
    CtlError::new(
        ErrorKind::UnknownProc,
        format!("the board profile declares no procedure {name:?}; t32ctl read procs lists them"),
    )
    .with_details(vec![format!("declared: {declared}")])
}

/// A declaration as `read procs` shows it, at its effective grade.
fn view(profile: &Profile, d: &ProcDecl) -> ProcView {
    let grade = effective_grade(profile, &d.name).expect("a declared procedure has a grade");
    let params = d
        .params
        .iter()
        .map(|p| {
            let (values, min, max) = match &p.ty {
                ParamType::Enum(values) => (values.clone(), None, None),
                ParamType::Integer { min, max } => (Vec::new(), *min, *max),
                _ => (Vec::new(), None, None),
            };
            ParamView {
                name: p.name.clone(),
                flag: p.flag(),
                ty: p.ty.name(),
                about: p.about.clone(),
                default: p.default.clone(),
                values,
                min,
                max,
                required: p.required(),
            }
        })
        .collect();
    ProcView {
        name: d.name.clone(),
        grade: grade.name(),
        timeout_ms: d.timeout.as_ms(),
        about: d.about.clone(),
        effects: d.effects.iter().map(|e| e.name()).collect(),
        outputs: d.outputs.iter().map(|o| o.to_string()).collect(),
        params,
        usage: d.usage(grade),
    }
}

/// The profile directory as printed, with its hash and file count.
pub fn profile_ref(setup: &Setup<'_>) -> ProfileRef {
    let p = &setup.profile;
    ProfileRef { dir: format!("{}/", setup.env.display(&p.dir)), hash: p.hash.to_string(), files: p.files.len() }
}

/// The profile's procedures as a table, or one declaration in full.
pub fn list(args: &ReadProcs, setup: &Setup<'_>) -> Result<Body, CtlError> {
    let p = &setup.profile;
    let procs = match &args.name {
        Some(name) => vec![view(p, p.proc(name).ok_or_else(|| unknown(p, name))?)],
        None => p.procs.iter().map(|d| view(p, d)).collect(),
    };
    Ok(Body::Procs(ProcsBody { profile: profile_ref(setup), procs, one: args.name.is_some() }))
}
