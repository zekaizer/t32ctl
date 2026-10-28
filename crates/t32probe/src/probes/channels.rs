// SPDX-License-Identifier: Apache-2.0
//! Values between the client and Lab scripts without macros: channel-in, channel-out.
//!
//! A path a script writes through a macro or `PRACTICE.ARG()` sits behind an `IF` that compares
//! the value with the literal the client expects, so a value that expanded to nothing never
//! writes into PowerView's working directory. `~~~~` is only read (`DO`, `Data.LOAD.Binary`).

use std::time::Duration;

use t32rcl::MacroName;
use t32stage::Undo;

use super::lab::{Answer, Case, CaseRecord, Expect, Lab, LabSizes, Slot, eval_text};
use crate::{
    ctx::{ProbeCtx, ProbeResult},
    outcome::Value,
};

fn case(id: impl Into<String>, body: &str, expect: Expect) -> Case {
    Case::new(id, body, expect, Duration::from_secs(10))
}

/// `{P2}`: a Windows path with a space, `&`, quotes and `;`.
const P2: &[u8] = b"C:\\x y\\&z \"q\";";
/// `{P3}`: "ü한" in UTF-8.
const P3: &[u8] = &[0xC3, 0xBC, 0xED, 0x95, 0x9C];
const VL: [u32; 6] = [255, 1024, 2047, 4095, 4096, 8192];
const A7: [usize; 4] = [256, 1024, 2000, 2037];
const SAFE_LINE: &str = "x,y a/b k=v P:0x10 m+n@o-p.q_r";

const VI1: &str = r#"@a
PRIVATE &v
&v=Data.STRing(VM:{P0})
@ask eq "&v" == "{S}\vi dir"
IF "&v"=="{S}\vi dir"
  MKDIR "&v\sub"
@z
@put 0 "[&v]"
@v
ENDDO"#;
const VI2: &str = r#"@a
PRIVATE &v
&v=Data.STRing(VM:{P1})
@ask empty "&v" == ""
@z
ENDDO"#;
const VI3: &str = r#"@a
@z
OPEN #1 "{S}\vi3-out.txt" /Create
WRITE #1 Data.STRing(VM:{P2})
WRITE #1 Data.STRing(VM:{P3})
CLOSE #1
@v
ENDDO"#;
const PR: &str = r#"@a
GLOBAL &{q}_p_addr
&{q}_p_addr=Data.STRing(VM:{P4})
DO "{S}\proc.cmm"
@z
ENDDO"#;
pub(crate) const PROC: &str = r#"@ask seen "&{q}_p_addr" == "0x40000000"
ENDDO"#;
const A1: &str = r#"@a
@ask n PRACTICE.ARG.SIZE() == 5.
@ask p0 PRACTICE.ARG(0.) == "{S}\a b"
@ask p1 PRACTICE.ARG(1.) == "tok-1"
@ask p3 PRACTICE.ARG(3.) == "0x10"
@ask p4 PRACTICE.ARG(4.) == ""
@z
PRIVATE &a0 &a2
&a0=PRACTICE.ARG(0.)
&a2=PRACTICE.ARG(2.)
@put 0 "[&a0]"
@put 1 "[&a2]"
@v
ENDDO"#;
const A2: &str = r#"@a
PARAMETERS &p0 &p1
@ask p0 "&p0" == "{S}\a b"
@ask p1 "&p1" == "tok-1"
IF "&p0"=="{S}\a b"
  MKDIR "&p0\dp"
@z
@put 0 "[&p0]"
@put 1 "[&p1]"
@v
ENDDO"#;
const A3N: &str = r#"@a
LOCAL &a &b &c
ENTRY &a &b &c
@ask b "&b" == "tok-1"
@ask c "&c" == "x,y"
@ask a "&a" == "{S}\a b"
@z
ENDDO"#;
const A3Q: &str = r#"@a
LOCAL &a &b &c
ENTRY &a &b &c
@ask a &a == "{S}\a b"
@z
ENDDO"#;
const A4: &str = r#"@a
LOCAL &l
ENTRY %LINE &l
@ask l "&l" == "x,y a/b k=v P:0x10 m+n@o-p.q_r"
@z
@put 0 "[&l]"
@v
ENDDO"#;
const A5: &str = r#"@a
LOCAL &a &b &c &d &e &f
ENTRY &a &b &c &d &e &f
@ask a "&a" == "x,y"
@ask b "&b" == "a/b"
@ask c "&c" == "k=v"
@ask d "&d" == "P:0x10"
@ask e "&e" == "m+n@o-p.q_r"
@ask fa "&f" == "a"
@ask f "&f" == "a;b"
@z
ENDDO"#;
const A6: &str = r#"@a
LOCAL &a
ENTRY &a
@ask re "&a" == "ca-{run}"
@z
@put 0 "[&a]"
@v
ENDDO"#;
const EI: &str = r#"@a
PRIVATE &e
&e=EVAL.STRing()
@ask e "&e" == "eval-in-{run}"
@ask empty "&e" == ""
@z
ENDDO"#;
const FI: &str = r#"@a
PRIVATE &l1
OPEN #1 "{S}\in.txt" /Read
READ #1 %LINE &l1
CLOSE #1
@ask l1 "&l1" == "C:\x y\z"
@z
@put 0 "[&l1]"
@v
ENDDO"#;
/// `{S}\in.txt` of `fi`: a path line and a line with quotes, CRLF.
pub(crate) const IN_TXT: &str = "C:\\x y\\z\r\nline2 \"q\"\r\n";
const T4: &str = r#"@a
@ask ppd OS.PPD() == "{S}\t4"
@ask abs OS.FILE.ABSPATH("~~~~/x.bin") == OS.FILE.ABSPATH("{S}\t4\x.bin")
@z
Data.LOAD.Binary "~~~~/t1.bin" VM:{B}
@mark load
DO "~~~~/sub/b.cmm"
@mark sub
ENDDO"#;
pub(crate) const T4_SUB: &str = r#"@mark b
@ask ppdb OS.PPD() == "{S}\t4\sub"
ENDDO"#;

pub(crate) fn vi1() -> Case {
    case("vi1", VI1, Expect::End).dirs(&["vi dir/sub"])
}
pub(crate) fn vi2() -> Case {
    case("vi2", VI2, Expect::End)
}
pub(crate) fn vi3() -> Case {
    case("vi3", VI3, Expect::HaltAfter("z")).outputs(&["vi3-out.txt"])
}
pub(crate) fn vl(n: u32) -> Case {
    let body = format!(
        "@a\n@ask fn STRing.LENgth(Data.STRing(VM:{{PL}})) == {n}.\nPRIVATE &v\n&v=Data.STRing(VM:{{PL}})\n\
         @ask m STRing.LENgth(\"&v\") == {n}.\n@z\nENDDO"
    );
    case(format!("vl-{n}"), &body, Expect::HaltAfter("a"))
}
pub(crate) fn pr() -> Case {
    case("pr", PR, Expect::End)
}
const AB: &str = r#""{S}\a b""#;
pub(crate) fn a1() -> Case {
    case("a1", A1, Expect::End).args(format!(r#"{AB} tok-1 "q""r" 0x10 """#))
}
pub(crate) fn a2() -> Case {
    case("a2", A2, Expect::End).args(format!(r#"{AB} "tok-1""#)).dirs(&["a b/dp"])
}
pub(crate) fn a3n() -> Case {
    case("a3n", A3N, Expect::HaltAfter("a")).args(format!("{AB} tok-1 x,y"))
}
pub(crate) fn a3q() -> Case {
    case("a3q", A3Q, Expect::HaltAfter("a")).args(format!("{AB} tok-1 x,y"))
}
pub(crate) fn a4() -> Case {
    case("a4", A4, Expect::End).args(SAFE_LINE)
}
pub(crate) fn a5() -> Case {
    case("a5", A5, Expect::HaltAfter("a")).args(format!("{SAFE_LINE} a;b"))
}
pub(crate) fn a6() -> Case {
    case("a6", A6, Expect::End).args("&{q}_ca")
}
/// The whole `DO` line is `n` characters: `DO "<s>\a7-<n>.cmm" ` and `m` times `x`.
pub(crate) fn a7(s: &str, n: usize) -> Option<Case> {
    let m = n.checked_sub(format!("DO \"{s}\\a7-{n}.cmm\" ").len()).filter(|m| *m > 0)?;
    let body = format!("@a\nLOCAL &l\nENTRY %LINE &l\n@ask len STRing.LENgth(\"&l\") == {m}.\n@z\nENDDO");
    Some(case(format!("a7-{n}"), &body, Expect::HaltAfter("a")).args("x".repeat(m)))
}
pub(crate) fn ei() -> Case {
    case("ei", EI, Expect::End)
}
pub(crate) fn fi() -> Case {
    case("fi", FI, Expect::End)
}
/// `End`, not `HaltAfter("a")`: it `DO`s after `a`. A halt in its asks runs into the timeout.
pub(crate) fn t4() -> Case {
    case("t4", T4, Expect::End).script("t4/a.cmm")
}

/// A sub-script: the case that `DO`s it, its path below `{S}`, its body.
#[cfg(test)]
pub(crate) type Sub = (&'static str, &'static str, &'static str);

/// Every channel-in case with its sub-scripts, in run order.
#[cfg(test)]
pub(crate) fn in_cases(s: &str) -> (Vec<Case>, Vec<Sub>) {
    let mut v = vec![vi1(), vi2(), vi3()];
    v.extend(VL.map(vl));
    v.extend([pr(), a1(), a2(), a3n(), a3q(), a4(), a5(), a6()]);
    v.extend(A7.iter().filter_map(|n| a7(s, *n)));
    v.extend([ei(), fi(), t4()]);
    (v, vec![("pr", "proc.cmm", PROC), ("t4", "t4/sub/b.cmm", T4_SUB)])
}

fn put(rec: &CaseRecord, n: u8) -> Option<&str> {
    rec.puts.iter().find(|(k, _)| *k == n).and_then(|(_, s)| match s {
        Slot::Text(t) => Some(t.as_str()),
        _ => None,
    })
}

fn yes(rec: &CaseRecord, k: &str) -> bool {
    rec.answer(k) == Answer::Yes
}

/// Client to script without macros: `VM:` strings, `DO` arguments, `EVAL`, `READ`, `OS.PPD()`
/// and `~~~~` reads. Every case is recorded; nothing here decides the verdict.
pub fn channel_in(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let (mut lab, _) = Lab::open(ctx, "in", LabSizes::default())?;
    let result = channel_in_cases(ctx, &mut lab);
    let closed = lab.close(ctx);
    result.and(closed)
}

fn channel_in_cases(ctx: &mut ProbeCtx<'_>, lab: &mut Lab) -> ProbeResult {
    for d in ["vi dir", "a b", "t4", "t4/sub"] {
        lab.mkdir(ctx, d)?;
    }
    let q = lab.q();
    let ca = MacroName::new(&format!("{q}_ca"))?;
    ctx.push(Undo::ClearMacros(vec![MacroName::new(&format!("{q}_p_addr"))?, ca.clone()]))?;
    let vi_dir = lab.fill("{S}\\vi dir")?;
    lab.input(ctx, 0, format!("{vi_dir}\0").as_bytes())?;
    lab.input(ctx, 1, &[0])?;
    lab.input(ctx, 2, &[P2, &[0]].concat())?;
    lab.input(ctx, 3, &[P3, &[0]].concat())?;
    lab.input(ctx, 4, b"0x40000000\0")?;

    let strs =
        [eval_text(ctx, &lab.fill("Data.STRing(VM:{P0})")?)?, eval_text(ctx, &lab.fill("Data.String(VM:{P0})")?)?];
    ctx.measure("vmstr_raw", Value::Text(format!("{:?} {:?}", strs[0], strs[1])));
    let vmstr = match strs[0].as_deref() {
        None => "refused",
        Some(v) if v == vi_dir => "ok",
        Some(v) if v == format!("\"{vi_dir}\"") => "quoted",
        Some(_) => "other",
    };
    ctx.text("vmstr", vmstr);

    let vi1 = lab.run(ctx, &vi1())?;
    let made = vi1.ran() && lab.dir_exists(ctx, "vi dir/sub")?;
    let vmpath = match vi1.answer("eq") {
        Answer::Yes if made => "ok",
        Answer::No if put(&vi1, 0) == Some("[]") => "empty",
        Answer::No => "other",
        _ => "FAIL",
    };
    ctx.text("vmpath", vmpath);
    let vi2 = lab.run(ctx, &vi2())?;
    ctx.text("guard", if yes(&vi2, "empty") { "ok" } else { "FAIL" });

    let vi3 = lab.run(ctx, &vi3())?;
    let got = if vi3.ran() { lab.fetch(ctx, "vi3-out.txt")? } else { None };
    ctx.text("vmbytes", vmbytes(got.as_deref()));

    let (mut fn_max, mut macro_max) = (0, 0);
    for n in VL {
        let text: String = (0..n).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        lab.input_long(ctx, format!("{text}\0").as_bytes())?;
        let rec = lab.run(ctx, &vl(n))?;
        if yes(&rec, "fn") {
            fn_max = n;
        }
        if yes(&rec, "m") {
            macro_max = n;
        }
    }
    ctx.text("vmlen", format!("{fn_max}/{macro_max}"));

    lab.stage_sub(ctx, "pr", "proc.cmm", PROC)?;
    let pr = lab.run(ctx, &pr())?;
    ctx.text("p1param", if yes(&pr, "seen") { "ok" } else { "FAIL" });

    let quoted_ab = format!("[\"{}\"]", lab.fill("{S}\\a b")?);
    let a1 = lab.run(ctx, &a1())?;
    let arg = match () {
        _ if !a1.ran() => "-",
        _ if yes(&a1, "p0") => "raw",
        _ if put(&a1, 0) == Some(&quoted_ab) => "quoted",
        _ => "other",
    };
    ctx.text("arg", arg);
    let a2 = lab.run(ctx, &a2())?;
    let params = match () {
        _ if yes(&a2, "p0") && yes(&a2, "p1") => "ok",
        _ if put(&a2, 0) == Some(&quoted_ab) => "quoted",
        _ => "FAIL",
    };
    ctx.text("params", params);
    let made = a2.ran() && lab.dir_exists(ctx, "a b/dp")?;
    ctx.measure("params_mkdir", Value::Flag(made));
    let (a3n, a3q) = (lab.run(ctx, &a3n())?, lab.run(ctx, &a3q())?);
    let entry = match () {
        _ if yes(&a3n, "a") => "raw",
        _ if yes(&a3q, "a") => "quoted",
        _ => "?",
    };
    ctx.text("entry", entry);
    let (a4, a5) = (lab.run(ctx, &a4())?, lab.run(ctx, &a5())?);
    ctx.text("comma", if yes(&a5, "a") { "kept" } else { "split" });
    let bad = std::iter::once((&a4, "l"))
        .chain(["a", "b", "c", "d", "e"].map(|k| (&a5, k)))
        .find(|(rec, k)| !yes(rec, k))
        .map(|(_, k)| k);
    ctx.text("argsafe", bad.map_or("ok".to_string(), |k| format!("bad:{k}")));
    let semi = match () {
        _ if yes(&a5, "f") => "kept",
        _ if yes(&a5, "fa") => "cut",
        _ if a5.has("a") && !a5.has("z") => "halt",
        _ => "other",
    };
    ctx.text("semi", semi);

    super::soft(ctx.session.cmd(&format!("GLOBAL {}", ca.amp())))?;
    super::soft(ctx.session.cmd(&lab.fill("&{q}_ca=\"ca-{run}\"")?))?;
    let a6 = lab.run(ctx, &a6())?;
    ctx.text("reexp", if yes(&a6, "re") { "yes" } else { "no" });

    let s = lab.fill("{S}")?;
    let mut dolen = 0;
    for n in A7 {
        if let Some(c) = a7(&s, n)
            && yes(&lab.run(ctx, &c)?, "len")
        {
            dolen = n;
        }
    }
    ctx.text("dolen", dolen.to_string());

    super::soft(ctx.session.cmd(&lab.fill("EVAL \"eval-in-{run}\"")?))?;
    let ei = lab.run(ctx, &ei())?;
    let evalin = match () {
        _ if yes(&ei, "e") => "ok",
        _ if yes(&ei, "empty") => "empty",
        _ => "other",
    };
    ctx.text("evalin", evalin);

    lab.stage_file(ctx, "in.txt", IN_TXT)?;
    let fi = lab.run(ctx, &fi())?;
    ctx.text("read", if yes(&fi, "l1") { "ok" } else { "FAIL" });

    lab.stage_file(ctx, "t4/t1.bin", "1")?;
    ctx.session.mem_write(&lab.blob().addr(), &[0])?;
    lab.stage_sub(ctx, "t4", "t4/sub/b.cmm", T4_SUB)?;
    let t4 = lab.run(ctx, &t4())?;
    let b0 = super::soft(ctx.session.mem_read(&lab.blob().addr(), 1))?;
    let loaded = t4.has("load") && b0.as_deref() == Some(&[0x31]);
    let pwd = eval_text(ctx, "OS.DIR(OS.PWD()+\"\\~~~~\")")?;
    ctx.measure("tilde4_pwd", Value::Text(format!("{pwd:?}")));
    ctx.text("ppd", if yes(&t4, "ppd") { "ok" } else { "FAIL" });
    let tilde4 = match () {
        _ if !t4.has("z") => "FAIL",
        _ if !loaded => "noload",
        _ if !t4.has("b") => "nodo",
        _ => "ok",
    };
    ctx.text("tilde4", tilde4);

    let abs = eval_text(ctx, "OS.FILE.ABSPATH(\"~~~/x\")")?;
    let ptd = eval_text(ctx, "OS.PTD()+\"\\x\"")?;
    ctx.measure("tilde3_raw", Value::Text(format!("{abs:?} {ptd:?}")));
    let same = matches!((&abs, &ptd), (Some(a), Some(p)) if !a.is_empty() && a.eq_ignore_ascii_case(p));
    ctx.text("tilde3", if same { "ok" } else { "no" });
    Ok(())
}

/// `exact` when the file holds `{P2}` and `{P3}` as two lines (CRLF or LF), else where the CRLF
/// form first differs.
fn vmbytes(got: Option<&[u8]>) -> String {
    let Some(got) = got else { return "-".into() };
    let crlf = [P2, b"\r\n", P3, b"\r\n"].concat();
    let lf = [P2, b"\n", P3, b"\n"].concat();
    if got == crlf || got == lf {
        return "exact".into();
    }
    let at = got.iter().zip(&crlf).position(|(a, b)| a != b).unwrap_or(got.len().min(crlf.len()));
    format!("diff@{at}")
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod channels_tests;
