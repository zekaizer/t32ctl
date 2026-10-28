// SPDX-License-Identifier: Apache-2.0
//! Single-file movement between the client and the PowerView file system through `VM:`.
//! Anything that is one command stays a `practice::cmd` builder plus `Session::cmd`.

use std::io::{Read, Write};

use flate2::{Compression, GzBuilder, read::GzDecoder};
use t32rcl::Session;

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
    session.cmd(&cmd::data_save_binary(dest, &slice, false))?;
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
    session.cmd(&cmd::data_load_binary(src, slice.start(), false))?;
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
    use crate::{practice::RelPath, testutil::session};

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
