// SPDX-License-Identifier: Apache-2.0
//! debugger-info against canned frames.

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    registry::ProbeId,
};

/// A stopped target on a build that lacks `PowerView.StartMode` and refuses to say whether it
/// has `STATE.RESET`; every other function answers.
fn conversation() -> Script {
    let mut s = Script::new().attach().expect_req(Request::TargetState).reply_state(2);
    for call in INFO {
        let name = call.split('(').next().unwrap();
        let avail = s.expect_fn(&format!("PRACTICE.FUNCtion.AVAILable({name})"));
        s = match name {
            "PowerView.StartMode" => {
                s = avail.reply_bool(false);
                continue;
            }
            "STATE.RESET" => {
                s = avail.reply_function(0, "unknown function");
                continue;
            }
            _ => avail.reply_bool(true),
        };
        let call = call.replace("{i}", "1.");
        let at = s.expect_fn(&call);
        s = match call.as_str() {
            "SYStem.Mode()" => at.reply_function(4, "0xB"),
            "SYStem.JtagClock()" => at.reply_decimal(10_000_000),
            "STATE.POWER()" => at.reply_bool(true),
            "LICENSE.getINDEX()" => at.reply_decimal(1),
            "LICENSE.DATE(1.)" => at.reply_text("2026/09"),
            "VERSION.SOFTWARE()" => at.reply_text("R.2024.09.000168999"),
            "SYStem.CONFIG.DEBUGPORTTYPE()" => at.reply_text("JTAG"),
            "IFCONFIG.CONFIGURATION()" => at.reply_text("USB3"),
            "CABLE.NAME()" => at.reply_text("ARM Debug Cable V4b"),
            _ => at.reply_text("x"),
        };
    }
    s
}

#[test]
fn debugger_info_reads_each_available_function_and_names_the_mode() {
    let mut session = connect(vec![Ok(conversation())]);
    let mut env = test_env("debugger-info");
    let r = run(&mut env, &mut session, &[ProbeId::DebuggerInfo.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    let text = |v: &str| Some(Value::Text(v.into()));
    for (k, v) in [
        ("avail", format!("{}/{}", INFO.len() - 2, INFO.len())),
        ("state", "stopped".into()),
        ("software", "R.2024.09.000168999".into()),
        ("mode", "0xb Up".into()),
        ("jtag_clock", "10000000".into()),
        ("port", "JTAG".into()),
        ("link", "USB3".into()),
        ("cable", "ARM Debug Cable V4b".into()),
        ("power", "TRUE".into()),
        ("licence_date", "2026/09".into()),
    ] {
        assert_eq!(o.lookup(k).cloned(), text(&v), "{k}");
    }
    assert_eq!(o.lookup("info.PowerView.StartMode()").cloned(), text("absent (FALSE)"));
    assert_eq!(o.lookup("info.STATE.RESET()").cloned(), text("absent (err0)"));
    assert!(INFO.len() > 40, "the table covers every category of the survey");
}
