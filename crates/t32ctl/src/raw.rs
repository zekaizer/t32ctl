// SPDX-License-Identifier: Apache-2.0
//! Bodies of the `raw` leaves: PRACTICE text the caller typed, sent literally, and repository
//! scripts run as they are.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use t32stage::{Bundle, BundleLimits, RelPath, practice::render_args};

use crate::{
    Setup,
    cli::{RawCommand, RawEval, RawScript},
    ctx::Ctx,
    error::{CtlError, ErrorKind},
    grade::Raw,
    out::OutDir,
    profile::walk,
    report::{Body, Bundled, EvalBody, Subject},
    script::Prepared,
};

pub fn cmd(args: &RawCommand, ctx: &mut Ctx<'_, Raw>, _setup: &Setup<'_>) -> Result<Body, CtlError> {
    ctx.cmd(&args.text).map(Body::Cmd)
}

pub fn eval(args: &RawEval, ctx: &mut Ctx<'_, Raw>, _setup: &Setup<'_>) -> Result<Body, CtlError> {
    ctx.eval(&args.text).map(|v| Body::Eval(EvalBody::new(&v)))
}

/// `p` without `.` and `..` components, without touching the file system.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn notation(message: String) -> CtlError {
    CtlError::new(ErrorKind::Notation, message)
}

/// The bundle, entry, arguments and output directory of `raw script`, before connecting. The root
/// is walked by the profile's rules: hidden files and editor leftovers stay out, and a symlink, a
/// special file or a file past the bundle limits is `bad-bundle`.
pub fn script_prepare(args: &RawScript, setup: &Setup<'_>) -> Result<Prepared, CtlError> {
    let env = setup.env;
    let entry = lexical(&env.cwd.join(&args.entry));
    let shown = env.display(&entry);
    let meta = fs::metadata(&entry).map_err(|e| notation(format!("ENTRY {shown}: {e}")))?;
    if !meta.is_file() {
        return Err(notation(format!("ENTRY {shown} is not a regular file")));
    }
    let root = match &args.root {
        Some(r) => lexical(&env.cwd.join(r)),
        None => entry.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let root_shown = env.display(&root);
    let rel = entry
        .strip_prefix(&root)
        .map_err(|_| notation(format!("ENTRY {shown} is not below --root {root_shown}")))?
        .to_string_lossy()
        .replace('\\', "/");
    let rel = RelPath::new(&rel).map_err(|e| notation(format!("ENTRY {shown}: {e}")))?;
    let bad = |message: String| CtlError::new(ErrorKind::BadBundle, message);
    let files = walk(&root).map_err(|e| bad(e.shown(env, "a script directory")))?;
    if !files.iter().any(|f| f.rel == rel) {
        return Err(notation(format!(
            "ENTRY {shown} is a hidden file or an editor leftover, which the bundle leaves out"
        )));
    }
    let mut bundle = Bundle::new(BundleLimits::default());
    let count = files.len();
    for f in files {
        bundle.insert(f.rel, f.bytes).map_err(|e| bad(format!("{root_shown}: {e}")))?;
    }
    render_args(&args.args).map_err(|e| notation(e.to_string()))?;
    let (out, out_shown) = match &args.out {
        Some(dir) => {
            let path = lexical(&env.cwd.join(dir));
            (OutDir::at(&path, &dir.to_string_lossy())?, format!("{}/", env.display(&path)))
        }
        None => {
            let out = OutDir::create(&setup.paths.out_base(), "s")?;
            let shown = format!("{}/", env.display(out.path()));
            (out, shown)
        }
    };
    Ok(Prepared {
        subject: Subject::Script {
            entry: shown,
            bundle: Bundled { hash: None, root: Some(root_shown), files: count, cached: false },
        },
        bundle,
        entry: rel,
        args: args.args.clone(),
        timeout: args.timeout.as_duration(),
        params: Vec::new(),
        out,
        out_shown,
        declared: Vec::new(),
        transfer_bound: setup.config.transfer_timeout.as_duration(),
    })
}

pub fn script(_args: &RawScript, prep: Prepared, ctx: &mut Ctx<'_, Raw>, _setup: &Setup<'_>) -> Result<Body, CtlError> {
    ctx.run_script(prep)
}
