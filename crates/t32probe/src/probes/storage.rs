// SPDX-License-Identifier: Apache-2.0
//! Probes about files and `VM:`: tempdir, vm-rw, vm-file, gzip, tar-download.

use std::time::Instant;

use t32rcl::Tuning;
use t32stage::{
    ExistsFn, Put, PvPath, RelPath, StagePath, StageRoot, Undo, VmRegion,
    practice::{cmd, fnc},
    transfer,
};

use super::{ok, pattern, scratch_regions, soft};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::Value,
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

/// `OS.PTD()`, both stage-root modes, the file-exists function, and the MKDIR/RM/RMDIR
/// semantics staging relies on.
pub fn tempdir(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.session.take_trace();
    let ptd = ctx.session.eval(fnc::PTD)?.into_text(fnc::PTD)?;
    let lossy = ctx.session.take_trace().lossy_text > 0;
    ctx.measure("ptd_text", Value::Text(ptd.clone()));
    let ascii = !lossy && PvPath::parse(&ptd).is_ok();
    ctx.text("ptd", if ascii { "ascii" } else { "non-ascii" });
    ctx.text("sep", if ptd.ends_with(['\\', '/']) { "yes" } else { "no" });

    let one = scratch_regions(ctx, &[4096])?[0].slice(0, 1).expect("1 byte fits");
    ctx.session.mem_write(&one.addr(), &[0x5A])?;
    let literal = if ascii { exercise_root(ctx, &StageRoot::from_ptd(&ptd), "lit", &one)? } else { None };
    let bound =
        cmd::bind_root().iter().try_fold(true, |acc, c| soft(ctx.session.cmd(c)).map(|r| acc && r.is_some()))?;
    let root_text = if bound {
        soft(ctx.session.macro_get(&t32stage::Macro::Root.name()))?.unwrap_or_default()
    } else {
        String::new()
    };
    ctx.measure("root_macro", Value::Text(root_text));
    let macro_mode = if bound { exercise_root(ctx, &StageRoot::Macro, "mac", &one)? } else { None };
    ctx.text("literal", if ascii { ok(literal.is_some()) } else { "n/a" });
    ctx.text("macro", ok(macro_mode.is_some()));

    let chosen = match &ctx.env.cfg.stage_root {
        Some(p) => StageRoot::Literal(p.clone()),
        None => StageRoot::from_ptd(&ptd),
    };
    ctx.text("root", chosen.tag());
    let file = match (&chosen, literal, macro_mode) {
        (StageRoot::Macro, _, Some(f)) => f,
        (StageRoot::Literal(_), Some(f), _) if ctx.env.cfg.stage_root.is_none() => f,
        (StageRoot::Literal(_), _, _) if ctx.env.cfg.stage_root.is_some() => {
            match exercise_root(ctx, &chosen, "override", &one)? {
                Some(f) => f,
                None => return Err(ProbeFail::Check("--stage-root is not usable".into())),
            }
        }
        _ => {
            return Err(ProbeFail::Check(format!("{} stage root does not work; try --stage-root", chosen.tag())));
        }
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
    ctx.text("exists", exists.name());

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

    let base = ctx.session.tuning();
    let ladder = [(1008, 1024), (2040, 2048), (4088, 4096), (8184, 8192), (Tuning::MAX_CHUNK, Tuning::MAX_CHUNK)];
    let mut best = None;
    let mut stop = None;
    for (r, w) in ladder {
        ctx.session.set_tuning(Tuning::new(r, w)?.with_macro_spelling(base.macro_spelling()));
        let data = pattern(r as u64, 256 * 1024);
        let result = ctx
            .session
            .mem_write(&region.addr(), &data)
            .and_then(|()| ctx.session.mem_read(&region.addr(), data.len()));
        match result {
            Ok(back) if back == data => best = Some((r, w)),
            Ok(_) => {
                stop = Some("mismatch".to_string());
                break;
            }
            Err(e) => {
                ctx.measure("chunk_error", Value::Text(e.to_string()));
                stop = Some(e.tag().to_string());
                break;
            }
        }
    }
    if let Some((r, w)) = best {
        ctx.measure("chunk_r", Value::Count(r as u64));
        ctx.measure("chunk_w", Value::Count(w as u64));
        let limit = if stop.is_none() { "(client-limit)" } else { "" };
        ctx.text("chunk", format!("r{r}/w{w}{limit}"));
    }
    if let Some(s) = stop {
        ctx.text("chunk_stop", s);
    }
    Ok(())
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
/// largest decompressed size up to `--gzip-max-mib`.
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
        let refused = soft(ctx.session.cmd(&cmd::data_load_binary(&f, b.start(), true)))?.is_none();
        ctx.measure(&format!("corrupt_{name}"), Value::Text((if refused { "rejected" } else { "accepted" }).into()));
        if !refused {
            accepted.push(*name);
        }
    }
    let corrupt = if accepted.is_empty() {
        format!("rejected({}/{})", cases.len(), cases.len())
    } else {
        format!("ACCEPTED({})", accepted.join(","))
    };
    ctx.text("corrupt", corrupt);

    let mut ceiling = 0u32;
    let mut size = MIB;
    while size <= max {
        let gz = transfer::gzip(&vec![0u8; size as usize]);
        let f = ctx.staged_file(&format!("zeros-{}m.gz", size / MIB))?;
        transfer::put_file(ctx.session, &a, &gz, &f)?;
        let tail = b.slice(size - 16, 16).expect("inside region B");
        ctx.session.mem_write(&tail.addr(), &[0xA5; 16])?;
        let loaded = soft(ctx.session.cmd(&cmd::data_load_binary(&f, b.start(), true)))?.is_some();
        if !loaded || ctx.session.mem_read(&tail.addr(), 16)? != [0u8; 16] {
            break;
        }
        ceiling = size;
        size *= 2;
    }
    ctx.headline("unzip_max", Value::Bytes(ceiling.into()));
    ctx.check(accepted.is_empty(), || format!("/UNZIP accepted corrupt gzip: {}", accepted.join(", ")))
}

/// PowerView packs a directory with `TAR` and `ZIP`; the client downloads and untars it.
pub fn tar_download(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let files: [(&str, Vec<u8>); 3] =
        [("a.txt", vec![b'A']), ("b.bin", pattern(1000, 1000)), ("sub/c.bin", pattern(70000, 70000))];
    let (stage, session) = ctx.stage()?;
    let dir = stage.path("o")?;
    let sub = dir.join("sub")?;
    let mut created = Vec::new();
    transfer::ensure_dir(session, &sub, |p| {
        created.push(Undo::RemoveDir(p.clone()));
        Ok(())
    })?;
    for undo in created {
        stage.register(undo)?;
    }
    for (name, bytes) in &files {
        stage.upload(session, Put::Plain, bytes, &format!("o/{name}"))?;
    }
    let archive = stage.path("o.tar")?;
    stage.register(Undo::RemoveFile(archive.clone()))?;
    session.cmd(&cmd::tar(&archive, &dir))?;
    let tar_len = transfer::file_size(session, &archive)?;
    let bytes = stage.download(session, &archive)?;
    let diag = t32stage::diagnose_tar(&bytes);
    let entries = t32stage::walk_tar(&bytes)?;
    ctx.measure("first_header", Value::Text(diag.first_header_hex.clone()));
    ctx.measure("first_name", Value::Text(diag.first_name_raw.clone()));
    ctx.headline("files", Value::Count(entries.len() as u64));
    ctx.headline("tar", Value::Bytes(tar_len));
    ctx.text("flavor", if diag.ustar_magic { "ustar" } else { "other" });
    ctx.text("sep", if diag.backslash { "\\" } else { "/" });
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    ctx.measure("names", Value::Text(names.join(" ")));
    let matched = files.iter().all(|(name, bytes)| entries.iter().any(|e| e.name.ends_with(name) && &e.data == bytes));
    let relative = files.iter().all(|(name, _)| names.contains(name));
    ctx.text("names", if relative { "relative" } else { "prefixed" });
    ctx.text("roundtrip", ok(matched));
    ctx.check(matched, || format!("archive holds {names:?}, contents or names differ"))
}
