// SPDX-License-Identifier: Apache-2.0
//! Scripted sessions for unit tests.

use std::{sync::Arc, time::Duration};

use t32rcl::{
    CallBudget, ConnectOpts, Endpoint, Session,
    testkit::{ConnectFailure, ManualClock, Script, ScriptedConnector},
};

pub fn session_with(scripts: Vec<Result<Script, ConnectFailure>>, tweak: impl FnOnce(&mut ConnectOpts)) -> Session {
    let clock = ManualClock::new();
    let mut connector = ScriptedConnector::new(clock.clone());
    for s in scripts {
        connector = connector.push(s);
    }
    let mut opts = ConnectOpts {
        connector: Arc::new(connector),
        clock: Arc::new(clock),
        budget: CallBudget::idle(Duration::from_secs(1)).with_total(Duration::from_secs(60)),
        ..ConnectOpts::default()
    };
    tweak(&mut opts);
    Session::connect(Endpoint::new("pv", 20000), opts).expect("scripted attach")
}

pub fn session(script: Script) -> Session {
    session_with(vec![Ok(script)], |_| {})
}

pub fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d =
        std::env::temp_dir().join(format!("t32stage-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
