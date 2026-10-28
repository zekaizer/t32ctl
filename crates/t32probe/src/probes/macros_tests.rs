// SPDX-License-Identifier: Apache-2.0
//! macro-visibility and macro-return decisions against canned frames.

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    env::ScriptOut,
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    probes::{
        channels::channels_tests::{ran, sub_removed, sub_staged},
        lab::lab_tests::{closed_down, files_removed, listed, opened},
    },
    registry::ProbeId,
};

const VIS_TAG: &str = "vis";

fn q(x: &str) -> String {
    format!("t32ctl_qr1_{x}")
}

/// EXP get under both spellings: the bare one answers `v`, the ampersand one is refused.
fn gets(s: Script, x: &str, v: &str) -> Script {
    let s = s.expect_req(Request::MacroGet { name: q(x) }).reply_macro(v);
    s.expect_req(Request::MacroGet { name: format!("&{}", q(x)) }).reply_exp_error(1, "unknown macro")
}

fn global(s: Script, x: &str) -> Script {
    s.expect_cmd(&format!("GLOBAL &{}", q(x))).reply_ok()
}

/// macro-visibility where every script sees the client's values; `rd1-v1` answers `val` only
/// when `v1_seen`.
fn visibility(v1_seen: bool) -> Script {
    let mut s = opened(Script::new().attach(), VIS_TAG);
    for x in VIS {
        let v = format!("{x}-r1");
        match x {
            "v1" | "v3" => s = global(s, x),
            "c1" | "e1" => s = global(s, x),
            _ => {}
        }
        s = match x {
            "v1" | "v2" => s.expect_req(Request::MacroSet { name: q(x), value: v.clone() }).reply_macro(""),
            "v3" | "v4" => {
                s.expect_req(Request::MacroSet { name: format!("&{}", q(x)), value: v.clone() }).reply_macro("")
            }
            "c1" | "c2" => s.expect_cmd(&format!("&{}=\"{v}\"", q(x))).reply_ok(),
            _ => s,
        };
        s = gets(s, x, if x == "u1" || x == "e1" { "" } else { &v });
    }
    for x in VIS {
        let val: &[&str] = if x != "v1" || v1_seen { &["val"] } else { &[] };
        s = ran(s, VIS_TAG, &rd1(x), val, None);
        s = ran(ran(s, VIS_TAG, &ex(x), &[], None), VIS_TAG, &rd2(x), &["w"], None);
        s = gets(s, x, &format!("w-{x}-r1"));
        if x == "v1" {
            s = ran(global(s, x), VIS_TAG, &rd3_v1(), &["w"], None);
        }
    }
    s = ran(s, VIS_TAG, &ref_u1(), &["any"], None);
    s = sub_staged(s, VIS_TAG, "im", "im-sub.cmm", IM_SUB);
    s = ran(s, VIS_TAG, &im(), &["seen"], Some(IM_SUB));
    s = s.expect_req(Request::MacroGet { name: q("im") }).reply_macro("im-r1");
    s = listed(s, VIS_TAG, &[]);
    s = files_removed(s, VIS_TAG, &im(), ScriptOut::Off, &[]);
    s = files_removed(sub_removed(s, VIS_TAG, "im", "im-sub.cmm", IM_SUB), VIS_TAG, &ref_u1(), ScriptOut::Off, &[]);
    for x in VIS.iter().rev() {
        if *x == "v1" {
            s = files_removed(s, VIS_TAG, &rd3_v1(), ScriptOut::Off, &[]);
        }
        for c in [rd2(x), ex(x), rd1(x)] {
            s = files_removed(s, VIS_TAG, &c, ScriptOut::Off, &[]);
        }
    }
    for x in ["v1", "v2", "v3", "v4", "c1", "c2", "e1", "im"] {
        s = s.expect_req(Request::MacroSet { name: q(x), value: String::new() }).reply_macro("");
    }
    closed_down(s, VIS_TAG)
}

#[test]
fn macro_visibility_learns_exp_to_script_from_rd1_v1() {
    for v1_seen in [true, false] {
        let mut session = connect(vec![Ok(visibility(v1_seen))]);
        let mut env = test_env(&format!("macro-visibility-{v1_seen}"));
        let r = run(&mut env, &mut session, &[ProbeId::MacroVisibility.def()]);
        let o = &r.outcomes[0];
        assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
        assert_eq!(env.learned.exp_to_script, Some(v1_seen));
        let rd1 = if v1_seen { "val" } else { "other" };
        for (k, v) in [
            ("v1", format!("{rd1}/ok/w/b:w-v1-r1,a:err1")),
            ("redeclare", "keeps".into()),
            ("c2", "val/ok/w/b:w-c2-r1,a:err1".into()),
            ("explicit_ref", "ok".into()),
            ("implicit", "visible".into()),
        ] {
            assert_eq!(o.lookup(k), Some(&Value::Text(v)), "{k}");
        }
        assert_eq!(o.lookup("globals_left"), Some(&Value::Count(9)));
        assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
    }
}
