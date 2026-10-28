// SPDX-License-Identifier: Apache-2.0
//! The library behind the `t32ctl` binary, so tests run whole calls in-process.
//!
//! One call is [`run`]: parse, resolve the config, the binding and the board profile, connect,
//! replay journals, run the leaf under its `Ctx<G>`, read the header, and render one report.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod cli;
pub mod config;
pub mod ctx;
pub mod describe;
pub mod error;
pub mod grade;
pub mod notation;
pub mod profile;
pub mod raw;
pub mod read;
pub mod report;

use std::{ffi::OsString, fs, path::PathBuf, sync::Arc, time::Duration};

use t32rcl::{CallBudget, Cancel, CancelReason, Clock, ConnectOpts, Connector, SystemClock, TcpConnector};

use crate::{
    cli::{Globals, GradeCmd, Leaf, Parsed, RawLeaf, ReadLeaf},
    config::{Binding, Config, Env, Paths},
    ctx::{Conn, CpuCheck, Ctx},
    error::{CtlError, ErrorKind},
    grade::{Grade, Raw, Read},
    profile::Profile,
    report::{Body, Report},
};

/// Everything a call takes from outside: the environment, the network, the clock, and the
/// interrupt flag.
#[derive(Debug)]
pub struct Deps {
    pub env: Env,
    pub connector: Arc<dyn Connector>,
    pub clock: Arc<dyn Clock>,
    /// Set by the first Ctrl-C or SIGTERM.
    pub cancel: Cancel,
}

impl Deps {
    pub fn from_process(cancel: Cancel) -> Self {
        Self { env: Env::from_process(), connector: Arc::new(TcpConnector), clock: Arc::new(SystemClock), cancel }
    }
}

/// What the process prints on stdout and its exit status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub text: String,
    pub code: u8,
}

/// TCP connect bound per candidate host.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// One call, from the command line to the report.
pub fn run(argv: &[OsString], deps: &Deps) -> Output {
    let cli = match cli::parse(argv) {
        Parsed::Cli(cli) => cli,
        Parsed::Print(text) => return Output { text, code: 0 },
        Parsed::Usage(e) => {
            let json = argv.iter().skip(2).any(|a| a == "--json");
            let mut report = Report::new(words(argv));
            report.error = Some(e);
            return render(&report, json);
        }
    };
    match cli.grade {
        GradeCmd::Read(g) => match &g.leaf {
            ReadLeaf::Status(a) => connected::<Read, _>(a, &g.globals, deps, CpuCheck::Report, read::status),
            ReadLeaf::Reg(a) => connected::<Read, _>(a, &g.globals, deps, CpuCheck::Refuse, read::reg),
            ReadLeaf::Describe(a) => document::<Read, _>(a, |a| describe::render(a.format)),
        },
        GradeCmd::Raw(g) => match &g.leaf {
            RawLeaf::Cmd(a) => connected::<Raw, _>(a, &g.globals, deps, CpuCheck::Refuse, raw::cmd),
            RawLeaf::Eval(a) => connected::<Raw, _>(a, &g.globals, deps, CpuCheck::Refuse, raw::eval),
        },
    }
}

/// The words of a command line that does not parse, up to the first flag, for `command`.
fn words(argv: &[OsString]) -> String {
    let words = argv.iter().skip(1).map(|a| a.to_string_lossy()).take_while(|a| !a.starts_with('-')).take(2);
    words.collect::<Vec<_>>().join(" ")
}

fn render(report: &Report, json: bool) -> Output {
    let text = if json { report.json() } else { report.text() };
    Output { text, code: report.exit().code() }
}

/// A leaf whose output is a document, not a report: it ignores `--json` and connects nowhere.
fn document<G: Grade, L: Leaf<G>>(args: &L, body: impl FnOnce(&L) -> String) -> Output {
    Output { text: body(args), code: 0 }
}

/// What every connecting call resolves before it connects.
#[derive(Debug)]
pub struct Setup<'e> {
    pub env: &'e Env,
    pub paths: Paths,
    pub config: Config,
    pub binding: Binding,
    /// The board profile the binding names, with the build's variant applied.
    pub profile: Profile,
    /// t32stage's state directory for this PowerView.
    pub pv_dir: PathBuf,
}

impl<'e> Setup<'e> {
    fn resolve(env: &'e Env, globals: &Globals) -> Result<Self, CtlError> {
        let paths = Paths::new(env)?;
        let config = Config::load(paths.config_file.as_deref())?;
        let binding = Binding::resolve(env, globals.build.as_deref())?;
        let profile = Profile::from_binding(env, &binding)?;
        let pv_dir = paths.pv_dir(&config);
        Ok(Self { env, paths, config, binding, profile, pv_dir })
    }
}

type LeafBody<L, G> = fn(&L, &mut Ctx<'_, G>, &Setup<'_>) -> Result<Body, CtlError>;

/// A leaf that connects: the pipeline around its body, and one report whatever happens.
fn connected<G: Grade, L: Leaf<G>>(
    args: &L,
    globals: &Globals,
    deps: &Deps,
    check: CpuCheck,
    body: LeafBody<L, G>,
) -> Output {
    let mut report = Report::new(L::SPEC.path.join(" "));
    let mut conn = None;
    match call(args, globals, deps, check, body, &mut conn) {
        Ok(b) => report.body = Some(b),
        Err(e) => report.error = Some(e),
    }
    if let Some(c) = conn.as_mut() {
        report.notices = c.take_notices();
        report.header = c.header();
    }
    render(&report, globals.json)
}

fn call<G: Grade, L: Leaf<G>>(
    args: &L,
    globals: &Globals,
    deps: &Deps,
    check: CpuCheck,
    body: LeafBody<L, G>,
    conn: &mut Option<Conn>,
) -> Result<Body, CtlError> {
    let setup = Setup::resolve(&deps.env, globals)?;
    if deps.cancel.reason(deps.clock.now()) == Some(CancelReason::Interrupt) {
        return Err(CtlError::new(ErrorKind::Interrupted, "interrupted before connecting; nothing was sent"));
    }
    fs::create_dir_all(&setup.pv_dir)
        .map_err(|e| CtlError::new(ErrorKind::StateDir, format!("{}: {e}", setup.env.display(&setup.pv_dir))))?;
    let opts = ConnectOpts {
        connect_timeout: CONNECT_TIMEOUT,
        budget: CallBudget::DEFAULT,
        tuning: setup.config.tuning,
        connector: Arc::clone(&deps.connector),
        clock: Arc::clone(&deps.clock),
        cancel: deps.cancel.clone(),
        ..ConnectOpts::default()
    };
    let c = conn.insert(Conn::connect(&setup.config.hosts(&deps.env), setup.config.port, &opts)?);
    let run = c.prelude(&setup.pv_dir, &setup.profile, check)?;
    body(args, &mut Ctx::new(c, run, &setup.profile), &setup)
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
