// SPDX-License-Identifier: Apache-2.0
//! `t32ctl`: every report goes to stdout; stderr carries only a panic.

use std::{
    io::Write,
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
};

use signal_hook::consts::{SIGINT, SIGTERM};
use t32rcl::Cancel;

fn main() -> ExitCode {
    // The first Ctrl-C or SIGTERM sets the flag: the call in flight is cancelled, cleanup runs
    // with its own deadline, and the report says so. A second one exits at once with 130.
    let flag = Arc::new(AtomicBool::new(false));
    for sig in [SIGINT, SIGTERM] {
        signal_hook::flag::register_conditional_shutdown(sig, 130, Arc::clone(&flag))
            .and_then(|_| signal_hook::flag::register(sig, Arc::clone(&flag)))
            .expect("SIGINT and SIGTERM accept a handler");
    }
    let deps = t32ctl::Deps::from_process(Cancel::from_flag(flag));
    let argv: Vec<_> = std::env::args_os().collect();
    let out = t32ctl::run(&argv, &deps);
    // A closed stdout leaves nobody to tell; the exit status still carries the result.
    let _ = std::io::stdout().lock().write_all(out.text.as_bytes());
    ExitCode::from(out.code)
}
