// SPDX-License-Identifier: Apache-2.0
//! Macros between the client and Lab scripts: macro-visibility, macro-return.
//!
//! Every answer comes from the file oracle; EXP reads are recorded beside it. Names are
//! `{q}_<x>` and values carry the run id.

use std::time::Duration;

use t32rcl::{MacroName, MacroSpelling};
use t32stage::Undo;

use super::{
    exp_as,
    lab::{Answer, Case, CaseRecord, Expect, Lab, LabSizes},
    short, soft,
};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::Value,
};

fn case(id: impl Into<String>, body: &str, expect: Expect) -> Case {
    Case::new(id, body, expect, Duration::from_secs(10))
}

fn yes(rec: &CaseRecord, k: &str) -> bool {
    rec.answer(k) == Answer::Yes
}

/// `%X%` is the macro suffix of the case, `rd1-v1` reads `&{q}_v1` and expects `"v1-{run}"`.
const RD1: &str = r#"@a
@ask val "&{q}_%X%" == "%X%-{run}"
@ask empty "&{q}_%X%" == ""
@z
@put 0 "[&{q}_%X%]"
@v
ENDDO"#;
const EX: &str = r#"@a
PMACRO.EXPLICIT
&{q}_%X%="w-%X%-{run}"
@z
ENDDO"#;
const RD2: &str = r#"@a
@ask w "&{q}_%X%" == "w-%X%-{run}"
@ask val "&{q}_%X%" == "%X%-{run}"
@ask empty "&{q}_%X%" == ""
@z
ENDDO"#;
const REF_U1: &str = r#"@a
PMACRO.EXPLICIT
@ask any "&{q}_none" == ""
@z
ENDDO"#;
const IM: &str = r#"@a
&{q}_im="im-{run}"
DO "{S}\im-sub.cmm"
@z
ENDDO"#;
pub(crate) const IM_SUB: &str = r#"@ask seen "&{q}_im" == "im-{run}"
ENDDO"#;
/// Suffixes in run order: EXP set (bare, amp, each with and without `GLOBAL`), command-line
/// assignment with and without `GLOBAL`, declared only, never created.
pub(crate) const VIS: [&str; 8] = ["v1", "v2", "v3", "v4", "c1", "c2", "e1", "u1"];

fn per(id: &str, x: &str, body: &str) -> Case {
    case(format!("{id}-{x}"), &body.replace("%X%", x), Expect::HaltAfter("a"))
}
pub(crate) fn rd1(x: &str) -> Case {
    per("rd1", x, RD1)
}
pub(crate) fn ex(x: &str) -> Case {
    per("ex", x, EX)
}
pub(crate) fn rd2(x: &str) -> Case {
    per("rd2", x, RD2)
}
pub(crate) fn rd3_v1() -> Case {
    per("rd3", "v1", RD2)
}
pub(crate) fn ref_u1() -> Case {
    case("ref-u1", REF_U1, Expect::HaltAfter("a"))
}
pub(crate) fn im() -> Case {
    case("im", IM, Expect::End)
}

/// Every macro-visibility case with its sub-script, in run order.
#[cfg(test)]
pub(crate) fn vis_cases() -> (Vec<Case>, Vec<super::channels::Sub>) {
    let mut v = Vec::new();
    for x in VIS {
        v.extend([rd1(x), ex(x), rd2(x)]);
        if x == "v1" {
            v.push(rd3_v1());
        }
    }
    v.extend([ref_u1(), im()]);
    (v, vec![("im", "im-sub.cmm", IM_SUB)])
}

/// Which value a read case saw: `-` not run, `halt` when it stopped before `z`.
fn seen(rec: &CaseRecord, keys: &[&str]) -> String {
    if !rec.ran() {
        return "-".into();
    }
    match keys.iter().find(|k| yes(rec, k)) {
        Some(k) => k.to_string(),
        None if !rec.has("z") => "halt".into(),
        None => "other".into(),
    }
}

/// An EXP get under both spellings: `b:<v>,a:<v>`, each value at most 16 characters.
fn both(ctx: &mut ProbeCtx<'_>, name: &MacroName) -> Result<(String, Result<String, String>), ProbeFail> {
    let learned = ctx.session.tuning().macro_spelling();
    let (b, a) = (
        exp_as(ctx, MacroSpelling::Bare, |s| s.macro_get(name))?,
        exp_as(ctx, MacroSpelling::Ampersand, |s| s.macro_get(name))?,
    );
    let text = |r: &Result<String, String>| r.as_ref().map_or_else(Clone::clone, |v| short(v));
    let token = format!("b:{},a:{}", text(&b), text(&a));
    Ok((token, if learned == MacroSpelling::Bare { b } else { a }))
}

/// Client macros seen by `DO`'d scripts: EXP set under each spelling, command-line `GLOBAL` and
/// assignment, declared-only and never-created names, `PMACRO.EXPLICIT` and implicit
/// visibility. Teaches `exp_to_script`; nothing here decides the verdict.
pub fn macro_visibility(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "vis", LabSizes::default())?;
    let result = visibility_cases(ctx, &mut lab);
    let closed = lab.close(ctx);
    result.and(closed)
}

fn visibility_cases(ctx: &mut ProbeCtx<'_>, lab: &mut Lab) -> ProbeResult {
    let q = lab.q();
    let name = |x: &str| MacroName::new(&format!("{q}_{x}"));
    let made: Vec<MacroName> =
        ["v1", "v2", "v3", "v4", "c1", "c2", "e1", "im"].iter().map(|x| name(x)).collect::<Result<_, _>>()?;
    ctx.push(Undo::ClearMacros(made))?;
    for x in VIS {
        let m = name(x)?;
        let value = lab.fill(&format!("{x}-{{run}}"))?;
        let global = format!("GLOBAL {}", m.amp());
        match x {
            "v1" | "v2" | "v3" | "v4" => {
                if x == "v1" || x == "v3" {
                    soft(ctx.session.cmd(&global))?;
                }
                let s = if matches!(x, "v1" | "v2") { MacroSpelling::Bare } else { MacroSpelling::Ampersand };
                let set = exp_as(ctx, s, |s| s.macro_set(&m, &value))?;
                ctx.measure(&format!("set.{x}"), Value::Text(set.map_or_else(|e| e, |()| "ok".into())));
            }
            "c1" | "c2" => {
                if x == "c1" {
                    soft(ctx.session.cmd(&global))?;
                }
                let ok = soft(ctx.session.cmd(&format!("{}=\"{value}\"", m.amp())))?.is_some();
                ctx.measure(&format!("set.{x}"), Value::Flag(ok));
            }
            "e1" => {
                soft(ctx.session.cmd(&global))?;
            }
            _ => {}
        }
        let (token, _) = both(ctx, &m)?;
        ctx.measure(&format!("prep.{x}"), Value::Text(token));
    }

    let mut left = 0;
    for x in VIS {
        let m = name(x)?;
        let r1 = lab.run(ctx, &rd1(x))?;
        if x == "v1" && r1.ran() {
            ctx.learned.exp_to_script = Some(yes(&r1, "val"));
        }
        let e = lab.run(ctx, &ex(x))?;
        let r2 = lab.run(ctx, &rd2(x))?;
        let (exp, learned) = both(ctx, &m)?;
        left += u64::from(learned.is_ok());
        let ex_token = match () {
            _ if !e.ran() => "-",
            _ if e.has("z") => "ok",
            _ => "halt",
        };
        let rd2_token = seen(&r2, &["w", "val", "empty"]);
        ctx.text(x, format!("{}/{ex_token}/{rd2_token}/{exp}", seen(&r1, &["val", "empty"])));
        if x == "v1" {
            soft(ctx.session.cmd(&format!("GLOBAL {}", m.amp())))?;
            let r3 = seen(&lab.run(ctx, &rd3_v1())?, &["w", "val", "empty"]);
            let redeclare = match (rd2_token.as_str(), r3.as_str()) {
                (a, b) if a == b && (a == "w" || a == "val") => "keeps",
                (a, "empty") if a != "empty" => "resets",
                _ => "other",
            };
            ctx.text("redeclare", redeclare);
        }
    }
    let r = lab.run(ctx, &ref_u1())?;
    ctx.text("explicit_ref", if r.has("z") { "ok" } else { "halt" });
    lab.stage_sub(ctx, "im", "im-sub.cmm", IM_SUB)?;
    let im = lab.run(ctx, &im())?;
    ctx.text("implicit", if yes(&im, "seen") { "visible" } else { "hidden" });
    let (learned, im_name) = (ctx.session.tuning().macro_spelling(), name("im")?);
    left += u64::from(exp_as(ctx, learned, |s| s.macro_get(&im_name))?.is_ok());
    ctx.measure("globals_left", Value::Count(left));
    Ok(())
}

#[cfg(test)]
#[path = "macros_tests.rs"]
mod macros_tests;
