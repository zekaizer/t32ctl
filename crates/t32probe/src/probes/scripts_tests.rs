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

#[test]
fn script_error_tokens_report_cases_that_never_ran_as_dash() {
    let cut = |id: &str| CaseRecord { id: id.into(), status: "cut", ..CaseRecord::default() };
    let failed = rec(&["a", "failed", "z"], 0);
    let mut runs: Vec<(&str, bool, CaseRecord)> =
        ERR_SUBS.iter().filter(|(_, m, _)| *m).map(|(n, _, _)| (*n, true, failed.clone())).collect();
    runs[5].2 = cut("r-err-save");
    runs.push(("stop", false, cut("r-stop")));
    runs.push(("ok_false", false, cut("r-ok-false")));
    let t = ErrTokens::of(&runs, &cut("es"), &cut("ig"), false);
    assert_eq!(t.caught, "5/5");
    assert_eq!((t.stop, t.ok_false, t.save_err, t.ifgoto), ("-", "-", "-", "-"));
}

#[test]
fn script_error_runs_es_and_ig_first_and_fits_its_deadline() {
    let order = run_order();
    let ids: Vec<&str> = order.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(&ids[..2], ["es", "ig"]);
    assert_eq!(ids.len(), 2 + ERR_SUBS.len());
    let worst: Duration = order.iter().map(|c| c.timeout).sum();
    let budget = crate::registry::ProbeId::ScriptError.def().deadline - crate::probes::lab::LAB_RESERVE;
    assert!(worst <= budget, "every case timing out takes {worst:?}, the Lab has {budget:?}");
}

#[test]
fn the_nested_size_check_names_the_file_through_a_macro() {
    let lines: Vec<&str> = SUB_B.lines().collect();
    assert_eq!(
        lines,
        [
            "&p=OS.PresentPracticeDirectory()+\"\\deep\\data.bin\"",
            "&size=OS.FILE.SIZE(\"&p\")",
            "IF &size!=4097.",
            "(",
            "  &tilde=OS.FILE.SIZE(\"~~~~\\deep\\data.bin\")",
            "  &expr=OS.FILE.SIZE(OS.PresentPracticeDirectory()+\"\\deep\\data.bin\")",
            "  ENDDO \"size-mismatch p=&p size=&size tilde=&tilde expr=&expr\"",
            ")",
            "WAIT 1.s",
            "DO \"~~~~/deep/c.cmm\"",
            "ENDDO \"chain-ok\"",
        ]
    );
}
