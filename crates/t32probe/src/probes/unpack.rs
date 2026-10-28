// SPDX-License-Identifier: Apache-2.0
//! unpack-literal: the shape of `bundle::render_unpack` on literal paths, timed, rerun over its
//! own files, and the two checks of its snapshot-path guard.

use std::time::Duration;

use t32stage::{RelPath, VmRegion, transfer};

use super::{
    Listing,
    lab::{Answer, Case, CaseRecord, Expect, Lab, LabSizes, eval_text},
    list_dir, ok, pattern, soft,
};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::Value,
};

const MIB: u32 = 1 << 20;
/// Bytes of each u1 file; 8 directories of 8 files.
const U_FILE: u32 = 0x4000;
const U_N: u32 = 8;

const U_HEAD: &str = "@a\nON ERROR JUMPTO t32p_fail\n";
const U_TAIL: &str = "ON ERROR inherit\n@mark rc0\n@z\nENDDO\nt32p_bad:\nON ERROR inherit\n@mark size\n@z\nENDDO\n\
                      t32p_fail:\nON ERROR inherit\n@mark error\n@z\nENDDO";
const GD: &str = "@a\nPRIVATE &e &r &p\n&e=\"\"\n&r=\"t32p-rel\"\n&p=\"{S}\"\n@ask e1 \"&e\" == \"\"\n\
                  @ask r1 OS.FILE.ABSPATH(\"&r\") == \"&r\"\n@ask p1 OS.FILE.ABSPATH(\"&p\") == \"&p\"\n@z\nENDDO";

fn mkdir_if_missing(rel: &str) -> String {
    format!("IF OS.DIR(\"{{S}}\\{rel}\")==FALSE()\n  MKDIR \"{{S}}\\{rel}\"\n")
}

fn rm_if_present(p: &str) -> String {
    format!("IF OS.FILE.readable(\"{p}\")==TRUE()\n  RM \"{p}\"\n")
}

/// The u1 shape over `d0..d7 x f0..f7` from `data`; `rm_first` puts an `RM` before every write.
pub(crate) fn u1_body(data: u32, rm_first: bool) -> String {
    let mut b = String::from(U_HEAD) + &mkdir_if_missing("u1");
    let rm = |p: &str| if rm_first { rm_if_present(p) } else { String::new() };
    for d in 0..U_N {
        b += &mkdir_if_missing(&format!("u1\\d{d}"));
        for i in 0..U_N {
            let p = format!("{{S}}\\u1\\d{d}\\f{i}.bin");
            let addr = data + (U_N * d + i) * U_FILE;
            b += &rm(&p);
            b += &format!("Data.SAVE.Binary \"{p}\" VM:0x{addr:08X}++0x{:08X}\n", U_FILE - 1);
            b += &format!("IF OS.FILE.SIZE(\"{p}\")!=0x{U_FILE:08X}\n  GOTO t32p_bad\n");
        }
    }
    let (empty, done) = ("{S}\\u1\\empty.txt", "{S}\\u1\\done.bin");
    b += &rm(empty);
    b += &format!("OPEN #1 \"{empty}\" /Create /Binary\nCLOSE #1\n");
    b += &rm(done);
    b += &format!("Data.SAVE.Binary \"{done}\" VM:0x{data:08X}++0x00000000\n");
    b + U_TAIL
}

/// One 8 MiB file from `{L}`.
pub(crate) fn u2_body() -> String {
    let p = "{S}\\u2\\big.bin";
    String::from(U_HEAD)
        + &mkdir_if_missing("u2")
        + &format!(
            "Data.SAVE.Binary \"{p}\" VM:{{L}}++0x007FFFFF\nIF OS.FILE.SIZE(\"{p}\")!=0x00800000\n  GOTO t32p_bad\n"
        )
        + U_TAIL
}

pub(crate) fn gd_case() -> Case {
    Case::new("gd", GD, Expect::HaltAfter("a"), Duration::from_secs(10))
}

pub(crate) fn u1_case(id: &str, data: u32, rm_first: bool, timeout: Duration) -> Case {
    Case::new(id, u1_body(data, rm_first), Expect::HaltAfter("a"), timeout)
}

pub(crate) fn u2_case(timeout: Duration) -> Case {
    Case::new("u2", u2_body(), Expect::HaltAfter("a"), timeout)
}

/// Every file the u-cases write, below `{S}`.
pub(crate) fn u_files() -> Vec<String> {
    let mut v: Vec<String> = (0..U_N).flat_map(|d| (0..U_N).map(move |i| format!("u1/d{d}/f{i}.bin"))).collect();
    v.extend(["u1/empty.txt", "u1/done.bin", "u2/big.bin"].map(String::from));
    v
}

/// Directories the u-cases create, parents first.
pub(crate) fn u_dirs() -> Vec<String> {
    let mut v = vec!["u1".to_string()];
    v.extend((0..U_N).map(|d| format!("u1/d{d}")));
    v.push("u2".into());
    v
}

/// `ABSPATH` identity, how the guard's two conditions evaluate in a script, then the unpack
/// shape: 64 small files, one of 8 MiB, the same 64 again over themselves, and again after
/// removing the `.bak` files the rerun left, with an `RM` before each write.
pub fn unpack_literal(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let sizes = LabSizes { large: Some(8 * MIB), extra: vec![MIB] };
    let (mut lab, extra) = Lab::open(ctx, "unp", sizes)?;
    let result = unpack_cases(ctx, &mut lab, extra[0]);
    let closed = lab.close(ctx);
    result.and(closed)
}

fn secs(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64())
}

fn took(r: &CaseRecord) -> Option<Duration> {
    r.ending.as_ref().map(|(_, at)| *at)
}

fn unpack_cases(ctx: &mut ProbeCtx<'_>, lab: &mut Lab, data: VmRegion) -> ProbeResult {
    let s = lab.dir().text();
    let abs = eval_text(ctx, &format!("OS.FILE.ABSPATH(\"{s}\")"))?;
    ctx.measure("abspath_raw", Value::Text(abs.clone().unwrap_or_else(|| "refused".into())));
    let same = abs.as_deref() == Some(s.as_str());
    if abs.is_some() {
        ctx.learned.abspath_same = Some(same);
    }
    ctx.text("abspath", if same { "same" } else { "differs" });

    let gd = lab.run(ctx, &gd_case())?;
    let guard = gd.answer("e1") == Answer::Yes && gd.answer("r1") == Answer::No && gd.answer("p1") == Answer::Yes;
    ctx.text("guard", ok(guard));

    for d in u_dirs() {
        lab.register(ctx, &d, true)?;
    }
    for f in u_files() {
        lab.register(ctx, &f, false)?;
        for c in RelPath::new(&f)?.bak_candidates() {
            lab.register(ctx, c.as_str(), false)?;
        }
    }
    ctx.session.mem_write(&data.addr(), &pattern(0x55, MIB as usize))?;

    let u1 = lab.run(ctx, &u1_case("u1", data.start(), false, Duration::from_secs(60)))?;
    let t1 = took(&u1).unwrap_or_default();
    ctx.text("u64", secs(t1));
    ctx.text("per_file", format!("{:.1}", t1.as_secs_f64() * 1e3 / f64::from(U_N * U_N)));
    let mut sizes_ok = u1.has("rc0");
    for f in u_files().iter().take((U_N * U_N) as usize) {
        sizes_ok &= size(ctx, lab, f)? == Some(u64::from(U_FILE));
    }
    sizes_ok &= size(ctx, lab, "u1/done.bin")? == Some(1);
    let empty = match size(ctx, lab, "u1/empty.txt")? {
        Some(n) => n.to_string(),
        None => "missing".into(),
    };
    ctx.text("empty", empty);

    let timeout = (t1 * 10).clamp(Duration::from_secs(20), Duration::from_secs(60));
    let big_ok = match lab.l {
        Some(l) => {
            ctx.session.mem_write(&l.addr(), &pattern(u64::from(8 * MIB), (8 * MIB) as usize))?;
            let u2 = lab.run(ctx, &u2_case(timeout))?;
            ctx.text("u8m", took(&u2).map_or("-".into(), secs));
            u2.has("rc0") && size(ctx, lab, "u2/big.bin")? == Some(u64::from(8 * MIB))
        }
        None => {
            ctx.text("u8m", "-");
            true
        }
    };

    lab.run(ctx, &u1_case("u1b", data.start(), false, timeout))?;
    let baks = bak_files(ctx, lab)?;
    ctx.text("rerun_bak", baks.len().to_string());
    ctx.measure("rerun_bak_names", Value::Text(baks.join(",")));
    for b in &baks {
        soft(ctx.session.cmd(&t32stage::practice::cmd::rm(&lab.path(b)?)))?;
    }
    lab.run(ctx, &u1_case("u1r", data.start(), true, timeout))?;
    let left = bak_files(ctx, lab)?;
    ctx.text("rm_bak", left.len().to_string());

    let mut stray = Vec::new();
    for d in u_dirs() {
        stray.extend(lab.sweep(ctx, &d)?.unwrap_or_default());
    }
    ctx.measure("stray_sub", Value::Text(stray.join(",")));
    if !lab.cut.is_empty() {
        return Err(ProbeFail::Check(format!("cut by the probe deadline: {}", lab.cut.join(","))));
    }
    ctx.check(sizes_ok && big_ok && guard, || format!("u1 rc0 and sizes: {sizes_ok}, u2: {big_ok}, guard: {guard}"))
}

/// `OS.FILE.SIZE` of a file below `{S}`; `None` when absent or refused.
fn size(ctx: &mut ProbeCtx<'_>, lab: &Lab, rel: &str) -> Result<Option<u64>, ProbeFail> {
    if !lab.readable(ctx, rel)? {
        return Ok(None);
    }
    soft(transfer::file_size(ctx.session, &lab.path(rel)?))
}

/// `.bak` files below `u1`, from the listings; the readable candidates when listing is refused.
fn bak_files(ctx: &mut ProbeCtx<'_>, lab: &Lab) -> Result<Vec<String>, ProbeFail> {
    let mut found = Vec::new();
    let dirs: Vec<String> = u_dirs().into_iter().filter(|d| d.starts_with("u1")).collect();
    for d in &dirs {
        match list_dir(ctx.session, &lab.path(d)?)? {
            Listing::Names { names, .. } => {
                found.extend(
                    names.iter().filter(|n| n.to_ascii_lowercase().ends_with(".bak")).map(|n| format!("{d}/{n}")),
                );
            }
            Listing::Unavailable(_) => {
                for f in u_files().iter().filter(|f| f.rsplit_once('/').is_some_and(|(p, _)| p == d)) {
                    for c in RelPath::new(f)?.bak_candidates() {
                        if lab.readable(ctx, c.as_str())? {
                            found.push(c.as_str().to_string());
                        }
                    }
                }
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
#[path = "unpack_tests.rs"]
mod tests;
