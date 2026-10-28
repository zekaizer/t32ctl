// SPDX-License-Identifier: Apache-2.0
//! `t32ctl`: every report goes to stdout; stderr carries only a panic.

use std::{
    io::Write,
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
};

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use t32rcl::Cancel;

fn main() -> ExitCode {
    // The first Ctrl-C, SIGTERM or SIGHUP (a closed terminal) sets the flag: the call in flight is
    // cancelled, cleanup runs with its own deadline, and the report says so. A second one exits at
    // once with 130. A SIGHUP the caller ignored (`nohup`) stays ignored.
    let flag = Arc::new(AtomicBool::new(false));
    let hup = (!sighup_ignored()).then_some(SIGHUP);
    for sig in [SIGINT, SIGTERM].into_iter().chain(hup) {
        signal_hook::flag::register_conditional_shutdown(sig, 130, Arc::clone(&flag))
            .and_then(|_| signal_hook::flag::register(sig, Arc::clone(&flag)))
            .expect("SIGINT, SIGTERM and SIGHUP accept a handler");
    }
    let deps = t32ctl::Deps::from_process(Cancel::from_flag(flag));
    let argv: Vec<_> = std::env::args_os().collect();
    let out = t32ctl::run(&argv, &deps);
    // A closed stdout leaves nobody to tell; the exit status still carries the result.
    let _ = std::io::stdout().lock().write_all(out.text.as_bytes());
    ExitCode::from(out.code)
}

/// Whether this process started with SIGHUP ignored: bit `SIGHUP - 1` of `SigIgn` in
/// `/proc/self/status`. An unreadable mask counts as not ignored.
fn sighup_ignored() -> bool {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let mask =
        status.lines().find_map(|l| l.strip_prefix("SigIgn:")).and_then(|m| u64::from_str_radix(m.trim(), 16).ok());
    mask.is_some_and(|m| m & (1 << (SIGHUP - 1)) != 0)
}
