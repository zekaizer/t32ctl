// SPDX-License-Identifier: Apache-2.0
//! Probes about files PowerView writes: fs-semantics, vm-large.

use std::collections::BTreeSet;

use t32rcl::RemoteError;
use t32stage::{
    RelPath, StagePath, Undo, VmRegion,
    practice::{cmd, fnc},
    transfer,
};

use super::{
    Listing,
    functions::{command, func, text_of},
    list_dir, pattern, scratch_regions, soft, stage_root, workdir,
};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::Value,
};

const KIB: u32 = 1024;

/// `err<status>/<kind>` of a refusal, `ok` otherwise.
fn status<T>(r: &Result<T, RemoteError>) -> String {
    r.as_ref().map_or_else(|e| format!("err{}/{}", e.code, e.kind.to_string().replace(' ', "-")), |_| "ok".into())
}

/// The fs-semantics work directory: every name below it whose removal is registered.
struct Fs {
    dir: StagePath,
    data: VmRegion,
    back: VmRegion,
    known: BTreeSet<String>,
}

impl Fs {
    fn path(&self, rel: &str) -> Result<StagePath, ProbeFail> {
        Ok(self.dir.join(rel)?)
    }

    /// Register the removal of `rel` and of both `.bak` candidates before anything writes it.
    fn file(&mut self, ctx: &mut ProbeCtx<'_>, rel: &str) -> Result<StagePath, ProbeFail> {
        let p = self.path(rel)?;
        ctx.push(Undo::RemoveFile(p.clone()))?;
        self.known.insert(rel.to_ascii_lowercase());
        for c in RelPath::new(rel)?.bak_candidates() {
            ctx.push(Undo::RemoveFile(self.path(c.as_str())?))?;
            self.known.insert(c.as_str().to_ascii_lowercase());
        }
        Ok(p)
    }

    /// `MKDIR` after its removal was registered; `true` when `OS.DIR` sees it.
    fn mkdir(&mut self, ctx: &mut ProbeCtx<'_>, rel: &str) -> Result<bool, ProbeFail> {
        let p = self.path(rel)?;
        ctx.push(Undo::RemoveDir(p.clone()))?;
        self.known.insert(rel.to_ascii_lowercase());
        soft(ctx.session.cmd(&cmd::mkdir(&p)))?;
        self.dir_exists(ctx, &p)
    }

    fn dir_exists(&self, ctx: &mut ProbeCtx<'_>, p: &StagePath) -> Result<bool, ProbeFail> {
        Ok(soft(ctx.session.eval(&fnc::dir_exists(p)).and_then(|v| v.into_bool("OS.DIR")))? == Some(true))
    }

    fn readable(&self, ctx: &mut ProbeCtx<'_>, rel: &str) -> Result<bool, ProbeFail> {
        let (p, e) = (self.path(rel)?, ctx.exists_fn());
        Ok(soft(ctx.session.eval(&e.call(&p)).and_then(|v| v.into_bool(e.name())))? == Some(true))
    }

    fn put(&self, ctx: &mut ProbeCtx<'_>, p: &StagePath, bytes: &[u8]) -> Result<bool, ProbeFail> {
        Ok(soft(transfer::put_file(ctx.session, &self.data, bytes, p))?.is_some())
    }

    /// The file's bytes through the second region; `None` when absent, too large or refused.
    fn fetch(&self, ctx: &mut ProbeCtx<'_>, rel: &str) -> Result<Option<Vec<u8>>, ProbeFail> {
        let p = self.path(rel)?;
        match soft(transfer::file_size(ctx.session, &p))? {
            Some(n) if n <= u64::from(self.back.len()) => {
                Ok(soft(transfer::get_file(ctx.session, &p, &self.back, n as u32))?)
            }
            _ => Ok(None),
        }
    }

    fn rm(&self, ctx: &mut ProbeCtx<'_>, rel: &str) -> Result<bool, ProbeFail> {
        Ok(soft(ctx.session.cmd(&cmd::rm(&self.path(rel)?)))?.is_some())
    }

    /// `D` listed, the raw names recorded as `list.<label>`; `None` when listing is refused.
    fn listing(&self, ctx: &mut ProbeCtx<'_>, label: &str) -> Result<Option<Vec<String>>, ProbeFail> {
        match list_dir(ctx.session, &self.dir)? {
            Listing::Names { names, .. } => {
                ctx.measure(&format!("list.{label}"), Value::Text(names.join(",")));
                Ok(Some(names))
            }
            Listing::Unavailable(_) => Ok(None),
        }
    }

    /// `rel` saved with `first`, then overwritten with `second`. The `.bak` names that exist
    /// afterwards: candidates that are readable, then names the listing gained, lowercase.
    fn twice(
        &mut self,
        ctx: &mut ProbeCtx<'_>,
        rel: &str,
        first: &[u8],
        second: &[u8],
    ) -> Result<Vec<String>, ProbeFail> {
        let p = self.file(ctx, rel)?;
        self.put(ctx, &p, first)?;
        let before = self.listing(ctx, &format!("{rel}.1"))?;
        self.put(ctx, &p, second)?;
        let after = self.listing(ctx, &format!("{rel}.2"))?;
        let mut baks = Vec::new();
        for c in RelPath::new(rel)?.bak_candidates() {
            if self.readable(ctx, c.as_str())? {
                baks.push(c.as_str().to_ascii_lowercase());
            }
        }
        if let (Some(b), Some(a)) = (before, after) {
            let b: BTreeSet<String> = b.iter().map(|n| n.to_ascii_lowercase()).collect();
            for n in a.iter().map(|n| n.to_ascii_lowercase()) {
                if !b.contains(&n) && n != rel && !baks.contains(&n) {
                    baks.push(n);
                }
            }
        }
        ctx.measure(&format!("bak.{rel}"), Value::Text(baks.join(",")));
        Ok(baks)
    }
}

/// `.bak` naming on overwrite, removing first, `/Append`, `RMDIR` of a full directory,
/// idempotent removal, punctuation in names, the `OS.FIRSTFILE` listing and `ABSPATH` identity.
/// Records only; passes once every step ran.
pub fn fs_semantics(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let root = stage_root(ctx)?;
    let dir = workdir(ctx, &root, "fs")?;
    let regions = scratch_regions(ctx, &[64 * KIB, 64 * KIB])?;
    let mut fs = Fs { dir, data: regions[0], back: regions[1], known: BTreeSet::new() };
    overwrite(ctx, &mut fs)?;
    append(ctx, &mut fs)?;
    rmdir(ctx, &mut fs)?;
    punctuation(ctx, &mut fs)?;
    listing(ctx, &mut fs)?;
    let text = fs.dir.text();
    let abs = func(ctx, &format!("OS.FILE.ABSPATH(\"{text}\")"))?.ok().map(|v| text_of(&v));
    ctx.measure("abspath_raw", Value::Text(abs.clone().unwrap_or_else(|| "refused".into())));
    ctx.text("abspath", if abs.as_deref() == Some(text.as_str()) { "same" } else { "differs" });
    strays(ctx, &fs)
}

fn first_or_dash(v: &[String]) -> String {
    v.first().cloned().unwrap_or_else(|| "-".into())
}

/// Steps 1, 2, 4 and 5: overwrite, remove first, names without and with several dots, and two
/// names that share a stem.
fn overwrite(ctx: &mut ProbeCtx<'_>, fs: &mut Fs) -> ProbeResult {
    let (one, two) = (pattern(1, 100), pattern(2, 100));
    let x = fs.twice(ctx, "x.bin", &one, &two)?;
    ctx.text("bak", if x.is_empty() { "no" } else { "yes" });
    if let Some(name) = x.first() {
        let got = fs.fetch(ctx, name)?;
        let content = match got {
            Some(b) if b == one => "first",
            Some(b) if b == two => "second",
            Some(_) => "other",
            None => "unreadable",
        };
        ctx.measure("bak_content", Value::Text(content.into()));
    }

    for name in x.iter().map(String::as_str).chain(["x.bin"]) {
        fs.rm(ctx, name)?;
    }
    let p = fs.path("x.bin")?;
    fs.put(ctx, &p, &one)?;
    let mut again = Vec::new();
    for c in RelPath::new("x.bin")?.bak_candidates().iter().map(|c| c.as_str().to_string()).chain(x.iter().cloned()) {
        if !again.contains(&c) && fs.readable(ctx, &c)? {
            again.push(c);
        }
    }
    ctx.measure("rm_first_left", Value::Text(again.join(",")));
    ctx.text("rm_first", if again.is_empty() { "nobak" } else { "bak" });

    let noext = fs.twice(ctx, "complete", &one, &two)?;
    let dotted = fs.twice(ctx, "a.b.c", &one, &two)?;
    ctx.text(
        "bak_name",
        format!("x:{},noext:{},dotted:{}", first_or_dash(&x), first_or_dash(&noext), first_or_dash(&dotted)),
    );

    let (three, four) = (pattern(3, 100), pattern(4, 100));
    fs.twice(ctx, "k.cmm", &one, &two)?;
    fs.twice(ctx, "k.bin", &three, &four)?;
    let mut k = Vec::new();
    for c in ["k.bak", "k.cmm.bak", "k.bin.bak"] {
        if fs.readable(ctx, c)? {
            k.push(c);
        }
    }
    let collide = match k.len() {
        0 => "none",
        1 => "one",
        _ => "two",
    };
    ctx.text("bak_collide", collide);
    if k.contains(&"k.bak") {
        let content = match fs.fetch(ctx, "k.bak")? {
            Some(b) if b == three => "bin",
            Some(b) if b == one => "cmm",
            Some(_) => "other",
            None => "unreadable",
        };
        ctx.measure("bak_k_content", Value::Text(content.into()));
    }
    Ok(())
}

/// Step 3: `/Append` onto a file, then `/Append /ZIP` twice onto a new one.
fn append(ctx: &mut ProbeCtx<'_>, fs: &mut Fs) -> ProbeResult {
    let (a, b) = (pattern(5, 15360), pattern(6, 4097));
    let at_b = fs.data.start() + 0x4000;
    ctx.session.mem_write(&fs.data.addr(), &a)?;
    ctx.session.mem_write(&t32rcl::MemAddr::vm(u64::from(at_b)), &b)?;
    let ap = fs.file(ctx, "ap.bin")?;
    let base = format!("Data.SAVE.Binary {} VM:0x{:08X}++0x00003BFF", ap.quoted(), fs.data.start());
    let first = soft(ctx.session.cmd(&base))?.is_some();
    let before = fs.listing(ctx, "ap.1")?;
    let line = format!("Data.SAVE.Binary {} VM:0x{at_b:08X}++0x00001000 /Append", ap.quoted());
    let appended = soft(ctx.session.cmd(&line))?.is_some();
    let after = fs.listing(ctx, "ap.2")?;
    let token = if !(first && appended) {
        "refused"
    } else if soft(transfer::file_size(ctx.session, &ap))? != Some(19457) {
        "size"
    } else if fs.fetch(ctx, "ap.bin")?.is_some_and(|got| got[..15360] == a[..] && got[15360..] == b[..]) {
        "ok"
    } else {
        "data"
    };
    ctx.text("append", token);
    let gained = match (before, after) {
        (Some(b), Some(a)) => a.iter().filter(|n| !b.contains(n)).count(),
        _ => 0,
    };
    ctx.text("append_bak", if gained == 0 { "no".to_string() } else { gained.to_string() });

    let ap2 = fs.file(ctx, "ap2.bin")?;
    let zip = format!("Data.SAVE.Binary {} VM:0x{:08X}++0x00003BFF /Append /ZIP", ap2.quoted(), fs.data.start());
    let mut zipped = true;
    for n in 1..=2 {
        let r = command(ctx, &zip)?;
        ctx.measure(&format!("append_zip.{n}"), Value::Text(status(&r)));
        zipped &= r.is_ok();
    }
    ctx.text("append_zip", if zipped { "ok" } else { "refused" });
    Ok(())
}

/// Step 6: `RMDIR` of a directory that still holds a file, twice; then removing what is not there.
fn rmdir(ctx: &mut ProbeCtx<'_>, fs: &mut Fs) -> ProbeResult {
    fs.mkdir(ctx, "nd")?;
    let f = fs.file(ctx, "nd/f.bin")?;
    fs.put(ctx, &f, &pattern(7, 16))?;
    let nd = fs.path("nd")?;
    let mut first = None;
    for n in 1..=2 {
        let r = command(ctx, &cmd::rmdir(&nd))?;
        let msg = r.as_ref().err().and_then(|e| e.message.clone()).unwrap_or_default();
        ctx.measure(&format!("rmdir_full.{n}"), Value::Text(format!("{} {msg:?}", status(&r))));
        first.get_or_insert(status(&r));
    }
    ctx.text("rmdir_full", first.unwrap_or_default());
    fs.rm(ctx, "nd/f.bin")?;
    soft(ctx.session.cmd(&cmd::rmdir(&nd)))?;
    let gone = !fs.dir_exists(ctx, &nd)?;
    ctx.measure("rmdir_empty", Value::Text((if gone { "ok" } else { "left" }).into()));
    let rm = command(ctx, &cmd::rm(&fs.path("gone.bin")?))?;
    let rd = command(ctx, &cmd::rmdir(&fs.path("gone")?))?;
    let word = |ok: bool| if ok { "ok" } else { "err" };
    ctx.text("idem", format!("rm:{},rmdir:{}", word(rm.is_ok()), word(rd.is_ok())));
    ctx.measure("idem_raw", Value::Text(format!("rm={} rmdir={}", status(&rm), status(&rd))));
    Ok(())
}

/// Punctuation `RelPath` allows, in a directory and a file name.
pub(crate) const PUNCT: [char; 10] = ['_', '-', '.', '+', '@', '=', ',', '(', ')', ' '];

/// Step 7: for each character a directory and a file named with it: make, size, remove, gone.
fn punctuation(ctx: &mut ProbeCtx<'_>, fs: &mut Fs) -> ProbeResult {
    let mut bad = String::new();
    for c in PUNCT {
        let (d, f) = (format!("p{c}d"), format!("p{c}d/f{c}g.bin"));
        let made = fs.mkdir(ctx, &d)?;
        let file = fs.file(ctx, &f)?;
        let put = made && fs.put(ctx, &file, &[c as u8])?;
        let removed = put && fs.rm(ctx, &f)? && !fs.readable(ctx, &f)?;
        let dir = fs.path(&d)?;
        let rd = removed && soft(ctx.session.cmd(&cmd::rmdir(&dir)))?.is_some() && !fs.dir_exists(ctx, &dir)?;
        if !rd {
            bad.push(c);
        }
    }
    ctx.text("punct_bad", if bad.is_empty() { "none".into() } else { format!("{bad:?}") });
    Ok(())
}

/// Step 8: the listing's name form and whether it includes a directory.
fn listing(ctx: &mut ProbeCtx<'_>, fs: &mut Fs) -> ProbeResult {
    fs.mkdir(ctx, "ls")?;
    match list_dir(ctx.session, &fs.dir)? {
        Listing::Names { names, first_raw } => {
            ctx.measure("list_first", Value::Text(first_raw.clone()));
            ctx.text("list_form", if first_raw.contains(['\\', '/']) { "path" } else { "name" });
            ctx.text("list_dirs", if names.iter().any(|n| n.eq_ignore_ascii_case("ls")) { "yes" } else { "no" });
        }
        Listing::Unavailable(why) => {
            ctx.measure("list_why", Value::Text(why));
            ctx.text("list_form", "unavailable");
        }
    }
    Ok(())
}

/// Names below `D` nobody registered: their removal registered, reported as `stray`.
fn strays(ctx: &mut ProbeCtx<'_>, fs: &Fs) -> ProbeResult {
    let Listing::Names { names, .. } = list_dir(ctx.session, &fs.dir)? else { return Ok(()) };
    let stray: Vec<String> = names.into_iter().filter(|n| !fs.known.contains(&n.to_ascii_lowercase())).collect();
    for n in &stray {
        if let Ok(p) = fs.dir.join(n) {
            ctx.push(Undo::RemoveFile(p))?;
        }
    }
    ctx.measure("stray_names", Value::Text(stray.join(",")));
    if !stray.is_empty() {
        ctx.headline("stray", Value::Count(stray.len() as u64));
    }
    Ok(())
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
