// SPDX-License-Identifier: Apache-2.0
//! Single-file movement between the client and the PowerView file system through `VM:`.
//! Anything that is one command stays a `practice::cmd` builder plus `Session::cmd`.

use std::{
    io::{Read, Write},
    time::Duration,
};

use flate2::{Compression, GzBuilder, read::GzDecoder};
use t32rcl::{CallBudget, Session};

use crate::{
    error::{StageError, VmError},
    practice::{StagePath, StageRoot, cmd, fnc},
    vm::VmRegion,
};

/// How bytes become a file on the PowerView host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Put {
    /// Raw bytes over RCL, then `Data.SAVE.Binary`.
    Plain,
    /// gzip over RCL into a temporary `.gz`, `Data.LOAD.Binary /UNZIP` into `VM:`, then save the
    /// plain file. Needs `VM:` for both sizes.
    Inflate,
    /// gzip over RCL, saved as the `.gz` file itself (for `Data.LOAD.Elf /UNZIP`).
    KeepGz,
}

/// Deterministic gzip: header mtime 0, level 6.
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = GzBuilder::new().mtime(0).write(Vec::new(), Compression::new(6));
    enc.write_all(bytes).expect("writing to a Vec cannot fail");
    enc.finish().expect("writing to a Vec cannot fail")
}

pub fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, StageError> {
    let mut out = Vec::new();
    GzDecoder::new(bytes).read_to_end(&mut out).map_err(StageError::Gzip)?;
    Ok(out)
}

/// Budget for a command that moves `bytes`: never below the session's idle budget.
pub fn data_budget(session: &Session, bytes: u64) -> CallBudget {
    // ASSUMED(vm-large): 10 s plus 1 s per MB covers the silence of a SAVE or LOAD.
    let sized = Duration::from_secs(10) + Duration::from_secs_f64(bytes as f64 / 1e6);
    let idle = session.budget().idle.max(sized);
    CallBudget::idle(idle).with_total(idle + Duration::from_secs(60))
}

/// `Data.SAVE.Binary dest VM:<region>` under `data_budget(region.len())`.
pub(crate) fn save_region(session: &mut Session, dest: &StagePath, region: &VmRegion) -> Result<(), StageError> {
    let budget = data_budget(session, region.len().into());
    session.with_budget(budget).cmd(&cmd::data_save_binary(dest, region, false))?;
    Ok(())
}

/// `Data.LOAD.Binary src VM:at [/UNZIP]` under `data_budget(len)`; `len` is what lands in `VM:`.
pub(crate) fn load_region(
    session: &mut Session,
    src: &StagePath,
    at: u32,
    len: u32,
    unzip: bool,
) -> Result<(), StageError> {
    let budget = data_budget(session, len.into());
    session.with_budget(budget).cmd(&cmd::data_load_binary(src, at, unzip))?;
    Ok(())
}

pub fn file_size(session: &mut Session, p: &StagePath) -> Result<u64, StageError> {
    let expr = fnc::file_size(p);
    let n = session.eval(&expr)?.into_int(&expr)?;
    u64::try_from(n).map_err(|_| StageError::BadReply { what: "OS.FILE.SIZE", text: n.to_string() })
}

/// RCL memory write of `bytes` into `region`, `Data.SAVE.Binary dest`, then compare
/// `OS.FILE.SIZE(dest)`. The caller's ledger entry for `dest` covers a partial file.
pub fn put_file(session: &mut Session, region: &VmRegion, bytes: &[u8], dest: &StagePath) -> Result<(), StageError> {
    let slice = region
        .slice(0, bytes.len() as u32)
        .ok_or(VmError::WindowTooSmall { need: bytes.len() as u64, have: region.len().into() })?;
    session.mem_write(&slice.addr(), bytes)?;
    save_region(session, dest, &slice)?;
    let got = file_size(session, dest)?;
    if got != bytes.len() as u64 {
        return Err(StageError::SizeMismatch { what: dest.to_string(), want: bytes.len() as u64, got });
    }
    Ok(())
}

/// `Data.LOAD.Binary src VM:<region>` and an RCL read of `len` bytes. `len` comes from
/// `OS.FILE.SIZE(src)`; the region must hold it.
pub fn get_file(session: &mut Session, src: &StagePath, region: &VmRegion, len: u32) -> Result<Vec<u8>, StageError> {
    let slice = region.slice(0, len).ok_or(VmError::WindowTooSmall { need: len.into(), have: region.len().into() })?;
    load_region(session, src, slice.start(), len, false)?;
    Ok(session.mem_read(&slice.addr(), len as usize)?)
}

/// Create the root itself if missing. The root is shared and never removed.
pub fn ensure_root(session: &mut Session, root: &StageRoot) -> Result<(), StageError> {
    let text = root.as_path().as_str().to_string();
    let quoted = format!("\"{text}\"");
    let expr = format!("OS.DIR({quoted})");
    if !session.eval(&expr)?.into_bool(&expr)? {
        session.cmd(&format!("MKDIR {quoted}"))?;
        if !session.eval(&expr)?.into_bool(&expr)? {
            return Err(StageError::BadReply { what: "MKDIR of the stage root", text });
        }
    }
    Ok(())
}

/// The names in `dir`, at most `limit`: `OS.FIRSTFILE("<dir>\*")`, then `OS.NEXTFILE()` until
/// `""`, each name the text after the last separator. `.` and `..` are left out: build 168999
/// lists them (`fs-semantics`). Stops at `limit` names or `limit + 2` answers.
pub(crate) fn list_dir(session: &mut Session, dir: &StagePath, limit: usize) -> Result<Vec<String>, StageError> {
    let first = fnc::first_file(dir);
    let mut name = session.eval(&first)?.into_text(&first)?;
    let (mut names, mut answers) = (Vec::new(), 0);
    while !name.is_empty() {
        let base = name.rsplit(['\\', '/']).next().unwrap_or_default();
        if !matches!(base, "." | "..") {
            names.push(base.to_string());
        }
        answers += 1;
        if names.len() >= limit || answers >= limit + 2 {
            break;
        }
        name = session.eval(fnc::NEXT_FILE)?.into_text(fnc::NEXT_FILE)?;
    }
    Ok(names)
}

/// Create `dir` and missing parents below the root, shallowest first, checking `OS.DIR` before
/// each `MKDIR`. Returns what it would create, in creation order, through `before_mkdir`, which
/// runs before each `MKDIR` so the caller can register the removal first.
pub fn ensure_dir(
    session: &mut Session,
    dir: &StagePath,
    mut before_mkdir: impl FnMut(&StagePath) -> Result<(), StageError>,
) -> Result<(), StageError> {
    let rel = dir.rel();
    let prefixes = rel.parents().map(str::to_string).chain([rel.as_str().to_string()]);
    for prefix in prefixes {
        let p = dir.root().path(crate::practice::RelPath::new(&prefix)?);
        let expr = fnc::dir_exists(&p);
        if session.eval(&expr)?.into_bool(&expr)? {
            continue;
        }
        before_mkdir(&p)?;
        session.cmd(&cmd::mkdir(&p))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use t32rcl::testkit::{Expect, Script};

    use super::*;
    use crate::{
        practice::RelPath,
        testutil::{session, session_with},
    };

    #[test]
    fn gzip_round_trips_and_is_deterministic() {
        let data: Vec<u8> = (0..5000).map(|i| (i % 7) as u8).collect();
        assert_eq!(gunzip(&gzip(&data)).unwrap(), data);
        assert_eq!(gzip(&data), gzip(&data));
        assert!(gunzip(&gzip(&data)[..20]).is_err());
    }

    #[test]
    fn put_file_writes_saves_and_checks_the_size() {
        let root = StageRoot::from_ptd("C:\\T").unwrap();
        let dest = root.path(RelPath::new("a.bin").unwrap());
        let region = VmRegion::new(0x0100_0000, std::num::NonZeroU32::new(4096).unwrap()).unwrap();
        let s = Script::new()
            .attach()
            .expect(Expect::MemWriteLen { access: "VM".into(), addr: 0x0100_0000, len: 3 })
            .reply_ok()
            .expect_cmd("Data.SAVE.Binary \"C:\\T\\t32ctl\\a.bin\" VM:0x01000000++0x00000002")
            .reply_ok()
            .expect_fn("OS.FILE.SIZE(\"C:\\T\\t32ctl\\a.bin\")")
            .reply_decimal(2);
        let mut session = session(s);
        assert!(matches!(
            put_file(&mut session, &region, &[1, 2, 3], &dest),
            Err(StageError::SizeMismatch { want: 3, got: 2, .. })
        ));
    }

    #[test]
    fn data_budget_grows_with_bytes_and_never_undercuts_the_session() {
        let s = Script::new().attach();
        let session = session_with(vec![Ok(s)], |o| o.budget = CallBudget::idle(Duration::from_secs(20)));
        assert_eq!(data_budget(&session, 0).idle, Duration::from_secs(20));
        let big = data_budget(&session, 32 << 20);
        assert!(big.idle >= Duration::from_secs(43), "{big:?}");
        assert_eq!(big.total, Some(big.idle + Duration::from_secs(60)));
    }

    /// `save_region` of `len` bytes whose reply comes after 12 s of silence, under the default
    /// budget. Without `replies` the script ends at the silence.
    fn slow_save(len: u32, replies: bool) -> Result<(), StageError> {
        let dest = StageRoot::from_ptd("C:\\T").unwrap().path(RelPath::new("big.bin").unwrap());
        let region = VmRegion::new(0x0100_0000, std::num::NonZeroU32::new(len).unwrap()).unwrap();
        let s = Script::new().attach().expect(Expect::CommandStarting("Data.SAVE.Binary ".into()));
        let s = s.silence(Duration::from_secs(12));
        let s = if replies { s.reply_ok() } else { s };
        let mut session = session_with(vec![Ok(s)], |o| o.budget = CallBudget::DEFAULT);
        save_region(&mut session, &dest, &region)
    }

    #[test]
    fn a_save_of_a_large_region_survives_a_reply_slower_than_the_default_idle() {
        slow_save(4 << 20, true).unwrap();
        assert!(matches!(slow_save(1024, false), Err(StageError::Rcl(t32rcl::Error::Timeout { .. }))));
    }

    #[test]
    fn a_listing_names_entries_without_the_dots_and_stops_at_its_limit() {
        let dir = StageRoot::from_ptd("C:\\T").unwrap().path(RelPath::new("d").unwrap());
        let list = |answers: &[&str], limit: usize| {
            let mut s = Script::new().attach().expect_fn("OS.FIRSTFILE(\"C:\\T\\t32ctl\\d\\*\")");
            for (i, a) in answers.iter().enumerate() {
                s = if i == 0 { s.reply_text(a) } else { s.expect_fn("OS.NEXTFILE()").reply_text(a) };
            }
            list_dir(&mut session(s), &dir, limit).unwrap()
        };
        assert_eq!(list(&["C:\\T\\t32ctl\\d\\.", "..", "a.log", "C:\\T\\t32ctl\\d\\sub", ""], 9), ["a.log", "sub"]);
        assert_eq!(list(&[".", "..", "a.log"], 1), ["a.log"]);
        assert_eq!(list(&[".", "..", "."], 1), Vec::<String>::new());
        assert_eq!(list(&[""], 9), Vec::<String>::new());
    }

    #[test]
    fn ensure_dir_registers_before_each_missing_level() {
        let root = StageRoot::from_ptd("C:\\T").unwrap();
        let dir = root.path(RelPath::new("a/b").unwrap());
        let s = Script::new()
            .attach()
            .expect_fn("OS.DIR(\"C:\\T\\t32ctl\\a\")")
            .reply_bool(true)
            .expect_fn("OS.DIR(\"C:\\T\\t32ctl\\a\\b\")")
            .reply_bool(false)
            .expect_cmd("MKDIR \"C:\\T\\t32ctl\\a\\b\"")
            .reply_ok();
        let mut seen = Vec::new();
        ensure_dir(&mut session(s), &dir, |p| {
            seen.push(p.rel().to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, ["a/b"]);
    }

    #[test]
    fn a_refused_registration_stops_ensure_dir_before_its_mkdir() {
        let dir = StageRoot::from_ptd("C:\\T").unwrap().path(RelPath::new("a/b").unwrap());
        let s = Script::new().attach().expect_fn("OS.DIR(\"C:\\T\\t32ctl\\a\")").reply_bool(false);
        let journal_full =
            |_: &StagePath| Err(StageError::Journal { path: "j".into(), source: std::io::Error::other("disk full") });
        assert!(matches!(ensure_dir(&mut session(s), &dir, journal_full), Err(StageError::Journal { .. })));
    }
}
