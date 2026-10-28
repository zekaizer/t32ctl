// SPDX-License-Identifier: Apache-2.0
//! macro-visibility and macro-return decisions against canned frames.

use t32rcl::testkit::{Request, Script};

use super::*;
use crate::{
    env::ScriptOut,
    exec::tests::{connect, run, test_env},
    outcome::{Value, Verdict},
    probes::{
        channels::channels_tests::{file_staged, ran, sub_removed, sub_staged},
        lab::lab_tests::{closed_down, dir, files_removed, listed, opened, readable, sampled, staged},
        scriptlab::scriptlab_tests::diagnosed,
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

/// macro-visibility where every script sees the client's values, except that the case Stage
/// reproduces (`v1` bare, `v3` amp) answers `val` only when `seen`.
fn visibility(amp: bool, seen: bool) -> Script {
    let learned = |x: &str| if amp { format!("&{}", q(x)) } else { q(x) };
    let stage_case = if amp { "v3" } else { "v1" };
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
        let val: &[&str] = if x != stage_case || seen { &["val"] } else { &[] };
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
    s = s.expect_req(Request::MacroGet { name: learned("im") }).reply_macro("im-r1");
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
        s = s.expect_req(Request::MacroSet { name: learned(x), value: String::new() }).reply_macro("");
    }
    closed_down(s, VIS_TAG)
}

#[test]
fn macro_visibility_learns_exp_to_script_from_the_case_stage_reproduces() {
    for (amp, seen) in [(false, true), (false, false), (true, true), (true, false)] {
        let mut session = connect(vec![Ok(visibility(amp, seen))]);
        let mut env = test_env(&format!("macro-visibility-{amp}-{seen}"));
        if amp {
            session.set_tuning(session.tuning().with_macro_spelling(t32rcl::MacroSpelling::Ampersand));
            env.learned.macro_spelling = Some(t32rcl::MacroSpelling::Ampersand);
        }
        let r = run(&mut env, &mut session, &[ProbeId::MacroVisibility.def()]);
        let o = &r.outcomes[0];
        assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
        assert_eq!(env.learned.exp_to_script, Some(seen), "amp={amp}");
        if amp {
            continue;
        }
        let rd1 = if seen { "val" } else { "other" };
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

const RET_TAG: &str = "ret";
const DEPTH: &str = "PRACTICE.StackDepth()";

fn mark(s: Script, id: &str, m: &str, v: bool) -> Script {
    readable(s, RET_TAG, &format!("{id}-{m}.bin"), v)
}

fn begun(s: Script, case: &Case) -> Script {
    let s = staged(s, RET_TAG, case, ScriptOut::Off);
    sampled(s, 0, 0).expect_cmd(&format!("DO \"{}\\{}.cmm\"", dir(RET_TAG), case.id)).reply_ok()
}

/// Three idle samples on a stack of one after `a` and `h`, then the diagnostics.
fn held(mut s: Script, id: &str, first: bool) -> Script {
    for k in 0..3 {
        s = sampled(s, 0, 1);
        if first && k == 0 {
            s = mark(mark(s, id, "a", true), id, "h", true);
        }
        s = mark(s, id, "z", false);
    }
    diagnosed(s, "stopped", "")
}

/// While `hh` is halted: both reads of `h1`, the client's set and command.
fn poked(s: Script) -> Script {
    let s = gets(s, "h1", "halt-h1");
    let s = s.expect_req(Request::MacroSet { name: q("h1"), value: "client-h1".into() }).reply_macro("");
    s.expect_cmd("&t32ctl_qr1_h2=\"cmd-h2\"").reply_ok()
}

fn asks(s: Script, id: &str, answers: &[(&str, bool)]) -> Script {
    answers.iter().fold(s, |s, (k, y)| mark(mark(s, id, &format!("{k}-yes"), *y), id, &format!("{k}-no"), !y))
}

fn halt_only(ctx: &mut crate::ctx::ProbeCtx<'_>) -> crate::ctx::ProbeResult {
    let (mut lab, _) = Lab::open(ctx, RET_TAG, LabSizes::default())?;
    let h1 = MacroName::new("t32ctl_qr1_h1")?;
    halted(ctx, &mut lab, &h1)?;
    lab.close(ctx)
}

#[test]
fn macro_return_continues_once_more_after_a_rehalt() {
    let mut s = held(begun(opened(Script::new().attach(), RET_TAG), &hh()), "hh", true);
    s = poked(s).expect_cmd("CONTinue").reply_ok();
    s = held(s, "hh", false).expect_cmd("CONTinue").reply_ok();
    s = held(s, "hh", false);
    s = s.expect_fn(DEPTH).reply_decimal(1).expect_cmd("END").reply_ok().expect_fn(DEPTH).reply_decimal(0);
    s = mark(s.expect_fn(DEPTH).reply_decimal(0), "hh", "z", false);
    s = asks(s, "hh", &[("h1c", false), ("h1s", true), ("h2c", false), ("h2s", true)]);
    s = files_removed(listed(s, RET_TAG, &[]), RET_TAG, &hh(), ScriptOut::Off, &[]);
    let mut session = connect(vec![Ok(closed_down(s, RET_TAG))]);
    let mut env = test_env("macro-return-rehalt");
    let def: &'static crate::registry::ProbeDef = Box::leak(Box::new(crate::registry::ProbeDef {
        id: ProbeId::MacroReturn,
        tier: crate::registry::Tier::Safe,
        requires: &[],
        needs_connection: true,
        deadline: std::time::Duration::from_secs(60),
        about: "",
        run: halt_only,
    }));
    let r = run(&mut env, &mut session, &[def]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    assert_eq!(o.lookup("halt"), Some(&Value::Text("get:halt-h1,set:n,cmd:n,cont:rehalt".into())));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}

fn bare_get(s: Script, x: &str, v: &str) -> Script {
    s.expect_req(Request::MacroGet { name: q(x) }).reply_macro(v)
}

#[test]
fn macro_return_records_every_step_on_a_build_that_answers_everything() {
    let mut s = opened(Script::new().attach(), RET_TAG);
    s = gets(ran(s, RET_TAG, &g1(), &[], None), "s1", "s1-r1");
    s = ran(s, RET_TAG, &g2(), &["seen"], None);
    s = s.expect_fn("PRACTICE.FUNCtion.AVAILable(PRACTICE.GLOBAL.VALUE)").reply_bool(true);
    s = s.expect_fn("PRACTICE.GLOBAL.VALUE(\"t32ctl_qr1_s1\")").reply_text("s1-r1");
    for x in ["d1", "d2"] {
        s = global(s, x);
        s = if x == "d1" {
            s.expect_req(Request::MacroSet { name: q(x), value: "init-r1".into() }).reply_macro("")
        } else {
            s.expect_cmd(&format!("&{}=\"init-r1\"", q(x))).reply_ok()
        };
        s = ran(bare_get(ran(s, RET_TAG, &wr(x), &["init"], None), x, "fs-r1"), RET_TAG, &rd(x), &["fs"], None);
    }
    s = global(s, "d3").expect_cmd("&t32ctl_qr1_d3=\"init-r1\"").reply_ok();
    s = ran(
        ran(file_staged(s, RET_TAG, "ret.cmm", crate::probes::channels::RET), RET_TAG, &en(), &[], None),
        RET_TAG,
        &rd_d3(),
        &["chain"],
        None,
    );
    s = bare_get(s, "d3", "chain ok");

    s = sampled(staged(s, RET_TAG, &fr(), ScriptOut::Off), 0, 0);
    s = s.expect_cmd(&format!("DO \"{}\\fr.cmm\"", dir(RET_TAG))).reply_ok();
    s = mark(mark(mark(sampled(s, 1, 1), "fr", "a", true), "fr", "w", true), "fr", "z", false);
    s = gets(gets(s, "loc", "loc-r1"), "glo", "glo-r1");
    s = gets(mark(sampled(s, 0, 0), "fr", "z", true), "glo", "glo-r1").expect_fn(DEPTH).reply_decimal(0);

    s = poked(held(begun(s, &hh()), "hh", true)).expect_cmd("CONTinue").reply_ok();
    s = mark(sampled(s, 0, 0), "hh", "z", true).expect_fn(DEPTH).reply_decimal(0);
    s = asks(s, "hh", &[("h1c", true), ("h1s", false), ("h2c", true), ("h2s", false)]);

    s = s.expect_req(Request::MacroSet { name: q("mk"), value: "mk-r1".into() }).reply_macro("");
    s = global(s, "mk2").expect_cmd("&t32ctl_qr1_mk2=\"mk2-r1\"").reply_ok();
    s = bare_get(bare_get(ran(s, RET_TAG, &e0(), &[], None), "mk", "mk-r1"), "mk2", "mk2-r1");
    s = held(begun(s, &st()), "st", true).expect_fn(DEPTH).reply_decimal(1).expect_fn(DEPTH).reply_decimal(1);
    s = mark(s.expect_cmd("END").reply_ok().expect_fn(DEPTH).reply_decimal(0), "st", "z", false);
    s = bare_get(bare_get(s, "mk", "mk-r1"), "mk2", "mk2-r1");
    s = bare_get(ran(ran(s, RET_TAG, &rd_mk2("rd-mk2"), &["mk2"], None), RET_TAG, &mk3(), &[], None), "mk2", "x-r1");
    s = ran(s, RET_TAG, &rd_mk2("rd-mk2b"), &["x"], None);

    for (y, v) in NQ {
        let (one, two): (&[&str], &[&str]) = match y {
            "a" | "e" => (&["s"], &[]),
            "h" => (&[], &["s"]),
            _ => (&[], &[]),
        };
        s = ran(ran(s, RET_TAG, &nq(y, v, 1), one, None), RET_TAG, &nq(y, v, 2), two, None);
    }
    s = ran(s, RET_TAG, &nqn(), &["b0", "c0", "d2d", "gt"], None);
    s = ran(s, RET_TAG, &ng(), &[], None);
    for (x, v) in [("n1", "0"), ("n2", "0."), ("n3", "2.")] {
        s = bare_get(s, x, v);
    }
    s = ran(ran(ran(s, RET_TAG, &pa(), &[], None), RET_TAG, &pa3(), &[], None), RET_TAG, &pb(), &["p", "p2a"], None);
    let pa2 = PA2.replace("{q}", "t32ctl_qr1").replace("{run}", "r1");
    s = sub_staged(file_staged(s, RET_TAG, "pa2.cmm", &pa2), RET_TAG, "pc", "pd.cmm", PD);
    s = ran(s, RET_TAG, &pc(), &["pp", "lo", "p1", "r1"], Some(PD));
    s = ran(s, RET_TAG, &pe(), &["pp", "r1"], None);
    for x in RET_GLOBALS {
        s = bare_get(s, x, "x");
    }
    s = listed(s, RET_TAG, &[]);

    s = files_removed(files_removed(s, RET_TAG, &pe(), ScriptOut::Off, &[]), RET_TAG, &pc(), ScriptOut::Off, &[]);
    s = readable(sub_removed(s, RET_TAG, "pc", "pd.cmm", PD), RET_TAG, "pa2.cmm", false);
    let mut late = vec![pb(), pa3(), pa(), ng(), nqn()];
    for (y, v) in NQ.iter().rev() {
        late.extend([nq(y, v, 2), nq(y, v, 1)]);
    }
    late.extend([rd_mk2("rd-mk2b"), mk3(), rd_mk2("rd-mk2"), st(), e0(), hh(), fr(), rd_d3(), en()]);
    for c in &late {
        s = files_removed(s, RET_TAG, c, ScriptOut::Off, &[]);
    }
    s = readable(s, RET_TAG, "ret.cmm", false);
    for c in [rd("d2"), wr("d2"), rd("d1"), wr("d1"), g2(), g1()] {
        s = files_removed(s, RET_TAG, &c, ScriptOut::Off, &[]);
    }
    for x in RET_GLOBALS {
        s = s.expect_req(Request::MacroSet { name: q(x), value: String::new() }).reply_macro("");
    }
    let mut session = connect(vec![Ok(closed_down(s, RET_TAG))]);
    let mut env = test_env("macro-return");
    let r = run(&mut env, &mut session, &[ProbeId::MacroReturn.def()]);
    let o = &r.outcomes[0];
    assert_eq!(o.verdict, Verdict::Pass, "{o:?}");
    for (k, v) in [
        ("s1", "exp:b,persist:y,pgv:s1-r1"),
        ("d1", "saw:init,after:fs,exp:fs-r1"),
        ("d2", "saw:init,after:fs,exp:fs-r1"),
        ("d3", "chain/chain ok"),
        ("frame", "loc:loc-r1,glo:glo-r1,after:glo-r1"),
        ("halt", "get:halt-h1,set:y,cmd:y,cont:ok"),
        ("marker", "exp:kept,cmd:kept,implicit:overwrote"),
        ("num", "b0/c0/d2d/gt"),
        ("quote", "a:plain,e:plain,h:quoted,f:?"),
        ("gen", "amp-expanded"),
        ("persist", "ok"),
        ("nest", "pp:y,lo:y,p1:y,r1:y"),
    ] {
        assert_eq!(o.lookup(k), Some(&Value::Text(v.into())), "{k}");
    }
    assert_eq!(o.lookup("globals_left"), Some(&Value::Count(17)));
    assert!(o.cleanup.iter().all(|c| c.clean), "{:?}", o.cleanup);
}
