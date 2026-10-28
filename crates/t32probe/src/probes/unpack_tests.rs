// SPDX-License-Identifier: Apache-2.0
//! unpack-literal templates and decisions.

use t32rcl::testkit::Script;

use super::*;
use crate::{
    env::ScriptOut,
    exec::tests::{connect, run, test_env},
    probes::lab::{
        Fill,
        lab_tests::{closed_down_large, dir, opened_large, test_fill},
        render,
    },
    registry::ProbeId,
};

fn fill() -> Fill {
    Fill { l: Some(0x0103_6000), ..test_fill("unp") }
}

#[test]
fn every_unpack_case_renders_and_u1r_removes_before_each_write() {
    let t = Duration::from_secs(1);
    let cases = [gd_case(), u1_case("u1", 0x0200_0000, false, t), u1_case("u1r", 0x0200_0000, true, t), u2_case(t)];
    for c in &cases {
        for out in [ScriptOut::VmSet, ScriptOut::Write, ScriptOut::Off] {
            render(c, &fill(), out).unwrap_or_else(|e| panic!("{} {out:?}: {e}", c.id));
        }
    }
    let u1 = render(&cases[1], &fill(), ScriptOut::Off).unwrap().text;
    assert_eq!(u1.lines().filter(|l| l.starts_with("Data.SAVE.Binary") && l.contains("\\u1\\")).count(), 65);
    assert!(u1.contains("\\u1\\d7\\f7.bin\" VM:0x020FC000++0x00003FFF"), "{u1}");
    let u1r = render(&cases[2], &fill(), ScriptOut::Off).unwrap().text;
    let lines: Vec<&str> = u1r.lines().collect();
    let mut writes = 0;
    for (i, l) in lines.iter().enumerate() {
        let target = ["Data.SAVE.Binary \"", "OPEN #1 \""].iter().find_map(|p| l.strip_prefix(p));
        let Some(rest) = target.filter(|r| r.contains("\\u1\\")) else { continue };
        let p = &rest[..rest.find('"').unwrap()];
        assert_eq!(lines[i - 2], format!("IF OS.FILE.readable(\"{p}\")==TRUE()"), "line {i}");
        assert_eq!(lines[i - 1], format!("  RM \"{p}\""), "line {i}");
        writes += 1;
    }
    assert_eq!(writes, 66);
}

#[test]
fn unpack_literal_learns_abspath_identity() {
    let d = dir("unp");
    let s = opened_large(Script::new().attach(), "unp", 1 << 20);
    let first = s.expect_fn(&format!("OS.FILE.ABSPATH(\"{d}\")")).reply_text(&d);
    let first = first.expect_fn(&format!("OS.FILE.readable(\"{d}\\gd-a.bin\")")).eof();
    let second = Script::new().attach().expect_fn("TRUE()").reply_bool(true);
    let mut session = connect(vec![Ok(first), Ok(closed_down_large(second, "unp", 1 << 20))]);
    let mut env = test_env("unpack-abspath");
    let r = run(&mut env, &mut session, &[ProbeId::UnpackLiteral.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.lookup("abspath"), Some(&Value::Text("same".into())), "{o:?}");
    assert_eq!(env.learned.abspath_same, Some(true));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
