// SPDX-License-Identifier: Apache-2.0
//! TRACE32 virtual memory `VM:` as a shared resource: the window we may use, regions carved
//! from it, a cross-process lease, and per-chunk snapshots for windows other people may use.

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Seek, Write},
    num::NonZeroU32,
    path::Path,
};

use t32rcl::{ErrorClass, MemAddr, RemoteKind, Session};

use crate::{
    error::{LeaseError, StageError, VmError},
    practice::cmd,
};

const ALIGN: u32 = 4096;
/// Snapshot granularity: one unreadable unit is restored on its own, never the whole region.
pub const SNAPSHOT_UNIT: u32 = 4096;

/// Address span in `VM:` we may touch. 32-bit, because generated PRACTICE text writes addresses
/// with fixed width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmWindow {
    base: u32,
    len: u32,
}

#[allow(clippy::len_without_is_empty)] // never empty by construction
impl VmWindow {
    pub const DEFAULT_BASE: u32 = 0x0100_0000;
    pub const DEFAULT_LEN: u32 = 64 * 1024 * 1024;

    pub fn new(base: u32, len: u32) -> Result<Self, VmError> {
        if len == 0 || u64::from(base) + u64::from(len) > u64::from(u32::MAX) {
            return Err(VmError::WindowRange { base: base.into(), len: len.into() });
        }
        Ok(Self { base, len })
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    /// Consecutive 4 KiB aligned regions for `sizes`, in order, never overlapping.
    pub fn carve(&self, sizes: &[u32]) -> Result<Vec<VmRegion>, VmError> {
        let mut at = u64::from(self.base);
        let mut out = Vec::with_capacity(sizes.len());
        for &s in sizes {
            let len = NonZeroU32::new(s).ok_or(VmError::ZeroLength)?;
            out.push(VmRegion { start: at as u32, len });
            at += u64::from(s).next_multiple_of(u64::from(ALIGN));
        }
        let limit = u64::from(self.base) + u64::from(self.len);
        if at > limit {
            return Err(VmError::WindowTooSmall { need: at - u64::from(self.base), have: self.len.into() });
        }
        Ok(out)
    }
}

impl Default for VmWindow {
    fn default() -> Self {
        Self { base: Self::DEFAULT_BASE, len: Self::DEFAULT_LEN }
    }
}

/// A non-empty span of `VM:`. PRACTICE cannot express an empty range, so the type cannot either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmRegion {
    start: u32,
    len: NonZeroU32,
}

#[allow(clippy::len_without_is_empty)] // never empty by construction
impl VmRegion {
    pub fn new(start: u32, len: NonZeroU32) -> Result<Self, VmError> {
        start.checked_add(len.get()).ok_or(VmError::WindowRange { base: start.into(), len: len.get().into() })?;
        Ok(Self { start, len })
    }

    pub fn start(&self) -> u32 {
        self.start
    }

    pub fn len(&self) -> u32 {
        self.len.get()
    }

    pub fn addr(&self) -> MemAddr {
        MemAddr::vm(self.start.into())
    }

    /// Sub-span `[offset, offset+len)`. `None` if empty or outside the region.
    pub fn slice(&self, offset: u32, len: u32) -> Option<VmRegion> {
        let len = NonZeroU32::new(len)?;
        let end = offset.checked_add(len.get())?;
        (end <= self.len()).then(|| VmRegion { start: self.start + offset, len })
    }
}

/// Exclusive use of a window across processes on this machine (`flock` semantics through
/// `File::try_lock`). Dropping releases it.
#[derive(Debug)]
pub struct VmLease {
    _file: File,
    window: VmWindow,
}

impl VmLease {
    pub fn acquire(state_dir: &Path, window: VmWindow, holder: &str) -> Result<Self, LeaseError> {
        let path = state_dir.join(format!("vm-{:08x}.lock", window.base()));
        let io = |source| LeaseError::Io { path: path.clone(), source };
        fs::create_dir_all(state_dir).map_err(io)?;
        let mut file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).map_err(io)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let mut text = String::new();
                let _ = file.read_to_string(&mut text);
                return Err(LeaseError::Busy { holder: (!text.trim().is_empty()).then(|| text.trim().to_string()) });
            }
            Err(TryLockError::Error(e)) => return Err(io(e)),
        }
        file.set_len(0).map_err(io)?;
        file.rewind().map_err(io)?;
        writeln!(file, "pid {} {holder}", std::process::id()).map_err(io)?;
        Ok(Self { _file: file, window })
    }

    pub fn window(&self) -> VmWindow {
        self.window
    }
}

/// One snapshot unit: its bytes, or `None` when PowerView refused to read it (never initialized).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prior {
    pub region: VmRegion,
    pub bytes: Option<Vec<u8>>,
}

/// Contents of a region before we wrote to it, per `SNAPSHOT_UNIT`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmSnapshot {
    region: VmRegion,
    chunks: Vec<Prior>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// Every unit reads back as captured; `uninit` units were cleared with `Data.CLEARVM`.
    Restored { uninit: usize },
    /// A readable unit reads back different at this region offset.
    Mismatch { first: u64 },
    /// Units that hold our data now: captured bytes PowerView refused to take back (`unwritten`,
    /// not verified), or uninitialized units it could not clear again (`uncleared`).
    Unrestored { unwritten: Vec<VmRegion>, uncleared: Vec<VmRegion> },
}

impl VmSnapshot {
    /// Read the region unit by unit. A unit PowerView refuses (`MemoryAccess`) is recorded as
    /// uninitialized; any other error aborts. ASSUMED(vm-rw): reading never-initialized `VM:`
    /// fails rather than returning zeros (`vm_uninit_units`).
    pub fn capture(session: &mut Session, region: VmRegion) -> Result<Self, StageError> {
        let mut chunks = Vec::new();
        let mut off = 0;
        while off < region.len() {
            let unit = region.slice(off, SNAPSHOT_UNIT.min(region.len() - off)).expect("inside the region");
            let bytes = match session.mem_read(&unit.addr(), unit.len() as usize) {
                Ok(b) => Some(b),
                Err(e) if e.remote().is_some_and(|r| r.kind == RemoteKind::MemoryAccess) => None,
                Err(e) => return Err(e.into()),
            };
            chunks.push(Prior { region: unit, bytes });
            off += unit.len();
        }
        Ok(Self { region, chunks })
    }

    pub fn region(&self) -> VmRegion {
        self.region
    }

    pub fn chunks(&self) -> &[Prior] {
        &self.chunks
    }

    /// True if a captured byte is non-zero: somebody uses this memory.
    pub fn occupied(&self) -> bool {
        self.chunks.iter().any(|c| c.bytes.as_ref().is_some_and(|b| b.iter().any(|&x| x != 0)))
    }

    pub fn uninitialized(&self) -> usize {
        self.chunks.iter().filter(|c| c.bytes.is_none()).count()
    }

    /// Uninitialized units with adjacent ones merged: the ranges a manual `Data.CLEARVM` covers.
    pub fn uninit_runs(&self) -> Vec<VmRegion> {
        let mut runs: Vec<VmRegion> = Vec::new();
        for c in self.chunks.iter().filter(|c| c.bytes.is_none()) {
            match runs.last_mut() {
                Some(r) if r.start() + r.len() == c.region.start() => {
                    let len = NonZeroU32::new(r.len() + c.region.len()).expect("non-empty units");
                    *r = VmRegion { start: r.start(), len };
                }
                _ => runs.push(c.region),
            }
        }
        runs
    }

    /// Write captured units back, clear uninitialized ones, then read back every unit that was
    /// put back to verify it. A refused unit is recorded and the rest still restored; only an
    /// error that ends the connection or the call stops the restore.
    pub fn restore(&self, session: &mut Session) -> Result<RestoreOutcome, StageError> {
        let (mut unwritten, mut uncleared) = (Vec::new(), Vec::new());
        for c in &self.chunks {
            let (result, refused) = match &c.bytes {
                Some(b) => (session.mem_write(&c.region.addr(), b), &mut unwritten),
                None => (session.cmd(&cmd::clear_vm(&c.region)), &mut uncleared),
            };
            match result {
                Ok(()) => {}
                Err(e) if e.class() == ErrorClass::Rejected => refused.push(c.region),
                Err(e) => return Err(e.into()),
            }
        }
        for c in self.chunks.iter().filter(|c| !unwritten.contains(&c.region)) {
            let back = session.mem_read(&c.region.addr(), c.region.len() as usize);
            match (&c.bytes, back) {
                (Some(want), Ok(got)) => {
                    if let Some(i) = want.iter().zip(&got).position(|(a, b)| a != b) {
                        let first = u64::from(c.region.start() - self.region.start()) + i as u64;
                        return Ok(RestoreOutcome::Mismatch { first });
                    }
                }
                (None, Ok(_)) => {
                    if !uncleared.contains(&c.region) {
                        uncleared.push(c.region);
                    }
                }
                (_, Err(e)) if e.remote().is_some_and(|r| r.kind == RemoteKind::MemoryAccess) && c.bytes.is_none() => {}
                (_, Err(e)) => return Err(e.into()),
            }
        }
        if unwritten.is_empty() && uncleared.is_empty() {
            Ok(RestoreOutcome::Restored { uninit: self.uninitialized() })
        } else {
            Ok(RestoreOutcome::Unrestored { unwritten, uncleared })
        }
    }
}

/// Whether `Stage` snapshots the regions it is about to overwrite.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VmGuard {
    /// The window belongs to t32ctl alone, enforced by the lease (Phase 1 default).
    #[default]
    Dedicated,
    /// Somebody else may use it: snapshot first, restore and verify afterwards (probes).
    SnapshotRestore,
}

#[cfg(test)]
mod tests {
    use t32rcl::{
        Tuning,
        testkit::{Request, Script},
    };

    use super::*;
    use crate::testutil::session_with;

    fn connect(script: Script) -> Session {
        session_with(vec![Ok(script)], |o| o.tuning = Tuning::new(4096, 4096).unwrap())
    }

    fn read(s: Script, addr: u64) -> Script {
        s.expect_req(Request::MemRead { access: "VM".into(), addr, len: 4096 })
    }

    fn write(s: Script, addr: u64, byte: u8) -> Script {
        s.expect_req(Request::MemWrite { access: "VM".into(), addr, data: vec![byte; 4096] }).reply_ok()
    }

    fn region() -> VmRegion {
        VmRegion::new(0x1000_0000, NonZeroU32::new(2 * 4096).unwrap()).unwrap()
    }

    #[test]
    fn carve_aligns_and_refuses_overflow() {
        let w = VmWindow::new(0x0100_0000, 0x10_000).unwrap();
        let r = w.carve(&[1, 4097, 10]).unwrap();
        assert_eq!(
            r.iter().map(|r| (r.start(), r.len())).collect::<Vec<_>>(),
            [(0x0100_0000, 1), (0x0100_1000, 4097), (0x0100_3000, 10)]
        );
        assert!(matches!(w.carve(&[0x10_001]), Err(VmError::WindowTooSmall { .. })));
        assert!(VmWindow::new(u32::MAX - 1, 4).is_err());
        assert_eq!(r[1].slice(4096, 1).map(|s| s.start()), Some(0x0100_2000));
        assert!(r[1].slice(4096, 2).is_none());
    }

    #[test]
    fn lease_is_exclusive_across_open_file_descriptions() {
        let dir = std::env::temp_dir().join(format!("t32stage-lease-{}", std::process::id()));
        let w = VmWindow::default();
        let held = VmLease::acquire(&dir, w, "first").unwrap();
        match VmLease::acquire(&dir, w, "second") {
            Err(LeaseError::Busy { holder: Some(h) }) => assert!(h.ends_with("first"), "{h}"),
            other => panic!("{other:?}"),
        }
        drop(held);
        VmLease::acquire(&dir, w, "third").unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn restore_writes_back_and_verifies_each_unit() {
        let mut s = read(Script::new().attach(), 0x1000_0000).reply_mem(&[7; 4096]);
        s = read(s, 0x1000_1000).reply_mem(&[0; 4096]);
        s = write(s, 0x1000_0000, 7);
        s = write(s, 0x1000_1000, 0);
        s = read(s, 0x1000_0000).reply_mem(&[7; 4096]);
        s = read(s, 0x1000_1000).reply_mem(&[0; 4096]);
        let mut session = connect(s);
        let snap = VmSnapshot::capture(&mut session, region()).unwrap();
        assert!(snap.occupied());
        assert_eq!(snap.restore(&mut session).unwrap(), RestoreOutcome::Restored { uninit: 0 });
    }

    #[test]
    fn an_unreadable_unit_is_cleared_alone_and_verified_unreadable() {
        let mut s = read(Script::new().attach(), 0x1000_0000).reply_error(16, "");
        s = read(s, 0x1000_1000).reply_mem(&[3; 4096]);
        s = s.expect_cmd("Data.CLEARVM VM:0x10000000++0x00000FFF").reply_ok();
        s = write(s, 0x1000_1000, 3);
        s = read(s, 0x1000_0000).reply_error(16, "");
        s = read(s, 0x1000_1000).reply_mem(&[3; 4096]);
        let mut session = connect(s);
        let snap = VmSnapshot::capture(&mut session, region()).unwrap();
        assert_eq!(snap.uninitialized(), 1);
        assert_eq!(snap.restore(&mut session).unwrap(), RestoreOutcome::Restored { uninit: 1 });
    }

    #[test]
    fn the_manual_clear_is_one_command_per_run_of_uninitialized_units() {
        let mut s = read(Script::new().attach(), 0x1000_0000).reply_error(16, "");
        s = read(s, 0x1000_1000).reply_mem(&[1; 4096]);
        s = read(s, 0x1000_2000).reply_error(16, "");
        s = read(s, 0x1000_3000).reply_error(16, "");
        let mut session = connect(s);
        let four = VmRegion::new(0x1000_0000, NonZeroU32::new(4 * 4096).unwrap()).unwrap();
        let snap = VmSnapshot::capture(&mut session, four).unwrap();
        let hint = crate::ledger::Undo::RestoreVm(snap).manual_hint().unwrap();
        assert_eq!(
            hint.lines().collect::<Vec<_>>(),
            ["Data.CLEARVM VM:0x10000000++0x00000FFF", "Data.CLEARVM VM:0x10002000++0x00001FFF"]
        );
    }

    #[test]
    fn a_refused_write_back_skips_that_unit_and_restores_the_rest() {
        let mut s = read(Script::new().attach(), 0x1000_0000).reply_mem(&[7; 4096]);
        s = read(s, 0x1000_1000).reply_mem(&[8; 4096]);
        s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: 0x1000_0000, data: vec![7; 4096] });
        s = s.reply_error(16, "bus error");
        s = write(s, 0x1000_1000, 8);
        s = read(s, 0x1000_1000).reply_mem(&[8; 4096]);
        let mut session = connect(s);
        let snap = VmSnapshot::capture(&mut session, region()).unwrap();
        let first = region().slice(0, 4096).unwrap();
        assert_eq!(
            snap.restore(&mut session).unwrap(),
            RestoreOutcome::Unrestored { unwritten: vec![first], uncleared: vec![] }
        );
    }

    #[test]
    fn a_missing_clearvm_and_a_wrong_readback_are_reported() {
        let mut s = read(Script::new().attach(), 0x1000_0000).reply_error(16, "");
        s = read(s, 0x1000_1000).reply_mem(&[3; 4096]);
        s = s.expect_cmd("Data.CLEARVM VM:0x10000000++0x00000FFF").reply_error(90, "unknown command");
        s = write(s, 0x1000_1000, 3);
        s = read(s, 0x1000_0000).reply_mem(&[0; 4096]);
        s = read(s, 0x1000_1000).reply_mem(&[3; 4096]);
        let mut session = connect(s);
        let snap = VmSnapshot::capture(&mut session, region()).unwrap();
        let first = region().slice(0, 4096).unwrap();
        assert_eq!(
            snap.restore(&mut session).unwrap(),
            RestoreOutcome::Unrestored { unwritten: vec![], uncleared: vec![first] }
        );

        let mut s = read(Script::new().attach(), 0x1000_0000).reply_mem(&[1; 4096]);
        s = read(s, 0x1000_1000).reply_mem(&[2; 4096]);
        s = write(s, 0x1000_0000, 1);
        s = write(s, 0x1000_1000, 2);
        s = read(s, 0x1000_0000).reply_mem(&[1; 4096]);
        let mut bad = vec![2; 4096];
        bad[5] = 9;
        s = read(s, 0x1000_1000).reply_mem(&bad);
        let mut session = connect(s);
        let snap = VmSnapshot::capture(&mut session, region()).unwrap();
        assert_eq!(snap.restore(&mut session).unwrap(), RestoreOutcome::Mismatch { first: 4096 + 5 });
    }
}
