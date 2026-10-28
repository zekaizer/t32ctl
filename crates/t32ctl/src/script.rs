// SPDX-License-Identifier: Apache-2.0
//! One bundle run through `t32stage`, shared by `<grade> proc` and `raw script`: file uploads and
//! parameter macros, the snapshot, the run under the runner, and the outputs written into the
//! call's output directory. Everything that needs no PowerView is settled before connecting
//! (`Prepared`).

use std::{fs::File, io::Read, path::PathBuf, time::Duration};

use t32rcl::{Cancel, CancelReason, Error, Session};
use t32stage::{Bundle, RelPath, RunOutcome, RunRequest, Stage, StageError, TarEntry, param_macro};

use crate::{
    error::{CtlError, ErrorKind, from_stage},
    notation::OutName,
    out::OutDir,
    profile::ParamValue,
    report::{Body, Outcome, OutputFile, OutputsView, ParamShown, RunBody, RunResult, Subject, Upload},
};

/// A run settled before connecting.
#[derive(Debug)]
pub struct Prepared {
    /// What the report names; `cached` is decided by the snapshot.
    pub subject: Subject,
    pub bundle: Bundle,
    pub entry: RelPath,
    pub args: Vec<String>,
    pub timeout: Duration,
    /// In declaration order.
    pub params: Vec<Param>,
    /// The call's output directory, and how the report prints it.
    pub out: OutDir,
    pub out_shown: String,
    /// Outputs the procedure declares.
    pub declared: Vec<OutName>,
    /// `[calls] transfer_timeout`, the bound of each file upload.
    pub transfer_bound: Duration,
    /// A burn procedure: success needs an explicit rc.
    pub burn: bool,
}

/// One parameter's value.
#[derive(Debug)]
pub struct Param {
    pub name: String,
    pub flag: String,
    pub value: ParamValue,
    /// The value in the caller's notation.
    pub shown: String,
    /// A file parameter's local length, checked before connecting.
    pub file_len: Option<u64>,
}

/// How a file upload ended.
enum Uploaded {
    Done(t32stage::Uploaded),
    /// The transfer bound passed; the link is poisoned and cleanup revives it.
    Stopped,
}

/// Upload `path` as parameter `p` under the transfer bound. The name on the PowerView host is the
/// local name when PowerView can carry it, else the parameter's name.
fn upload(
    session: &mut Session,
    stage: &mut Stage,
    cancel: &Cancel,
    p: &Param,
    path: &PathBuf,
    len: u64,
    bound: Duration,
) -> Result<Uploaded, CtlError> {
    let local = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = RelPath::new(&local).or_else(|_| RelPath::new(&p.name)).expect("a parameter name is a path");
    let mut file =
        File::open(path).map_err(|e| CtlError::new(ErrorKind::ProcParams, format!("{} {}: {e}", p.flag, p.shown)))?;
    let t0 = session.now();
    session.set_cancel(cancel.with_deadline(t0 + bound));
    let result = stage.upload_file(session, &name, len, |buf| file.read_exact(buf));
    session.set_cancel(cancel.clone());
    match result {
        Ok(u) => Ok(Uploaded::Done(u)),
        Err(StageError::Rcl(Error::Cancelled { reason: CancelReason::Deadline, .. })) => Ok(Uploaded::Stopped),
        Err(StageError::Source(e)) => Err(CtlError::new(ErrorKind::ProcParams, format!("{} {}: {e}", p.flag, p.shown))),
        Err(e) => Err(from_stage(&e, &format!("the upload of {}", p.flag))),
    }
}

/// The parameters as the report shows them, the uploaded ones with their copy.
fn shown(params: &[Param], uploads: &[Option<t32stage::Uploaded>]) -> Vec<ParamShown> {
    params
        .iter()
        .enumerate()
        .map(|(i, p)| ParamShown {
            name: p.name.clone(),
            value: p.shown.clone(),
            file: uploads.get(i).and_then(Option::as_ref).map(|u| Upload {
                path: u.path.text(),
                bytes: u.len,
                slices: u.slices,
            }),
        })
        .collect()
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// The run: uploads, macros, snapshot, the runner, outputs. A dialog, a vanished runner, or a
/// failed output fetch after a known result is an error that carries the body.
pub(crate) fn execute(
    session: &mut Session,
    stage: &mut Stage,
    cancel: &Cancel,
    prep: Prepared,
) -> Result<Body, CtlError> {
    let Prepared { mut subject, bundle, entry, args, timeout, params, out, out_shown, declared, transfer_bound, burn } =
        prep;
    let mut macros = Vec::with_capacity(params.len());
    let mut uploads = Vec::with_capacity(params.len());
    for p in &params {
        let name = param_macro(&p.name).map_err(|e| CtlError::new(ErrorKind::ProcParams, e.to_string()))?;
        let value = match (&p.value, p.file_len) {
            (ParamValue::File(path), Some(len)) => {
                match upload(session, stage, cancel, p, path, len, transfer_bound)? {
                    Uploaded::Done(u) => {
                        let text = u.path.text();
                        uploads.push(Some(u));
                        text
                    }
                    Uploaded::Stopped => {
                        set_params(&mut subject, shown(&params, &uploads));
                        let result = RunResult {
                            outcome: Outcome::UploadStopped { flag: p.flag.clone(), bound_ms: ms(transfer_bound) },
                            rc: t32stage::ScriptRc::Unset,
                            message: String::new(),
                            error: None,
                            line: None,
                            after_ms: 0,
                            burn,
                        };
                        let outputs = OutputsView { dir: None, files: Vec::new(), missing: Vec::new() };
                        return Ok(Body::Run(Box::new(RunBody { subject, result: Some(result), outputs })));
                    }
                }
            }
            (v, _) => {
                uploads.push(None);
                v.macro_text().expect("a value that is not a file has PRACTICE text")
            }
        };
        macros.push((name, value));
    }
    set_params(&mut subject, shown(&params, &uploads));
    let snap = stage.ensure_snapshot(session, &bundle).map_err(|e| from_stage(&e, "the snapshot"))?;
    set_cached(&mut subject, snap.cached);
    let req = RunRequest { entry, args, timeout, collect_outputs: true, params: macros };
    let report = stage.run(session, &snap, &req).map_err(|e| from_stage(&e, "the run"))?;
    let after_ms = ms(report.elapsed);
    let result = |outcome, rc, message, error, line| RunResult { outcome, rc, message, error, line, after_ms, burn };
    let (result, failure) = match report.outcome.clone() {
        RunOutcome::Finished { caught: false, rc, message, .. } => {
            (Some(result(Outcome::Finished, rc, message, None, None)), None)
        }
        RunOutcome::Finished { caught: true, rc, message, .. } => {
            (Some(result(Outcome::Failed, rc, message, None, None)), None)
        }
        RunOutcome::Halted { error } => {
            let (message, line) = error.map_or((None, None), |e| (Some(e.message), Some(e.cmdline)));
            (Some(result(Outcome::Halted, t32stage::ScriptRc::Unset, String::new(), message, line)), None)
        }
        RunOutcome::TimedOut { .. } => {
            let outcome = Outcome::TimedOut { timeout_ms: ms(timeout) };
            (Some(result(outcome, t32stage::ScriptRc::Unset, String::new(), None, None)), None)
        }
        RunOutcome::DialogAborted => (
            None,
            Some(CtlError::new(
                ErrorKind::Dialog,
                "the script opened a dialog; t32ctl stopped it and unwound the script, and the window may still be \
                 open in PowerView",
            )),
        ),
        RunOutcome::Vanished => (
            None,
            Some(CtlError::new(
                ErrorKind::ScriptVanished,
                "the run ended without the runner reaching its end: something outside t32ctl cleared the PRACTICE \
                 stack",
            )),
        ),
        other => (None, Some(CtlError::new(ErrorKind::PvError, format!("the run ended as {other:?}")))),
    };
    let fetched = stage.fetch_outputs(session, &report);
    let (outputs, fetch_error) = match fetched {
        Ok(o) => match write_outputs(&out, &out_shown, &o.files, &declared) {
            Ok(v) => (v, None),
            Err(e) => (OutputsView { dir: None, files: Vec::new(), missing: Vec::new() }, Some(e)),
        },
        Err(e) => {
            let v = OutputsView { dir: None, files: Vec::new(), missing: Vec::new() };
            (v, Some(from_stage(&e, "fetching the outputs")))
        }
    };
    let body = Body::Run(Box::new(RunBody { subject, result, outputs }));
    match failure.or(fetch_error) {
        Some(e) => Err(e.with_body(body)),
        None => Ok(body),
    }
}

/// Each fetched file into the output directory; declared outputs not among them are missing.
fn write_outputs(out: &OutDir, shown: &str, files: &[TarEntry], declared: &[OutName]) -> Result<OutputsView, CtlError> {
    let mut written = Vec::with_capacity(files.len());
    for f in files {
        out.write(&f.name, &f.data)?;
        written.push(OutputFile { name: f.name.clone(), bytes: f.data.len() as u64 });
    }
    let missing = declared
        .iter()
        .filter(|d| !written.iter().any(|w| w.name.eq_ignore_ascii_case(d.as_str())))
        .map(ToString::to_string)
        .collect();
    let dir = (!written.is_empty()).then(|| shown.to_string());
    Ok(OutputsView { dir, files: written, missing })
}

fn set_params(subject: &mut Subject, shown: Vec<ParamShown>) {
    if let Subject::Proc { params, .. } = subject {
        *params = shown;
    }
}

fn set_cached(subject: &mut Subject, cached: bool) {
    match subject {
        Subject::Proc { profile: b, .. } | Subject::Script { bundle: b, .. } => b.cached = cached,
    }
}
