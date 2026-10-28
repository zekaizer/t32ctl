// SPDX-License-Identifier: Apache-2.0
//! script-error templates and decisions.

use std::{collections::BTreeSet, time::Duration};

use super::*;
use crate::{
    env::ScriptOut,
    probes::lab::{CaseRecord, Ending, fill_text, lab_tests::test_fill, render},
};

#[test]
fn script_error_marks_failed_before_any_risky_line() {
    let f = test_fill("err");
    for (name, _, body) in ERR_SUBS {
        fill_text(body, &f, "").unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let cases: Vec<Case> = ERR_SUBS.iter().map(|(n, _, _)| err_case(n)).chain([es_case(), ig_case()]).collect();
    for out in [ScriptOut::VmSet, ScriptOut::Write, ScriptOut::Off] {
        for c in &cases {
            render(c, &f, out).unwrap_or_else(|e| panic!("{} {out:?}: {e}", c.id));
        }
        let text = render(&err_case("err_top"), &f, out).unwrap().text;
        let at = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap_or(usize::MAX);
        assert!(text.contains("DO \"C:\\T\\t32ctl\\probe-r1-err\\err_top.cmm\""), "{text}");
        assert!(at("t32p_failed:") < at("-failed.bin"), "{text}");
        for risky in ["StackDepth", "ERROR.MESSAGE", "PRIVATE"] {
            assert!(at("r-err-top-failed.bin") < at(risky), "{risky} before the failed marker:\n{text}");
            assert!(at("r-err-top-z.bin") < at(risky), "{risky} before z:\n{text}");
        }
    }
    assert_eq!(err_case("err_top").id, "r-err-top");
}

fn rec(markers: &[&str], depth: i64) -> CaseRecord {
    CaseRecord {
        id: "x".into(),
        status: "ok",
        markers: markers.iter().map(|m| m.to_string()).collect::<BTreeSet<_>>(),
        depth,
        ending: Some((Ending::Ended, Duration::ZERO)),
        ..CaseRecord::default()
    }
}

#[test]
fn script_error_tokens_never_call_a_failing_script_a_success() {
    let mut failed = rec(&["a", "failed", "z", "v"], 0);
    failed.answers = vec![("d1".into(), Answer::No)];
    failed.puts = vec![(0, Slot::Text("[unknown command]".into()))];
    let mut runs: Vec<(&str, bool, CaseRecord)> =
        ERR_SUBS.iter().filter(|(_, m, _)| *m).map(|(n, _, _)| (*n, true, failed.clone())).collect();
    runs[3].2 = rec(&["a", "d", "ok", "z"], 0);
    runs.push(("stop", false, rec(&["a"], 1)));
    runs.push(("ok_false", false, rec(&["a", "d", "ok", "z"], 0)));
    let (es, ig) = (rec(&["a", "caught", "z"], 0), rec(&["a", "jumped", "dirfalse", "sizene", "z"], 0));
    let t = ErrTokens::of(&runs, &es, &ig, false);
    let want = ErrTokens {
        caught: "5/6".into(),
        false_ok: "err_data".into(),
        stop: "halted",
        ok_false: "ok",
        hdepth: "other",
        after: "depth1".into(),
        msg: "slot",
        save_err: "caught",
        ifgoto: "ok",
    };
    assert_eq!(t, want);
    let t = ErrTokens::of(&runs, &rec(&["a", "notcaught", "z"], 0), &rec(&["a", "fell", "z"], 0), true);
    assert_eq!((t.save_err, t.ifgoto), ("created", "FAIL"));
}
