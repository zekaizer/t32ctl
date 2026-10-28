// SPDX-License-Identifier: Apache-2.0
//! `t32probe`: Phase 0 measurement tool. Parse, wire, run, print, exit; the logic lives in
//! `registry`, `exec`, `outcome`, `report` and the library crates.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod cli;
mod ctx;
mod env;
mod exec;
mod outcome;
mod probes;
mod registry;
mod report;
mod stats;

use std::{
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use t32rcl::{Cancel, Endpoint, Session};
use t32stage::CleanupReport;

use crate::{
    cli::Args,
    env::{Env, WslFacts},
    exec::{Executor, Observer},
    outcome::Outcome,
    registry::ProbeDef,
};

fn main() -> ExitCode {
    let args = Args::parse();
    if args.list {
        print!("{}", if args.markdown { registry::list_markdown() } else { registry::list_text() });
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("t32probe: {msg}");
            ExitCode::from(2)
        }
    }
}

/// Progress on stderr and the JSON file, rewritten after every probe.
struct Progress {
    json: Option<PathBuf>,
    total: usize,
    done: usize,
    interrupted: Arc<AtomicBool>,
}

impl Observer for Progress {
    fn starting(&mut self, def: &ProbeDef) {
        eprintln!("t32probe: [{}/{}] {}", self.done + 1, self.total, def.id.name());
    }

    fn finished(&mut self, env: &Env, outcomes: &[Outcome], replay: &CleanupReport) {
        self.done = outcomes.len();
        if let Some(path) = &self.json {
            let interrupted = self.interrupted.load(std::sync::atomic::Ordering::SeqCst);
            let doc = report::json_report(env, outcomes, replay, env.learned.build, interrupted);
            if let Err(e) = report::write_json_atomic(path, &doc) {
                eprintln!("t32probe: --json {}: {e}", path.display());
            }
        }
    }
}

fn run_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{:04x}", (nanos ^ std::process::id().rotate_left(16)) & 0xFFFF)
}

/// Whether this process started with SIGHUP ignored: bit `SIGHUP - 1` of `SigIgn` in
/// `/proc/self/status`. An unreadable mask counts as not ignored.
fn sighup_ignored() -> bool {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let mask =
        status.lines().find_map(|l| l.strip_prefix("SigIgn:")).and_then(|m| u64::from_str_radix(m.trim(), 16).ok());
    mask.is_some_and(|m| m & (1 << (SIGHUP - 1)) != 0)
}

fn run(args: &Args) -> Result<u8, String> {
    let started = Instant::now();
    // First Ctrl-C, SIGTERM or SIGHUP (a closed terminal) sets the flag: the current probe stops,
    // its cleanup runs, the partial report prints. A second one exits at once with 130; the JSON
    // written so far stays valid. A SIGHUP the caller ignored (`nohup`) stays ignored.
    let flag = Arc::new(AtomicBool::new(false));
    let hup = (!sighup_ignored()).then_some(SIGHUP);
    for sig in [SIGINT, SIGTERM].into_iter().chain(hup) {
        signal_hook::flag::register_conditional_shutdown(sig, 130, Arc::clone(&flag)).map_err(|e| e.to_string())?;
        signal_hook::flag::register(sig, Arc::clone(&flag)).map_err(|e| e.to_string())?;
    }
    let (mut env, selected) = args.plan(WslFacts::read(), run_id())?;
    env.cancel = Cancel::from_flag(Arc::clone(&flag));
    let mut session = Session::unconnected(Endpoint::new(&env.cfg.hosts[0], env.cfg.port), env.connect_opts());
    let mut progress =
        Progress { json: args.json.clone(), total: selected.len(), done: 0, interrupted: Arc::clone(&flag) };
    let result = Executor::new(&mut env, &mut session, &mut progress).run(&selected);
    let not_run = registry::not_run(&env.cfg.gates);
    print!("{}", report::render_text(&result, &env, env.learned.build, &not_run, started.elapsed()));
    Ok(outcome::exit_code(&result.outcomes, result.interrupted))
}
