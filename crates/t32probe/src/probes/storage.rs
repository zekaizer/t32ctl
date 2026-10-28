// SPDX-License-Identifier: Apache-2.0
//! Probes about files and `VM:`: tempdir, vm-rw, vm-file, gzip, tar-download.

use std::time::{Duration, Instant};

use t32rcl::{Session, Tuning};
use t32stage::{
    ExistsFn, RelPath, StagePath, StageRoot, Undo, VmRegion,
    practice::{cmd, fnc},
    transfer,
};

use super::{ok, pattern, scratch_regions, soft, stage_root, workdir};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::{Value, key},
    stats::mb_per_sec,
};

const MIB: u32 = 1 << 20;

/// Create `<root>\probe-<run>-<tag>` and a one-byte file in it, both registered for removal.
/// `Some(file)` when every step held.
fn exercise_root(
    ctx: &mut ProbeCtx<'_>,
    root: &StageRoot,
    tag: &str,
    one: &VmRegion,
) -> Result<Option<StagePath>, ProbeFail> {
    if soft(transfer::ensure_root(ctx.session, root))?.is_none() {
        return Ok(None);
    }
    let dir = root.path(RelPath::new(&format!("probe-{}-{tag}", ctx.env.cfg.run_id))?);
    let file = dir.join("one.bin")?;
    ctx.push(Undo::RemoveDir(dir.clone()))?;
    ctx.push(Undo::RemoveFile(file.clone()))?;
    let made = soft(ctx.session.cmd(&cmd::mkdir(&dir)))?.is_some()
        && soft(ctx.session.eval(&fnc::dir_exists(&dir)).and_then(|v| v.into_bool("OS.DIR")))? == Some(true)
        && soft(ctx.session.cmd(&cmd::data_save_binary(&file, one, false)))?.is_some()
        && soft(transfer::file_size(ctx.session, &file))? == Some(1);
    Ok(made.then_some(file))
}

/// `OS.PTD()`, the stage root (`OS.PTD()\t32ctl` or `--stage-root`), the file-exists function,
/// and the MKDIR/RM/RMDIR semantics staging relies on.
pub fn tempdir(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.session.take_trace();
    let ptd = ctx.session.eval(fnc::PTD)?.into_text(fnc::PTD)?;
    let lossy = ctx.session.take_trace().lossy_text > 0;
    ctx.measure("ptd_text", Value::Text(ptd.clone()));
    let from_ptd = StageRoot::from_ptd(&ptd).ok().filter(|_| !lossy);
    ctx.text("ptd", if from_ptd.is_some() { "ascii" } else { "non-ascii" });
    ctx.text("sep", if ptd.ends_with(['\\', '/']) { "yes" } else { "no" });

    let (chosen, tag) = match (&ctx.env.cfg.stage_root, from_ptd) {
        (Some(p), _) => (StageRoot::new(p.clone()), "override"),
        (None, Some(r)) => (r, "ptd"),
        (None, None) => {
            return Err(ProbeFail::Check("OS.PTD() is not an ASCII-safe path; pass --stage-root".into()));
        }
    };
    ctx.text(key::ROOT, tag);
    let one = scratch_regions(ctx, &[4096])?[0].slice(0, 1).expect("1 byte fits");
    ctx.session.mem_write(&one.addr(), &[0x5A])?;
    let Some(file) = exercise_root(ctx, &chosen, tag, &one)? else {
        return Err(ProbeFail::Check(format!("the {tag} stage root does not work; try --stage-root")));
    };
    let dir_rel = file.rel().parents().last().expect("file sits in a directory").to_string();
    let dir = chosen.path(RelPath::new(&dir_rel)?);
    let missing = dir.join("missing.bin")?;

    let mut working = None;
    for e in ExistsFn::ALL {
        let ask = |ctx: &mut ProbeCtx<'_>, p: &StagePath| {
            soft(ctx.session.eval(&e.call(p)).and_then(|v| v.into_bool(e.name())))
        };
        let (yes, no) = (ask(ctx, &file)?, ask(ctx, &missing)?);
        ctx.measure(&format!("exists_{}", e.name()), Value::Text(format!("{yes:?}/{no:?}")));
        if yes == Some(true) && no == Some(false) && working.is_none() {
            working = Some(e);
        }
    }
    let Some(exists) = working else {
        return Err(ProbeFail::Check("no file-exists function answers correctly".into()));
    };
    ctx.learned.exists = Some(exists);
    ctx.text(key::EXISTS, exists.name());

    let again = soft(ctx.session.cmd(&cmd::mkdir(&dir)))?.is_some();
    ctx.text("mkdir_exists", if again { "ok" } else { "error" });
    let (a, ab) = (dir.join("a")?, dir.join("a/b")?);
    ctx.push(Undo::RemoveDir(a))?;
    ctx.push(Undo::RemoveDir(ab.clone()))?;
    soft(ctx.session.cmd(&cmd::mkdir(&ab)))?;
    let nested = soft(ctx.session.eval(&fnc::dir_exists(&ab)).and_then(|v| v.into_bool("OS.DIR")))? == Some(true);
    ctx.text("mkdir_nested", if nested { "yes" } else { "no" });
    let rm_missing = soft(ctx.session.cmd(&cmd::rm(&missing)))?.is_some();
    ctx.text("rm_missing", if rm_missing { "ok" } else { "error" });

    let spaced = dir.join("with space")?;
    let spaced_file = spaced.join("x y.bin")?;
    ctx.push(Undo::RemoveDir(spaced.clone()))?;
    ctx.push(Undo::RemoveFile(spaced_file.clone()))?;
    let space_ok = soft(ctx.session.cmd(&cmd::mkdir(&spaced)))?.is_some()
        && soft(ctx.session.cmd(&cmd::data_save_binary(&spaced_file, &one, false)))?.is_some()
        && soft(ctx.session.eval(&exists.call(&spaced_file)).and_then(|v| v.into_bool(exists.name())))? == Some(true);
    ctx.text("space_path", ok(space_ok));

    let depth_fns = ["PRACTICE.StackDepth()", "PRACTICE.SD()"];
    let mut answered = Vec::new();
    for f in depth_fns {
        if soft(ctx.session.eval(f))?.is_some() {
            answered.push(f);
        }
    }
    ctx.measure("depth_fn", Value::Text(answered.join(" ")));
    ctx.check(answered.contains(&fnc::STACK_DEPTH), || format!("{} does not answer", fnc::STACK_DEPTH))?;
    ctx.check(space_ok, || "a path with spaces does not work".into())
}

/// RCL memory write and read on `VM:` from 1 B to 1 MiB, throughput, and the chunk ladder.
/// The ladder runs last: a rejected size may kill the connection, which the executor revives.
pub fn vm_rw(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let region = scratch_regions(ctx, &[MIB + 4096])?[0];
    for size in [1u32, 2, 3, 7, 8, 255, 1024, 1025, 4096, 65536, MIB] {
        let offsets: &[u64] = if size < 65536 { &[0, 1] } else { &[0] };
        for &off in offsets {
            let at = region.addr().offset(off).expect("inside the region");
            let data = pattern(u64::from(size) + off, size as usize);
            let t = Instant::now();
            ctx.session.mem_write(&at, &data)?;
            let w = t.elapsed();
            let t = Instant::now();
            let back = ctx.session.mem_read(&at, data.len())?;
            let r = t.elapsed();
            ctx.check(back == data, || format!("{size} bytes at offset {off} read back different"))?;
            if size == MIB {
                ctx.headline("max", Value::Bytes(size.into()));
                if let Some(v) = mb_per_sec(size.into(), w) {
                    ctx.headline("w", Value::MbPerSec(v));
                }
                if let Some(v) = mb_per_sec(size.into(), r) {
                    ctx.headline("r", Value::MbPerSec(v));
                }
            }
        }
    }

    let ladder = chunk_ladder(ctx.session, &region)?;
    if let Some(e) = &ladder.error {
        ctx.measure("chunk_error", Value::Text(e.clone()));
    }
    if let Some((r, w)) = ladder.best {
        ctx.measure(key::CHUNK_R, Value::Count(r as u64));
        ctx.measure(key::CHUNK_W, Value::Count(w as u64));
    }
    if let Some(t) = ladder.token() {
        ctx.text("chunk", t);
    }
    if let Some(s) = ladder.stop {
        ctx.text("chunk_stop", s);
    }
    Ok(())
}

/// Read and write chunk sizes the ladder tries, smallest first; the last is the client's maximum.
pub(crate) const LADDER: [(usize, usize); 5] =
    [(1008, 1024), (2040, 2048), (4088, 4096), (8184, 8192), (Tuning::MAX_CHUNK, Tuning::MAX_CHUNK)];

/// What the chunk ladder found.
#[derive(Debug, PartialEq)]
pub(crate) struct Ladder {
    /// Largest read and write chunk that round-tripped 256 KiB.
    pub best: Option<(usize, usize)>,
    /// Why it stopped before the end: `mismatch` or an error tag. `None`: every rung passed.
    pub stop: Option<String>,
    pub error: Option<String>,
}

impl Ladder {
    /// `r<read>/w<write>`, marked `(client-limit)` when no rung failed: the ladder ended at the
    /// client's own maximum, not at a PowerView refusal.
    pub fn token(&self) -> Option<String> {
        let (r, w) = self.best?;
        let limit = if self.stop.is_none() { "(client-limit)" } else { "" };
        Some(format!("r{r}/w{w}{limit}"))
    }
}

/// 256 KiB written and read back at each rung of `LADDER`, until one fails.
pub(crate) fn chunk_ladder(session: &mut Session, region: &VmRegion) -> Result<Ladder, ProbeFail> {
    let spelling = session.tuning().macro_spelling();
    let mut ladder = Ladder { best: None, stop: None, error: None };
    for (r, w) in LADDER {
        session.set_tuning(Tuning::new(r, w)?.with_macro_spelling(spelling));
        let data = pattern(r as u64, 256 * 1024);
        let result =
            session.mem_write(&region.addr(), &data).and_then(|()| session.mem_read(&region.addr(), data.len()));
        match result {
            Ok(back) if back == data => ladder.best = Some((r, w)),
            Ok(_) => {
                ladder.stop = Some("mismatch".to_string());
                break;
            }
            Err(e) => {
                ladder.error = Some(e.to_string());
                ladder.stop = Some(e.tag().to_string());
                break;
            }
        }
    }
    Ok(ladder)
}

/// `VM:` to file and back with `Data.SAVE.Binary` / `Data.LOAD.Binary`, `++` slices, odd starts,
/// the `--` end form and `/SKIP`.
pub fn vm_file(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let regions = scratch_regions(ctx, &[8192, 8192, 8192])?;
    let (r0, r1, r2) = (regions[0], regions[1], regions[2]);
    let data = pattern(4097, 4097);
    ctx.session.mem_write(&r0.addr(), &data)?;
    let save = |ctx: &mut ProbeCtx<'_>, name: &str, from: &VmRegion| -> Result<(StagePath, Option<u64>), ProbeFail> {
        let f = ctx.staged_file(name)?;
        soft(ctx.session.cmd(&cmd::data_save_binary(&f, from, false)))?;
        let size = soft(transfer::file_size(ctx.session, &f))?;
        Ok((f, size))
    };
    let load = |ctx: &mut ProbeCtx<'_>,
                f: &StagePath,
                to: &VmRegion,
                len: usize,
                extra: &str|
     -> Result<Option<Vec<u8>>, ProbeFail> {
        let line = format!("{}{extra}", cmd::data_load_binary(f, to.start(), false));
        if soft(ctx.session.cmd(&line))?.is_none() {
            return Ok(None);
        }
        Ok(Some(ctx.session.mem_read(&to.addr(), len)?))
    };

    let whole = r0.slice(0, 4097).expect("fits");
    let (f, size) = save(ctx, "vmfile.bin", &whole)?;
    let roundtrip = size == Some(4097) && load(ctx, &f, &r1, 4097, "")?.as_deref() == Some(&data[..]);
    ctx.text("roundtrip", ok(roundtrip));

    let (g, size) = save(ctx, "slice.bin", &r0.slice(0x10, 0x80).expect("fits"))?;
    let slice = size == Some(0x80) && load(ctx, &g, &r2, 0x80, "")?.as_deref() == Some(&data[0x10..0x90]);
    ctx.text("slice", ok(slice));
    let (_, one) = save(ctx, "one.bin", &r0.slice(5, 1).expect("fits"))?;
    let (_, odd) = save(ctx, "odd.bin", &r0.slice(1, 4095).expect("fits"))?;
    ctx.text("small_odd", ok(one == Some(1) && odd == Some(4095)));

    let end_file = ctx.staged_file("endform.bin")?;
    let end_line =
        format!("Data.SAVE.Binary {} VM:0x{:08X}--0x{:08X}", end_file.quoted(), r0.start(), r0.start() + 0xFF);
    let end_form = soft(ctx.session.cmd(&end_line))?.is_some()
        && soft(transfer::file_size(ctx.session, &end_file))? == Some(0x100);
    ctx.text("end_form", ok(end_form));
    let skip = load(ctx, &g, &r2, 0x70, " /SKIP 0x10")?.as_deref() == Some(&data[0x20..0x90]);
    ctx.text("skip", ok(skip));
    ctx.check(roundtrip && slice, || "VM: <-> file round trip or ++ slicing failed".into())
}

/// gzip upload and `/UNZIP`: two payload kinds, four corruptions that must be refused, and the
/// largest decompressed size up to `--gzip-max-mib`, each step timed under a data-sized budget.
pub fn gzip(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let max = ctx.env.cfg.gzip_max.max(MIB);
    let regions = scratch_regions(ctx, &[MIB + 4096, max])?;
    let (a, b) = (regions[0], regions[1]);

    let text: Vec<u8> = b"t32probe gzip text payload 0123456789\n".iter().cycle().take(MIB as usize).copied().collect();
    for (name, data) in [("text", text.clone()), ("random", pattern(77, MIB as usize))] {
        let gz = transfer::gzip(&data);
        let f = ctx.staged_file(&format!("{name}.bin.gz"))?;
        let t = Instant::now();
        transfer::put_file(ctx.session, &a, &gz, &f)?;
        let up = t.elapsed();
        let t = Instant::now();
        ctx.session.cmd(&cmd::data_load_binary(&f, b.start(), true))?;
        let unzip = t.elapsed();
        let back = ctx.session.mem_read(&b.addr(), data.len())?;
        ctx.check(back == data, || format!("{name}: /UNZIP produced different bytes"))?;
        ctx.measure(&format!("{name}_gz"), Value::Bytes(gz.len() as u64));
        ctx.measure(&format!("{name}_unzip"), Value::Seconds(unzip.as_secs_f64()));
        if name == "random"
            && let Some(v) = mb_per_sec(gz.len() as u64, up)
        {
            ctx.headline("up", Value::MbPerSec(v));
        }
    }

    let good = transfer::gzip(&text[..64 * 1024]);
    let n = good.len();
    let mut cases: Vec<(&str, Vec<u8>)> = vec![("truncated", good[..n / 2].to_vec())];
    for (name, at) in [("payload", n / 2), ("crc", n - 8), ("magic", 0)] {
        let mut bad = good.clone();
        bad[at] ^= 0xFF;
        cases.push((name, bad));
    }
    let mut accepted = Vec::new();
    for (name, bad) in &cases {
        let f = ctx.staged_file(&format!("corrupt-{name}.gz"))?;
        transfer::put_file(ctx.session, &a, bad, &f)?;
        let taken = unzip_accepted(ctx.session, &f, b.start())?;
        ctx.measure(&format!("corrupt_{name}"), Value::Text((if taken { "accepted" } else { "rejected" }).into()));
        if taken {
            accepted.push(*name);
        }
    }
    ctx.text("corrupt", corrupt_token(&accepted, cases.len()));

    let mut ceiling = 0u32;
    let mut size = MIB;
    let mut stop = "window";
    while size <= max {
        let gz = transfer::gzip(&vec![0u8; size as usize]);
        let f = ctx.staged_file(&format!("zeros-{}m.gz", size / MIB))?;
        transfer::put_file(ctx.session, &a, &gz, &f)?;
        let tail = b.slice(size - 16, 16).expect("inside region B");
        ctx.session.mem_write(&tail.addr(), &[0xA5; 16])?;
        let (taken, took) = unzip_timed(ctx.session, &f, b.start(), size)?;
        ctx.measure(&format!("zeros_{}m_unzip", size / MIB), Value::Seconds(took.as_secs_f64()));
        if !taken {
            stop = "refused";
            break;
        }
        if ctx.session.mem_read(&tail.addr(), 16)? != [0u8; 16] {
            stop = "mismatch";
            break;
        }
        ceiling = size;
        size *= 2;
    }
    ctx.headline("unzip_max", Value::Bytes(ceiling.into()));
    ctx.text("unzip_stop", stop);
    ctx.check(accepted.is_empty(), || format!("/UNZIP accepted corrupt gzip: {}", accepted.join(", ")))
}

/// `Data.LOAD.Binary <f> VM:<at> /UNZIP`: `true` when PowerView took it, `false` when it refused.
pub(crate) fn unzip_accepted(session: &mut Session, f: &StagePath, at: u32) -> Result<bool, ProbeFail> {
    Ok(soft(session.cmd(&cmd::data_load_binary(f, at, true)))?.is_some())
}

/// `/UNZIP` of a file that inflates to `len` bytes under `data_budget(len)`: whether PowerView
/// took it, and how long it took on the session clock.
pub(crate) fn unzip_timed(
    session: &mut Session,
    f: &StagePath,
    at: u32,
    len: u32,
) -> Result<(bool, Duration), ProbeFail> {
    let budget = transfer::data_budget(session, len.into());
    let t = session.now();
    let taken = soft(session.with_budget(budget).cmd(&cmd::data_load_binary(f, at, true)))?.is_some();
    Ok((taken, session.now().saturating_duration_since(t)))
}

/// `rejected(n/n)` when every corrupt stream was refused, else `ACCEPTED(<names>)`.
pub(crate) fn corrupt_token(accepted: &[&str], cases: usize) -> String {
    if accepted.is_empty() { format!("rejected({cases}/{cases})") } else { format!("ACCEPTED({})", accepted.join(",")) }
}

/// PowerView packs a work directory with `TAR` and `ZIP`; the client downloads both through `VM:`,
/// untars and gunzips them. No `Stage`: its `GLOBAL` macros are not part of what is measured.
/// TAR and ZIP never run twice on one name: an overwrite may ask in a dialog.
pub fn tar_download(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let root = stage_root(ctx)?;
    let dir = workdir(ctx, &root, "tar")?;
    let regions = scratch_regions(ctx, &[256 * 1024, 256 * 1024])?;
    let (up, down) = (regions[0], regions[1]);
    let o = dir.join("o")?;
    for d in ["o", "o/sub", "o/s d"] {
        mkdir_checked(ctx, &dir.join(d)?)?;
    }
    let files: [(&str, Vec<u8>); 4] = [
        ("a.txt", vec![b'A']),
        ("b.bin", pattern(1000, 1000)),
        ("sub/c.bin", pattern(70000, 70000)),
        ("s d/e.bin", pattern(10, 10)),
    ];
    for (name, bytes) in &files {
        let p = o.join(name)?;
        ctx.push(Undo::RemoveFile(p.clone()))?;
        transfer::put_file(ctx.session, &up, bytes, &p)?;
    }
    let archive = dir.join("o.tar")?;
    ctx.push(Undo::RemoveFile(archive.clone()))?;
    ctx.session.cmd(&cmd::tar(&archive, &o))?;
    let tar_len = transfer::file_size(ctx.session, &archive)?;
    ctx.headline("tar", Value::Bytes(tar_len));
    ctx.check(tar_len <= u64::from(down.len()), || format!("the archive of {tar_len} bytes exceeds 256 KiB"))?;
    let bytes = transfer::get_file(ctx.session, &archive, &down, tar_len as u32)?;
    let diag = t32stage::diagnose_tar(&bytes);
    let entries = t32stage::walk_tar(&bytes)?;
    ctx.measure("first_header", Value::Text(diag.first_header_hex.clone()));
    ctx.measure("first_name", Value::Text(diag.first_name_raw.clone()));
    ctx.headline("files", Value::Count(entries.len() as u64));
    ctx.text("flavor", if diag.ustar_magic { "ustar" } else { "other" });
    ctx.text("sep", if diag.backslash { "\\" } else { "/" });
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    ctx.measure("names", Value::Text(names.join(" ")));
    let holds = |name: &str, bytes: &[u8]| entries.iter().any(|e| e.name.ends_with(name) && e.data == bytes);
    let matched = files.iter().all(|(name, bytes)| holds(name, bytes));
    let relative = files.iter().all(|(name, _)| names.contains(name));
    ctx.text("names", if relative { "relative" } else { "prefixed" });
    ctx.text("roundtrip", ok(matched));
    ctx.text("space_dir", ok(holds(files[3].0, &files[3].1)));

    let gz = dir.join("o.tar.gz")?;
    ctx.push(Undo::RemoveFile(gz.clone()))?;
    let zip = match soft(ctx.session.cmd(&cmd::zip(&archive, &gz)))? {
        None => "refused",
        Some(()) => match soft(transfer::file_size(ctx.session, &gz))? {
            Some(n) if n <= u64::from(down.len()) => {
                let raw = transfer::get_file(ctx.session, &gz, &down, n as u32)?;
                ctx.measure("zip_header", Value::Text(super::hex(&raw[..raw.len().min(10)])));
                let same = transfer::gunzip(&raw).is_ok_and(|t| t == bytes);
                if raw.starts_with(&[0x1F, 0x8B, 0x08]) && same { "gzip" } else { "other" }
            }
            _ => "other",
        },
    };
    ctx.text("zip", zip);
    ctx.check(matched, || format!("archive holds {names:?}, contents or names differ"))
}

/// `MKDIR` after its removal was registered; `Check` when `OS.DIR` does not see it.
fn mkdir_checked(ctx: &mut ProbeCtx<'_>, p: &StagePath) -> ProbeResult {
    ctx.push(Undo::RemoveDir(p.clone()))?;
    ctx.session.cmd(&cmd::mkdir(p))?;
    let expr = fnc::dir_exists(p);
    let made = ctx.session.eval(&expr)?.into_bool(&expr)?;
    ctx.check(made, || format!("MKDIR did not create {p}"))
}
