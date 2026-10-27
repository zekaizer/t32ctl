// SPDX-License-Identifier: Apache-2.0
//! `t32ctl` skeleton: `status` only, enough to prove the crate layout. Phase 1 commands build on
//! `t32rcl` and `t32stage`.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use t32rcl::{ConnectOpts, Endpoint, Error, ErrorClass, Feature, Session, Support};

#[derive(Debug, Parser)]
#[command(name = "t32ctl", version, about = "TRACE32 control over RCL")]
struct Cli {
    /// PowerView host as seen from WSL.
    #[arg(long, default_value = "127.0.0.1", global = true)]
    host: String,
    /// RCL port (NETTCP).
    #[arg(long, default_value_t = Endpoint::DEFAULT_PORT, global = true)]
    port: u16,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// One line: PowerView build, target state, RCL clients.
    Status,
}

/// Exit codes by what the caller should do next: 0 ok, 20 a human is needed (no connection,
/// hung, not RCL), 30 fix the input (usage, PowerView refused), 130 interrupted.
fn exit_code(e: &Error) -> u8 {
    match e.class() {
        ErrorClass::Usage | ErrorClass::Rejected => 30,
        ErrorClass::Unreachable | ErrorClass::ConnectionFatal => 20,
        ErrorClass::Cancelled => 130,
    }
}

/// `NrUsedCons` is optional: refused on a build the table does not vouch for, it prints `?`.
fn status_line(s: &mut Session) -> Result<String, Error> {
    let build = s.build()?;
    let state = s.target_state()?;
    let expr = "api.RCL.TCP.NrUsedCons()";
    let clients = match s.eval(expr).and_then(|v| v.into_int(expr)) {
        Ok(n) => n.to_string(),
        Err(e) if e.class() == ErrorClass::Rejected && build.support(Feature::NrUsedCons) == Support::Unknown => {
            "?".to_string()
        }
        Err(e) => return Err(e),
    };
    Ok(format!("pv build={build} state={} clients={clients}", state.name()))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Command::Status = cli.command;
    let result = Session::connect(Endpoint::new(&cli.host, cli.port), ConnectOpts::default())
        .map_err(Error::from)
        .and_then(|mut s| status_line(&mut s));
    match result {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("t32ctl: {e}");
            if let Error::Connect(c) = &e {
                eprintln!("next: {}", c.hint());
            }
            ExitCode::from(exit_code(&e))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use t32rcl::{
        ConnectError,
        testkit::{ManualClock, Request, Script, ScriptedConnector},
    };

    use super::*;

    fn session(script: Script) -> Session {
        let clock = ManualClock::new();
        let opts = ConnectOpts {
            connector: Arc::new(ScriptedConnector::new(clock.clone()).push(Ok(script))),
            clock: Arc::new(clock),
            ..ConnectOpts::default()
        };
        Session::connect(Endpoint::new("pv", 20000), opts).unwrap()
    }

    fn build(s: Script, build: i64) -> Script {
        s.expect_fn("SOFTWARE.BUILD()")
            .reply_decimal(build)
            .expect_fn("SOFTWARE.BUILD.BASE()")
            .reply_decimal(build - 100)
    }

    #[test]
    fn status_reports_build_state_and_clients() {
        let s = build(Script::new().attach(), 172_803).expect_req(Request::TargetState).reply_state(2);
        let mut s = session(s.expect_fn("api.RCL.TCP.NrUsedCons()").reply_decimal(1));
        assert_eq!(status_line(&mut s).unwrap(), "pv build=172803/base 172703 state=stopped clients=1");
    }

    #[test]
    fn a_refused_optional_function_on_an_old_build_prints_a_question_mark() {
        let s = build(Script::new().attach(), 150_000).expect_req(Request::TargetState).reply_state(0);
        let mut s = session(s.expect_fn("api.RCL.TCP.NrUsedCons()").reply_function(0, "unknown"));
        assert_eq!(status_line(&mut s).unwrap(), "pv build=150000/base 149900 state=down clients=?");
    }

    #[test]
    fn exit_codes_follow_the_error_class() {
        let refused = Error::Connect(ConnectError::Refused { endpoint: Endpoint::new("pv", 1) });
        assert_eq!(exit_code(&refused), 20);
        assert_eq!(exit_code(&Error::Local(t32rcl::LocalError::EmbeddedNul)), 30);
    }
}
