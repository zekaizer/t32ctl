// SPDX-License-Identifier: Apache-2.0
//! Opt-in probe decisions against canned frames.

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    env::ScriptOut,
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    probes::lab::lab_tests::{closed_down, files_removed, listed, opened, readable, sampled, staged, started},
    registry::ProbeId,
};

const DEPTH: &str = "PRACTICE.StackDepth()";

#[test]
fn dialog_stops_on_state_2_and_ends_only_its_own_script() {
    const TAG: &str = "dlg";
    let case = dlg();
    let mark = |s: Script, m: &str, v: bool| readable(s, TAG, &format!("dlg-{m}.bin"), v);
    let mut s = started(staged(opened(Script::new().attach(), TAG), TAG, &case, ScriptOut::Off), TAG, &case);
    s = mark(mark(sampled(s, 2, 1), "a", true), "z", false);
    s = sampled(s.expect_req(Request::Stop).reply_error(1, ""), 0, 1);
    s = s.expect_fn(DEPTH).reply_decimal(1).expect_cmd("END").reply_ok().expect_fn(DEPTH).reply_decimal(0);
    s = mark(mark(s.expect_fn(DEPTH).reply_decimal(0), "h", true), "z", false);
    s = files_removed(listed(s, TAG, &[]), TAG, &case, ScriptOut::Off, &[]);
    let mut session = connect(vec![Ok(closed_down(s, TAG))]);
    let mut env = test_env("dialog-state2");
    env.cfg.gates.dialog = true;
    let r = run(&mut env, &mut session, &[ProbeId::Dialog.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [("state2", "seen"), ("stop", "st1"), ("after", "0/1"), ("stack_clean", "yes"), ("z", "no")] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
