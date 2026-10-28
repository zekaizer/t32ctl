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
pub mod out;
pub mod procs;
pub mod profile;
pub mod raw;
pub mod read;
pub mod report;
pub mod script;

use std::{ffi::OsString, fs, path::PathBuf, sync::Arc, time::Duration};

use t32rcl::{CallBudget, Cancel, CancelReason, Clock, ConnectOpts, Connector, SystemClock, TcpConnector};
use t32stage::{ExistsFn, PollSchedule, Retention, StageConfig, VmGuard};

use crate::{
    cli::{BurnLeaf, Globals, GradeCmd, Leaf, Parsed, ProcArgs, RawLeaf, ReadLeaf, RunLeaf, SysLeaf, WriteLeaf},
    config::{Binding, Config, Env, Paths},
    ctx::{Conn, CpuCheck, Ctx},
    error::{CtlError, ErrorKind},
    grade::{Burn, Grade, Raw, Read, Run, Sys, Write},
    procs::ProcPrep,
    profile::{Effect, Profile, policy::effective_grade},
    report::{Body, Hint, Report},
};

/// Everything a call takes from outside: the environment, the network, the clock, the interrupt
/// flag, and how often a script run is polled.
#[derive(Debug)]
pub struct Deps {
    pub env: Env,
    pub connector: Arc<dyn Connector>,
    pub clock: Arc<dyn Clock>,
    /// Set by the first Ctrl-C or SIGTERM.
    pub cancel: Cancel,
    pub poll: PollSchedule,
    /// The run token a scripted conversation expects (`t32stage::testkit`).
    #[cfg(test)]
    pub stage_token: Option<String>,
}

impl Deps {
    pub fn from_process(cancel: Cancel) -> Self {
        Self {
            env: Env::from_process(),
            connector: Arc::new(TcpConnector),
            clock: Arc::new(SystemClock),
            cancel,
            poll: PollSchedule::DEFAULT,
            #[cfg(test)]
            stage_token: None,
        }
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
            report.fail(e);
            return render(&report, json);
        }
    };
    match cli.grade {
        GradeCmd::Read(g) => match &g.leaf {
            ReadLeaf::Status(a) => connected::<Read, _>(a, &g.globals, deps, CpuCheck::Report, read::status),
            ReadLeaf::Mem(a) => {
                prepared::<Read, _, _>(a, &g.globals, deps, |_| CpuCheck::Refuse, read::mem_prepare, read::mem)
            }
            ReadLeaf::Reg(a) => connected::<Read, _>(a, &g.globals, deps, CpuCheck::Refuse, read::reg),
            ReadLeaf::Procs(a) => offline::<Read, _>(a, &g.globals, deps, procs::list),
            ReadLeaf::Describe(a) => document::<Read, _>(a, |a| describe::render(a.format)),
            ReadLeaf::Proc(a) => procedure::<Read>(a, &g.globals, deps),
        },
        GradeCmd::Run(g) => match &g.leaf {
            RunLeaf::Proc(a) => procedure::<Run>(a, &g.globals, deps),
        },
        GradeCmd::Write(g) => match &g.leaf {
            WriteLeaf::Proc(a) => procedure::<Write>(a, &g.globals, deps),
        },
        GradeCmd::Sys(g) => match &g.leaf {
            SysLeaf::Proc(a) => procedure::<Sys>(a, &g.globals, deps),
        },
        GradeCmd::Burn(g) => match &g.leaf {
            BurnLeaf::Proc(a) => procedure::<Burn>(a, &g.globals, deps),
        },
        GradeCmd::Raw(g) => match &g.leaf {
            RawLeaf::Cmd(a) => connected::<Raw, _>(a, &g.globals, deps, CpuCheck::Refuse, raw::cmd),
            RawLeaf::Eval(a) => connected::<Raw, _>(a, &g.globals, deps, CpuCheck::Refuse, raw::eval),
            RawLeaf::Script(a) => {
                prepared::<Raw, _, _>(a, &g.globals, deps, |_| CpuCheck::Refuse, raw::script_prepare, raw::script)
            }
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

/// `<grade> proc NAME ...`: the tokens after the name may carry the global flags; `--help` prints
/// the declaration. The CPU check is skipped for a procedure that declares `debugger-config`.
fn procedure<G: Grade>(args: &ProcArgs, globals: &Globals, deps: &Deps) -> Output
where
    ProcArgs: Leaf<G>,
{
    let call = procs::split(&args.tokens);
    let globals =
        Globals { json: globals.json || call.json, build: call.build.clone().or_else(|| globals.build.clone()) };
    if call.help {
        return offline::<G, _>(args, &globals, deps, |a, setup| procs::help(&a.name, setup));
    }
    let check = |p: &ProcPrep<G>| {
        if p.adm.decl().declares(Effect::DebuggerConfig) { CpuCheck::Skip } else { CpuCheck::Refuse }
    };
    prepared::<G, _, _>(
        args,
        &globals,
        deps,
        check,
        |a, setup| procs::prepare::<G>(&a.name, &call, setup),
        |_, prep, ctx, setup| procs::run(prep, ctx, setup),
    )
}

/// A leaf that reads the profile and connects nowhere.
fn offline<G: Grade, L: Leaf<G>>(
    args: &L,
    globals: &Globals,
    deps: &Deps,
    body: impl FnOnce(&L, &Setup<'_>) -> Result<Body, CtlError>,
) -> Output {
    let mut report = Report::new(L::SPEC.path.join(" "));
    match Setup::resolve(&deps.env, globals).and_then(|setup| body(args, &setup)) {
        Ok(b) => report.body = Some(b),
        Err(e) => report.fail(e),
    }
    render(&report, globals.json)
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

/// A leaf that connects and checks nothing before it does.
fn connected<G: Grade, L: Leaf<G>>(
    args: &L,
    globals: &Globals,
    deps: &Deps,
    check: CpuCheck,
    body: LeafBody<L, G>,
) -> Output {
    prepared(args, globals, deps, move |_: &()| check, |_, _| Ok(()), |a, (), ctx, setup| body(a, ctx, setup))
}

/// A leaf that connects: the pipeline around its body, and one report whatever happens.
/// `prepare` runs the leaf's checks that need no PowerView before the state directory and the
/// connection; what it returns decides the CPU check and goes to `body`. After the body the
/// stage is cleaned up, then the header is read.
fn prepared<G: Grade, L: Leaf<G>, P>(
    args: &L,
    globals: &Globals,
    deps: &Deps,
    check: impl FnOnce(&P) -> CpuCheck,
    prepare: impl FnOnce(&L, &Setup<'_>) -> Result<P, CtlError>,
    body: impl FnOnce(&L, P, &mut Ctx<'_, G>, &Setup<'_>) -> Result<Body, CtlError>,
) -> Output {
    let mut report = Report::new(L::SPEC.path.join(" "));
    let mut conn = None;
    let result = call(args, globals, deps, check, prepare, body, &mut conn);
    let closed = conn.as_mut().map(Conn::close).unwrap_or_default();
    match result {
        Ok(b) => report.body = Some(b),
        Err(e) => report.fail(e),
    }
    if let Some(e) = report.error.as_mut().filter(|e| e.kind == ErrorKind::Interrupted && closed.clean > 0) {
        let steps = if closed.clean == 1 { "step" } else { "steps" };
        e.details.push(format!("cleanup ran: {} {steps}", closed.clean));
    }
    if let Some(residue) = closed.residue {
        match &mut report.error {
            None => report.fail(residue),
            Some(e) => e.details.push(residue.message),
        }
    }
    if let Some(c) = conn.as_mut() {
        report.notices = c.take_notices();
        report.header = c.header();
    }
    render(&report, globals.json)
}

/// How the stage of this call opens if its leaf needs one.
fn stage_config(setup: &Setup<'_>, deps: &Deps, path: &[&str]) -> StageConfig {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    StageConfig {
        window: setup.config.window,
        guard: VmGuard::Dedicated,
        state_dir: setup.pv_dir.clone(),
        root: setup.config.stage_root.clone(),
        subdir: None,
        retention: Retention::Cache,
        exists: ExistsFn::OsFileReadable,
        poll: deps.poll,
        holder: format!("t32ctl {} (pid {})", path.join(" "), std::process::id()),
        journal_name: format!("t32ctl-{}-{nanos}", std::process::id()),
    }
}

/// After `target-down`, the board's `attach` procedure when it takes no required parameter.
fn attach_hint(e: CtlError, profile: &Profile) -> CtlError {
    if e.kind != ErrorKind::TargetDown || !e.next.is_empty() {
        return e;
    }
    match (profile.proc("attach"), effective_grade(profile, "attach")) {
        (Some(attach), Some(grade)) if attach.params.iter().all(|p| !p.required()) => {
            e.with_next(vec![Hint(vec![grade.name().into(), "proc".into(), "attach".into()])])
        }
        _ => e,
    }
}

fn call<G: Grade, L: Leaf<G>, P>(
    args: &L,
    globals: &Globals,
    deps: &Deps,
    check: impl FnOnce(&P) -> CpuCheck,
    prepare: impl FnOnce(&L, &Setup<'_>) -> Result<P, CtlError>,
    body: impl FnOnce(&L, P, &mut Ctx<'_, G>, &Setup<'_>) -> Result<Body, CtlError>,
    conn: &mut Option<Conn>,
) -> Result<Body, CtlError> {
    let setup = Setup::resolve(&deps.env, globals)?;
    let prepared = prepare(args, &setup)?;
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
    c.set_stage(stage_config(&setup, deps, L::SPEC.path));
    #[cfg(test)]
    c.set_stage_token(deps.stage_token.clone());
    let run = c.prelude(&setup.pv_dir, &setup.profile, check(&prepared))?;
    body(args, prepared, &mut Ctx::new(c, run, &setup.profile), &setup).map_err(|e| attach_hint(e, &setup.profile))
}

#[cfg(test)]
#[path = "proc_tests.rs"]
mod proc_tests;
#[cfg(test)]
#[path = "script_tests.rs"]
mod script_tests;
#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
