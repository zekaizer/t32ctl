// SPDX-License-Identifier: Apache-2.0
//! Macros between the client and Lab scripts: macro-visibility, macro-return.
//!
//! Every answer comes from the file oracle; EXP reads are recorded beside it. Names are
//! `{q}_<x>` and values carry the run id.

use std::time::Duration;

use t32rcl::{MacroName, MacroSpelling};
use t32stage::Undo;

use super::{
    channels::RET,
    exp_as, hex,
    lab::{Answer, Case, CaseRecord, Ending, Expect, Lab, LabSizes, Seen, Slot, Until, eval_text},
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

/// An EXP get under both spellings; a refusal is `err<code>`.
struct Reads {
    bare: Result<String, String>,
    amp: Result<String, String>,
    learned: MacroSpelling,
}

impl Reads {
    fn of(ctx: &mut ProbeCtx<'_>, name: &MacroName) -> Result<Reads, ProbeFail> {
        Ok(Reads {
            bare: exp_as(ctx, MacroSpelling::Bare, |s| s.macro_get(name))?,
            amp: exp_as(ctx, MacroSpelling::Ampersand, |s| s.macro_get(name))?,
            learned: ctx.session.tuning().macro_spelling(),
        })
    }

    /// `b:<v>,a:<v>`, each value at most 16 characters.
    fn token(&self) -> String {
        format!("b:{},a:{}", text(&self.bare), text(&self.amp))
    }

    fn learned(&self) -> &Result<String, String> {
        if self.learned == MacroSpelling::Bare { &self.bare } else { &self.amp }
    }

    /// `b`, `a`, `both` or `none`: the spellings that read `want`.
    fn which(&self, want: &str) -> &'static str {
        match (self.bare.as_deref() == Ok(want), self.amp.as_deref() == Ok(want)) {
            (true, true) => "both",
            (true, false) => "b",
            (false, true) => "a",
            (false, false) => "none",
        }
    }
}

/// A read value at most 16 characters, or the refusal.
fn text(r: &Result<String, String>) -> String {
    r.as_ref().map_or_else(Clone::clone, |v| short(v))
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
        let token = Reads::of(ctx, &m)?.token();
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
        let reads = Reads::of(ctx, &m)?;
        let exp = reads.token();
        left += u64::from(reads.learned().is_ok());
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

const G1: &str = r#"@a
GLOBAL &{q}_s1
&{q}_s1="s1-{run}"
@z
ENDDO"#;
const G2: &str = r#"@a
@ask seen "&{q}_s1" == "s1-{run}"
@z
ENDDO"#;
const WR: &str = r#"@a
@ask init "&{q}_%X%" == "init-{run}"
&{q}_%X%="fs-{run}"
@z
ENDDO"#;
const RD: &str = r#"@a
@ask fs "&{q}_%X%" == "fs-{run}"
@ask init "&{q}_%X%" == "init-{run}"
@z
ENDDO"#;
/// `d` follows the last `DO`: a case may halt only after every `DO` returned.
const EN: &str = r#"@a
DO "{S}\ret.cmm"
@mark d
ENTRY %LINE &{q}_d3
@z
ENDDO"#;
const RD_D3: &str = r#"@a
@ask chain "&{q}_d3" == "chain ok"
@ask init "&{q}_d3" == "init-{run}"
@z
ENDDO"#;
const FR: &str = r#"@a
LOCAL &{q}_loc
&{q}_loc="loc-{run}"
GLOBAL &{q}_glo
&{q}_glo="glo-{run}"
@w
WAIT 2.s
@z
ENDDO"#;
const HH: &str = r#"@a
GLOBAL &{q}_h1 &{q}_h2
&{q}_h1="halt-h1"
&{q}_h2="halt-h2"
@h
STOP
@ask h1c "&{q}_h1" == "client-h1"
@ask h1s "&{q}_h1" == "halt-h1"
@ask h2c "&{q}_h2" == "cmd-h2"
@ask h2s "&{q}_h2" == "halt-h2"
@z
ENDDO"#;
const E0: &str = "@a\n@z\nENDDO";
const ST: &str = "@a\n@h\nSTOP\n@z\nENDDO";
const MK3: &str = r#"@a
&{q}_mk2="x-{run}"
@z
ENDDO"#;
const RD_MK2: &str = r#"@a
@ask mk2 "&{q}_mk2" == "mk2-{run}"
@ask x "&{q}_mk2" == "x-{run}"
@z
ENDDO"#;
/// `%V%` is the stored value: plain when `nq<y>1` answers yes, quoted when `nq<y>2` does.
const NQ1: &str = r#"@a
PRIVATE &x
&x=%V%
@ask s "&x" == %V%
@z
ENDDO"#;
const NQ2: &str = r#"@a
PRIVATE &x
&x=%V%
@ask s &x == %V%
@z
ENDDO"#;
const NQ: [(&str, &str); 4] = [("a", r#""0""#), ("e", r#""a b""#), ("h", r#""D:0x40100010""#), ("f", r#""a""b""#)];
const NQN: &str = r#"@a
PRIVATE &b &c &d &g
&b=0.
&c=0
&d=1.+1.
&g=TRUE()
@ask b0d "&b" == "0."
@ask b0 "&b" == "0"
@ask b0x "&b" == "0x0"
@ask c0 "&c" == "0"
@ask c0x "&c" == "0x0"
@ask d2d "&d" == "2."
@ask d2x "&d" == "0x2"
@ask d2e "&d" == "1.+1."
@ask gt "&g" == "TRUE()"
@ask g1 "&g" == "1"
@ask gtt "&g" == "TRUE"
@z
@put 0 "[&b|&c|&d|&g]"
@v
ENDDO"#;
const NG: &str = r#"@a
GLOBAL &{q}_n1 &{q}_n2 &{q}_n3
&{q}_n1="0"
&{q}_n2=0.
&{q}_n3=1.+1.
@z
ENDDO"#;
const PA: &str = r#"@a
GLOBAL &{q}_p
&{q}_p="p-{run}"
@z
ENDDO"#;
const PA3: &str = r#"@a
GLOBAL &{q}_p2
&{q}_p2="a&{q}_never"
@z
ENDDO"#;
const PB: &str = r#"@a
@ask p "&{q}_p" == "p-{run}"
@ask p2a "&{q}_p2" == "a"
@z
@put 0 "[&{q}_p2]"
@v
ENDDO"#;
pub(crate) const PA2: &str = "GLOBAL &{q}_pp\n&{q}_pp=\"pp-{run}\"\nENDDO\n";
pub(crate) const PD: &str = r#"@ask lo "&{q}_lo" == "lo-{run}"
@ask p1 "&{q}_p1" == "param-{run}"
&{q}_r1="result-{run}"
ENDDO"#;
const PC: &str = r#"@a
DO "{S}\pa2.cmm"
@ask pp "&{q}_pp" == "pp-{run}"
LOCAL &{q}_lo
&{q}_lo="lo-{run}"
GLOBAL &{q}_p1 &{q}_r1
&{q}_p1="param-{run}"
&{q}_r1=""
DO "{S}\pd.cmm"
@mark d
@ask r1 "&{q}_r1" == "result-{run}"
@z
ENDDO"#;
const PE: &str = r#"@a
@ask pp "&{q}_pp" == "pp-{run}"
@ask r1 "&{q}_r1" == "result-{run}"
@z
ENDDO"#;
/// Globals macro-return creates or assigns; `LOCAL` names are left out.
const RET_GLOBALS: [&str; 17] =
    ["s1", "d1", "d2", "d3", "glo", "h1", "h2", "mk", "mk2", "n1", "n2", "n3", "p", "p2", "pp", "p1", "r1"];

fn halt(id: &str, body: &str) -> Case {
    case(id, body, Expect::HaltAfter("a"))
}
fn ended(id: &str, body: &str) -> Case {
    case(id, body, Expect::End)
}
pub(crate) fn g1() -> Case {
    halt("g1", G1)
}
pub(crate) fn g2() -> Case {
    ended("g2", G2)
}
pub(crate) fn wr(x: &str) -> Case {
    halt(&format!("wr-{x}"), &WR.replace("%X%", x))
}
pub(crate) fn rd(x: &str) -> Case {
    ended(&format!("rd-{x}"), &RD.replace("%X%", x))
}
pub(crate) fn en() -> Case {
    case("en", EN, Expect::HaltAfter("d"))
}
pub(crate) fn rd_d3() -> Case {
    ended("rd-d3", RD_D3)
}
pub(crate) fn fr() -> Case {
    Case::new("fr", FR, Expect::End, Duration::from_secs(15))
}
pub(crate) fn hh() -> Case {
    Case::new("hh", HH, Expect::HaltAfter("h"), Duration::from_secs(20))
}
pub(crate) fn e0() -> Case {
    ended("e0", E0)
}
pub(crate) fn st() -> Case {
    case("st", ST, Expect::HaltAfter("h"))
}
pub(crate) fn mk3() -> Case {
    halt("mk3", MK3)
}
pub(crate) fn rd_mk2(id: &str) -> Case {
    ended(id, RD_MK2)
}
pub(crate) fn nq(y: &str, v: &str, form: u8) -> Case {
    halt(&format!("nq{y}{form}"), &if form == 1 { NQ1 } else { NQ2 }.replace("%V%", v))
}
pub(crate) fn nqn() -> Case {
    halt("nqn", NQN)
}
pub(crate) fn ng() -> Case {
    halt("ng", NG)
}
pub(crate) fn pa() -> Case {
    halt("pa", PA)
}
pub(crate) fn pa3() -> Case {
    halt("pa3", PA3)
}
pub(crate) fn pb() -> Case {
    halt("pb", PB)
}
pub(crate) fn pc() -> Case {
    case("pc", PC, Expect::HaltAfter("d"))
}
pub(crate) fn pe() -> Case {
    halt("pe", PE)
}

/// Every macro-return case with its sub-script, in run order.
#[cfg(test)]
pub(crate) fn ret_cases() -> (Vec<Case>, Vec<super::channels::Sub>) {
    let mut v = vec![g1(), g2(), wr("d1"), rd("d1"), wr("d2"), rd("d2"), en(), rd_d3(), fr(), hh(), e0(), st()];
    v.extend([rd_mk2("rd-mk2"), mk3(), rd_mk2("rd-mk2b")]);
    for (y, val) in NQ {
        v.extend([nq(y, val, 1), nq(y, val, 2)]);
    }
    v.extend([nqn(), ng(), pa(), pa3(), pb(), pc(), pe()]);
    (v, vec![("pc", "pd.cmm", PD)])
}

fn yn(b: bool) -> char {
    if b { 'y' } else { 'n' }
}

/// Script macros seen by the client and by later scripts: script `GLOBAL` over EXP, scripts
/// writing client globals, frames during `WAIT`, EXP while halted and `CONTinue`, values across
/// `END`, the stored form of values, and generated wrappers. Nothing here decides the verdict.
pub fn macro_return(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "ret", LabSizes::default())?;
    let result = return_cases(ctx, &mut lab);
    let closed = lab.close(ctx);
    result.and(closed)
}

fn return_cases(ctx: &mut ProbeCtx<'_>, lab: &mut Lab) -> ProbeResult {
    let q = lab.q();
    let name = |x: &str| MacroName::new(&format!("{q}_{x}"));
    let globals: Vec<MacroName> = RET_GLOBALS.iter().map(|x| name(x)).collect::<Result<_, _>>()?;
    ctx.push(Undo::ClearMacros(globals.clone()))?;
    let learned = ctx.session.tuning().macro_spelling();
    let get = |ctx: &mut ProbeCtx<'_>, x: &str| -> Result<Result<String, String>, ProbeFail> {
        let m = name(x)?;
        exp_as(ctx, learned, |s| s.macro_get(&m))
    };
    let cmd = |ctx: &mut ProbeCtx<'_>, lab: &Lab, text: &str| -> Result<(), ProbeFail> {
        soft(ctx.session.cmd(&lab.fill(text)?))?;
        Ok(())
    };

    lab.run(ctx, &g1())?;
    let s1 = Reads::of(ctx, &name("s1")?)?;
    ctx.measure("s1_exp", Value::Text(s1.token()));
    let g2 = lab.run(ctx, &g2())?;
    let avail = soft(ctx.session.eval("PRACTICE.FUNCtion.AVAILable(PRACTICE.GLOBAL.VALUE)"))?;
    let pgv = match avail.and_then(|v| v.into_bool("PRACTICE.FUNCtion.AVAILable").ok()) {
        Some(true) => eval_text(ctx, &format!("PRACTICE.GLOBAL.VALUE(\"{q}_s1\")"))?.map_or("na".into(), |v| short(&v)),
        _ => "na".into(),
    };
    let want = lab.fill("s1-{run}")?;
    ctx.text("s1", format!("exp:{},persist:{},pgv:{pgv}", s1.which(&want), yn(yes(&g2, "seen"))));

    for x in ["d1", "d2"] {
        cmd(ctx, lab, &format!("GLOBAL &{{q}}_{x}"))?;
        if x == "d1" {
            let (m, v) = (name(x)?, lab.fill("init-{run}")?);
            let set = exp_as(ctx, MacroSpelling::Bare, |s| s.macro_set(&m, &v))?;
            ctx.measure("set.d1", Value::Text(set.map_or_else(|e| e, |()| "ok".into())));
        } else {
            cmd(ctx, lab, &format!("&{{q}}_{x}=\"init-{{run}}\""))?;
        }
        let w = lab.run(ctx, &wr(x))?;
        let exp = get(ctx, x)?;
        let r = lab.run(ctx, &rd(x))?;
        let saw = if yes(&w, "init") { "init" } else { "other" };
        let after = if yes(&r, "fs") {
            "fs"
        } else if yes(&r, "init") {
            "init"
        } else {
            "other"
        };
        ctx.text(x, format!("saw:{saw},after:{after},exp:{}", text(&exp)));
    }
    cmd(ctx, lab, "GLOBAL &{q}_d3")?;
    cmd(ctx, lab, "&{q}_d3=\"init-{run}\"")?;
    lab.stage_file(ctx, "ret.cmm", RET)?;
    lab.run(ctx, &en())?;
    let r = lab.run(ctx, &rd_d3())?;
    let exp = get(ctx, "d3")?;
    let d3 = if yes(&r, "chain") {
        "chain"
    } else if yes(&r, "init") {
        "init"
    } else {
        "other"
    };
    ctx.text("d3", format!("{d3}/{}", text(&exp)));

    frame(ctx, lab, &name("loc")?, &name("glo")?)?;
    halted(ctx, lab, &name("h1")?)?;

    let mk = name("mk")?;
    let mk_v = lab.fill("mk-{run}")?;
    let set = exp_as(ctx, MacroSpelling::Bare, |s| s.macro_set(&mk, &mk_v))?;
    ctx.measure("set.mk", Value::Text(set.map_or_else(|e| e, |()| "ok".into())));
    cmd(ctx, lab, "GLOBAL &{q}_mk2")?;
    cmd(ctx, lab, "&{q}_mk2=\"mk2-{run}\"")?;
    lab.run(ctx, &e0())?;
    let after_e0 = [get(ctx, "mk")?, get(ctx, "mk2")?];
    ctx.measure("marker_e0", Value::Text(format!("{after_e0:?}")));
    lab.run(ctx, &st())?;
    let after_st = [get(ctx, "mk")?, get(ctx, "mk2")?];
    ctx.measure("marker_st", Value::Text(format!("{after_st:?}")));
    lab.run(ctx, &rd_mk2("rd-mk2"))?;
    lab.run(ctx, &mk3())?;
    let after_mk3 = get(ctx, "mk2")?;
    ctx.measure("marker_mk3", Value::Text(text(&after_mk3)));
    let b = lab.run(ctx, &rd_mk2("rd-mk2b"))?;
    let kept = |r: &Result<String, String>, v: &str| if r.as_deref() == Ok(v) { "kept" } else { "lost" };
    let implicit = match () {
        _ if yes(&b, "x") => "overwrote",
        _ if yes(&b, "mk2") => "kept",
        _ => "other",
    };
    let mk2_v = lab.fill("mk2-{run}")?;
    ctx.text(
        "marker",
        format!("exp:{},cmd:{},implicit:{implicit}", kept(&after_st[0], &mk_v), kept(&after_st[1], &mk2_v)),
    );

    let mut num = Vec::new();
    let mut quote = Vec::new();
    for (y, v) in NQ {
        let (plain, quoted) = (lab.run(ctx, &nq(y, v, 1))?, lab.run(ctx, &nq(y, v, 2))?);
        let form = match () {
            _ if yes(&plain, "s") => "plain",
            _ if yes(&quoted, "s") => "quoted",
            _ => "?",
        };
        quote.push(format!("{y}:{form}"));
    }
    let n = lab.run(ctx, &nqn())?;
    for group in [&["b0d", "b0", "b0x"][..], &["c0", "c0x"], &["d2d", "d2x", "d2e"], &["gt", "g1", "gtt"]] {
        let hit: Vec<&str> = group.iter().copied().filter(|k| yes(&n, k)).collect();
        num.push(if hit.is_empty() { "-".to_string() } else { hit.join("+") });
    }
    ctx.text("num", num.join("/"));
    ctx.text("quote", quote.join(","));
    lab.run(ctx, &ng())?;
    for x in ["n1", "n2", "n3"] {
        let m = name(x)?;
        let p = exp_as(ctx, learned, |s| s.macro_get_payload(&m))?;
        ctx.measure(&format!("stored.{x}"), Value::Text(p.map_or_else(|e| e, |p| hex(&p))));
    }

    lab.run(ctx, &pa())?;
    lab.run(ctx, &pa3())?;
    let pb = lab.run(ctx, &pb())?;
    let wrapped = format!("[a&{q}_never]");
    let wrapper = match pb.puts.iter().find(|(k, _)| *k == 0).map(|(_, s)| s) {
        _ if yes(&pb, "p2a") => "amp-expanded",
        Some(Slot::Text(t)) if *t == wrapped => "ok",
        Some(Slot::NoChannel) | None => "?",
        Some(_) => "FAIL",
    };
    ctx.text("gen", wrapper);
    ctx.text("persist", if yes(&pb, "p") { "ok" } else { "FAIL" });
    lab.stage_file(ctx, "pa2.cmm", PA2)?;
    lab.stage_sub(ctx, "pc", "pd.cmm", PD)?;
    let pc = lab.run(ctx, &pc())?;
    let nest: Vec<String> = ["pp", "lo", "p1", "r1"].iter().map(|k| format!("{k}:{}", yn(yes(&pc, k)))).collect();
    ctx.text("nest", nest.join(","));
    lab.run(ctx, &pe())?;

    let mut left = 0;
    for m in &globals {
        left += u64::from(exp_as(ctx, learned, |s| s.macro_get(m))?.is_ok());
    }
    ctx.measure("globals_left", Value::Count(left));
    Ok(())
}

/// `fr`: the script's `LOCAL` and `GLOBAL` read over EXP while it waits, and the global after.
fn frame(ctx: &mut ProbeCtx<'_>, lab: &mut Lab, loc: &MacroName, glo: &MacroName) -> ProbeResult {
    let Some(mut run) = lab.begin(ctx, &fr())? else {
        ctx.text("frame", "-");
        return Ok(());
    };
    let (mut during, mut times) = (Vec::new(), Vec::new());
    if lab.wait(ctx, &mut run, Until::Marker("w"))? == Seen::Reached {
        for m in [loc, glo] {
            let t = ctx.session.now();
            during.push(Reads::of(ctx, m)?);
            times.push(ctx.session.now().saturating_duration_since(t).as_millis());
        }
        lab.wait(ctx, &mut run, Until::Done)?;
    }
    let after = if during.is_empty() { None } else { Some(Reads::of(ctx, glo)?) };
    lab.finish(ctx, run)?;
    let v = |r: Option<&Reads>| match r.map(Reads::learned) {
        Some(Ok(v)) if !v.is_empty() => short(v),
        _ => "-".into(),
    };
    ctx.measure("frame_reads", Value::Text(during.iter().map(Reads::token).collect::<Vec<_>>().join(" ")));
    ctx.measure("frame_ms", Value::Text(format!("{times:?}")));
    ctx.text("frame", format!("loc:{},glo:{},after:{}", v(during.first()), v(during.get(1)), v(after.as_ref())));
    Ok(())
}

/// `hh`: halted by `STOP`, the client reads and writes globals, then `CONTinue`; a script that
/// halts again gets one more `CONTinue`, then `END`.
pub(crate) fn halted(ctx: &mut ProbeCtx<'_>, lab: &mut Lab, h1: &MacroName) -> ProbeResult {
    let Some(mut run) = lab.begin(ctx, &hh())? else {
        ctx.text("halt", "-");
        return Ok(());
    };
    let is_halt = |s: &Seen| matches!(s, Seen::End(Ending::Halted { .. }));
    let mut got_cont = None;
    if is_halt(&lab.wait(ctx, &mut run, Until::Done)?) {
        let got = Reads::of(ctx, h1)?;
        ctx.measure("halt_reads", Value::Text(got.token()));
        let set = exp_as(ctx, got.learned, |s| s.macro_set(h1, "client-h1"))?;
        ctx.measure("halt_set", Value::Text(set.map_or_else(|e| e, |()| "ok".into())));
        soft(ctx.session.cmd(&lab.fill("&{q}_h2=\"cmd-h2\"")?))?;
        let cont = if !lab.cont(ctx, &mut run)? {
            lab.end(ctx, 3)?;
            "refused"
        } else if !is_halt(&lab.wait(ctx, &mut run, Until::Done)?) {
            "ok"
        } else {
            if lab.cont(ctx, &mut run)? && is_halt(&lab.wait(ctx, &mut run, Until::Done)?) {
                lab.end(ctx, 3)?;
            }
            "rehalt"
        };
        got_cont = Some((text(got.learned()), cont));
    }
    let rec = lab.finish(ctx, run)?;
    let token = got_cont.map_or("none".into(), |(get, cont)| {
        format!("get:{get},set:{},cmd:{},cont:{cont}", yn(yes(&rec, "h1c")), yn(yes(&rec, "h2c")))
    });
    ctx.text("halt", token);
    Ok(())
}

#[cfg(test)]
#[path = "macros_tests.rs"]
mod macros_tests;
