// SPDX-License-Identifier: Apache-2.0
//! Board procedures: `read procs` and the `<grade> proc` leaves.

use std::{fs, path::PathBuf};

use t32stage::{Bundle, BundleLimits};

use crate::{
    Setup,
    cli::ReadProcs,
    ctx::Ctx,
    error::{CtlError, ErrorKind},
    grade::{Grade, GradeId},
    notation::shell_word,
    out::OutDir,
    profile::{
        ParamCx, ParamType, ParamValue, ProcDecl, Profile,
        policy::{Admitted, admit, effective_grade},
    },
    report::{Body, Bundled, Hint, ParamView, ProcView, ProcsBody, ProfileRef, Subject},
    script::{Param, Prepared},
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
pub fn profile_ref(p: &Profile, setup: &Setup<'_>) -> ProfileRef {
    ProfileRef { dir: format!("{}/", setup.env.display(&p.dir)), hash: p.hash.to_string(), files: p.files.len() }
}

/// The profile's procedures as a table, or one declaration in full.
pub fn list(args: &ReadProcs, setup: &Setup<'_>) -> Result<Body, CtlError> {
    let p = setup.need_profile()?;
    let procs = match &args.name {
        Some(name) => vec![view(p, p.proc(name).ok_or_else(|| unknown(p, name))?)],
        None => p.procs.iter().map(|d| view(p, d)).collect(),
    };
    Ok(Body::Procs(ProcsBody { profile: profile_ref(p, setup), procs, one: args.name.is_some() }))
}

/// The tokens after a procedure's name: `--PARAM VALUE` or `--PARAM=VALUE` pairs, and the global
/// flags clap left in them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcCall {
    pub params: Vec<(String, String)>,
    pub json: bool,
    /// The caller's `--build`: `split` takes it after the name, `procedure` adds the one clap took
    /// before it. Hints repeat it.
    pub build: Option<PathBuf>,
    pub help: bool,
    /// Tokens that are not a pair, each as a message.
    pub stray: Vec<String>,
}

/// Take `--json`, `--build DIR`, `--build=DIR` and `--help` out of `tokens`; the rest are
/// parameter pairs.
pub fn split(tokens: &[String]) -> ProcCall {
    let mut call = ProcCall::default();
    let mut it = tokens.iter();
    while let Some(t) = it.next() {
        match t.as_str() {
            "--json" => call.json = true,
            "--help" => call.help = true,
            "--build" => match it.next() {
                Some(dir) => call.build = Some(dir.into()),
                None => call.stray.push("--build needs DIR".into()),
            },
            flag if flag.starts_with("--build=") => call.build = Some(flag["--build=".len()..].into()),
            flag if flag.starts_with("--") && flag.len() > 2 => match flag.split_once('=') {
                Some((f, v)) => call.params.push((f.to_string(), v.to_string())),
                None => match it.next() {
                    Some(v) => call.params.push((flag.to_string(), v.clone())),
                    None => call.stray.push(format!("{flag} needs a value")),
                },
            },
            other => call.stray.push(format!("{} is not a --PARAM VALUE pair", shell_word(other))),
        }
    }
    call
}

fn params_error<G: Grade>(adm: &Admitted<G>, message: String) -> CtlError {
    CtlError::new(ErrorKind::ProcParams, message).with_details(vec![format!("usage: {}", adm.decl().usage(G::ID))])
}

/// Every declared parameter's value: as typed, else its default. Typed values are checked before
/// defaults, so the caller's own mistake is reported first. A file must be a non-empty regular
/// file.
fn values<G: Grade>(
    adm: &Admitted<G>,
    call: &ProcCall,
    cx: &ParamCx<'_>,
    setup: &Setup<'_>,
) -> Result<Vec<Param>, CtlError> {
    let decl = adm.decl();
    if let Some(stray) = call.stray.first() {
        return Err(params_error(adm, stray.clone()));
    }
    for (i, (flag, _)) in call.params.iter().enumerate() {
        if decl.param_by_flag(flag).is_none() {
            return Err(params_error(adm, format!("proc {} declares no parameter {flag}", decl.name)));
        }
        if call.params[..i].iter().any(|(f, _)| f == flag) {
            return Err(params_error(adm, format!("{flag} is given twice")));
        }
    }
    let typed: Vec<Option<ParamValue>> = decl
        .params
        .iter()
        .map(|p| {
            let text = call.params.iter().find(|(f, _)| *f == p.flag()).map(|(_, v)| v);
            text.map(|t| p.parse(t, cx)).transpose()
        })
        .collect::<Result<_, _>>()
        .map_err(|m| params_error(adm, m))?;
    let mut out = Vec::with_capacity(decl.params.len());
    for (p, typed) in decl.params.iter().zip(typed) {
        let flag = p.flag();
        let value = match typed {
            Some(v) => v,
            None => p
                .default_value(cx)
                .unwrap_or_else(|| Err(format!("{flag} is required")))
                .map_err(|m| params_error(adm, m))?,
        };
        let shown = value.shown(setup.env);
        let file_len = match &value {
            ParamValue::File(path) => {
                let bad = |why: String| params_error(adm, format!("{flag} {}: {why}", shell_word(&shown)));
                let meta = fs::metadata(path).map_err(|e| bad(e.to_string()))?;
                if !meta.is_file() {
                    return Err(bad("not a regular file".into()));
                }
                if meta.len() == 0 {
                    return Err(bad("an empty file".into()));
                }
                Some(meta.len())
            }
            _ => None,
        };
        out.push(Param { name: p.name.clone(), flag, value, shown, file_len });
    }
    Ok(out)
}

/// What a procedure call settled before connecting: the grade check's proof and the run. Built
/// only by [`prepare`], from the `Admitted<G>` it consumes, so `Ctx::run_procedure` runs nothing
/// the grade check did not admit:
///
/// ```compile_fail,E0451
/// use t32ctl::{grade::Burn, procs::ProcPrep, profile::policy::Admitted, script::Prepared};
/// fn forge(adm: Admitted<Burn>, run: Prepared) -> ProcPrep<Burn> {
///     ProcPrep { adm, run }
/// }
/// ```
#[derive(Debug)]
pub struct ProcPrep<G: Grade> {
    adm: Admitted<G>,
    run: Prepared,
}

impl<G: Grade> ProcPrep<G> {
    pub fn decl(&self) -> &ProcDecl {
        self.adm.decl()
    }

    /// The run, for `Ctx::run_procedure`; the admission is consumed with it.
    pub(crate) fn into_run(self) -> Prepared {
        self.run
    }
}

/// The profile directory's files as one bundle; a file PowerView cannot take is `profile-invalid`.
fn profile_bundle(profile: &Profile, setup: &Setup<'_>) -> Result<Bundle, CtlError> {
    let mut bundle = Bundle::new(BundleLimits::default());
    for f in &profile.files {
        bundle.insert(f.rel.clone(), f.bytes.clone()).map_err(|e| {
            CtlError::new(
                ErrorKind::ProfileInvalid,
                format!("{}: {e}", setup.env.display(&profile.dir.join(f.rel.as_str()))),
            )
        })?;
    }
    Ok(bundle)
}

/// The grade check, the parameters, the bundle and the output directory: everything before
/// connecting. A grade mismatch hints the same call under the declared grade.
pub fn prepare<G: Grade>(name: &str, call: &ProcCall, setup: &Setup<'_>) -> Result<ProcPrep<G>, CtlError> {
    let profile = setup.need_profile()?;
    let adm = admit::<G>(profile, name).map_err(|e| {
        if e.kind != ErrorKind::GradeMismatch {
            return e;
        }
        let declared = effective_grade(profile, name).expect("a mismatch has a declared grade");
        let mut words = vec![declared.name().to_string(), "proc".into(), name.to_string()];
        words.extend(call.params.iter().flat_map(|(f, v)| [f.clone(), v.clone()]));
        if let Some(b) = &call.build {
            words.extend(["--build".to_string(), b.to_string_lossy().into_owned()]);
        }
        e.with_next(vec![Hint(words)])
    })?;
    let decl = adm.decl();
    let build = setup.binding.build.as_ref().map(|b| b.value.as_path());
    let cx = ParamCx { target: &profile.target, cwd: &setup.env.cwd, build };
    let params = values(&adm, call, &cx, setup)?;
    let bundle = profile_bundle(profile, setup)?;
    let out = OutDir::create(&setup.paths.out_base(), "p")?;
    let out_shown = format!("{}/", setup.env.display(out.path()));
    let subject = Subject::Proc {
        name: decl.name.clone(),
        grade: G::ID.name(),
        profile: Bundled {
            hash: Some(profile.hash.to_string()),
            root: None,
            files: profile.files.len(),
            cached: false,
        },
        params: Vec::new(),
    };
    let run = Prepared {
        subject,
        bundle,
        entry: decl.script.clone(),
        args: Vec::new(),
        timeout: decl.timeout.as_duration(),
        params,
        out,
        out_shown,
        declared: decl.outputs.clone(),
        transfer_bound: setup.config.transfer_timeout.as_duration(),
    };
    Ok(ProcPrep { adm, run })
}

/// Run the admitted procedure. An interrupted `burn` says what the storage may hold.
pub fn run<G: Grade>(prep: ProcPrep<G>, ctx: &mut Ctx<'_, G>, _setup: &Setup<'_>) -> Result<Body, CtlError> {
    ctx.run_procedure(prep).map_err(|mut e| {
        if e.kind == ErrorKind::Interrupted && G::ID == GradeId::Burn {
            e.details.push("the burn procedure was interrupted: the storage may hold a partial image".into());
        }
        e
    })
}

/// `<grade> proc NAME --help`: the declaration, as `read procs NAME` prints it.
pub fn help(name: &str, setup: &Setup<'_>) -> Result<Body, CtlError> {
    list(&ReadProcs { name: Some(name.to_string()) }, setup)
}

#[cfg(test)]
#[path = "procs_tests.rs"]
mod tests;
