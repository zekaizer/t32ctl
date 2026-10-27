// SPDX-License-Identifier: Apache-2.0
//! `Stage`: the staging facade.
//!
//! | Operation | Hidden from the caller |
//! |---|---|
//! | `open` | VM: lease, journal, stage root resolution, macro declaration |
//! | `ensure_snapshot` | gzip, two `VM:` regions, unpack script, offsets, token wait, marker |
//! | `run` | idle check, macros, token protocol, dialog handling, unwinding |
//! | `fetch_outputs` | `TAR`, `ZIP`, `VM:` region, tar walking |
//! | `upload` / `download` | regions, gzip, size checks, temporary files |
//! | `close` | the cleanup ledger |
//!
//! `Stage` does not own the `Session`: every call takes `&mut Session`, so the caller keeps using
//! the session between calls.

use std::{
    collections::BTreeSet,
    fmt,
    num::NonZeroU32,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use t32rcl::{PracticeState, Session};

use crate::{
    bundle::{Bundle, MARKER_PATH, RESERVED_DIR, RUNNER_PATH, SnapshotKey, UNPACK_PATH},
    error::{PathError, StageError, VmError},
    ledger::{CleanupReport, Journal, Ledger, Undo, stack_depth, unwind_practice},
    macros::Macro,
    practice::{ExistsFn, PvPath, RelPath, StagePath, StageRoot, cmd, fnc, render_args},
    runner::{
        IdleVerdict, Next, PollSchedule, PollStats, PracticeErrorInfo, RunOutcome, RunReport, RunRequest, ScriptWatch,
    },
    tar::{self, TarDiag, TarEntry},
    transfer::{self, Put},
    vm::{VmGuard, VmLease, VmRegion, VmSnapshot, VmWindow},
};

/// Whether snapshot files outlive the `Stage`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Retention {
    /// Snapshots stay and are reused by key (Phase 1).
    #[default]
    Cache,
    /// Every directory and file the stage creates is removed at `close` (probes).
    Transient,
}

#[derive(Clone, Debug)]
pub struct StageConfig {
    pub window: VmWindow,
    pub guard: VmGuard,
    /// Lease file and journal directory, normally `ledger::default_state_dir()`.
    pub state_dir: PathBuf,
    /// Replaces `OS.PTD()\t32ctl` (operator override, e.g. `--stage-root`).
    pub root: Option<PvPath>,
    /// Extra directory level below the root, e.g. `probe-<run id>`.
    pub subdir: Option<RelPath>,
    pub retention: Retention,
    pub exists: ExistsFn,
    pub poll: PollSchedule,
    /// Shown to a process that finds the lease held.
    pub holder: String,
    /// Journal file name, unique per process.
    pub journal_name: String,
}

#[derive(Debug)]
pub struct Snapshot {
    pub key: SnapshotKey,
    pub dir: StagePath,
    pub cached: bool,
    pub upload: Option<UploadStats>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UploadStats {
    pub raw_bytes: u64,
    pub gz_bytes: u64,
    pub elapsed: Duration,
    pub unpack: Duration,
    pub polls: u32,
}

#[derive(Debug)]
pub struct StagedFile {
    pub path: StagePath,
    pub len: u64,
}

#[derive(Debug, Default)]
pub struct Outputs {
    pub files: Vec<TarEntry>,
    pub diag: TarDiag,
}

const UNPACK_TIMEOUT: Duration = Duration::from_secs(120);

/// `Stage::open` failed. `cleanup` holds what was registered before the failure. The caller runs
/// it under a `Cancel` of its own: the failure may be the caller's deadline or Ctrl-C firing.
#[derive(Debug)]
pub struct OpenError {
    pub error: StageError,
    pub cleanup: Ledger,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug)]
pub struct Stage {
    cfg: StageConfig,
    root: StageRoot,
    _lease: VmLease,
    ledger: Ledger,
    claimed_end: u32,
    #[cfg(test)]
    pub(crate) fixed_token: Option<String>,
}

impl Stage {
    /// Take the VM: lease, open the journal, resolve the root, declare the macros, ensure the
    /// root and subdirectory exist. On failure nothing is undone here: `OpenError::cleanup`
    /// carries what was registered so far.
    pub fn open(session: &mut Session, cfg: StageConfig) -> Result<Self, Box<OpenError>> {
        let exists = cfg.exists;
        let bare = move |error: StageError| Box::new(OpenError { error, cleanup: Ledger::new(exists) });
        let lease = VmLease::acquire(&cfg.state_dir, cfg.window, &cfg.holder).map_err(|e| bare(e.into()))?;
        let journal = Journal::create(&cfg.state_dir, &cfg.journal_name).map_err(bare)?;
        let ledger = Ledger::journaled(journal, cfg.exists);
        let claimed_end = cfg.window.base();
        let mut stage = Stage {
            root: StageRoot::Macro,
            cfg,
            _lease: lease,
            ledger,
            claimed_end,
            #[cfg(test)]
            fixed_token: None,
        };
        match stage.setup(session) {
            Ok(()) => Ok(stage),
            Err(error) => Err(Box::new(OpenError { error, cleanup: stage.ledger })),
        }
    }

    fn setup(&mut self, session: &mut Session) -> Result<(), StageError> {
        self.root = match &self.cfg.root {
            Some(p) => StageRoot::Literal(p.clone()),
            None => StageRoot::from_ptd(&session.eval(fnc::PTD)?.into_text(fnc::PTD)?),
        };
        if self.root == StageRoot::Macro {
            for c in cmd::bind_root() {
                session.cmd(&c)?;
            }
        }
        let ours: Vec<Macro> = Macro::ALL.iter().copied().filter(|m| *m != Macro::Root).collect();
        self.ledger.push(Undo::ClearMacros(ours.iter().map(|m| m.name()).collect()))?;
        session.cmd(&cmd::global(&ours))?;
        transfer::ensure_root(session, &self.root)?;
        if let Some(sub) = self.cfg.subdir.clone() {
            let ledger = &mut self.ledger;
            transfer::ensure_dir(session, &self.root.path(sub), |p| ledger.push(Undo::RemoveDir(p.clone())))?;
        }
        Ok(())
    }

    pub fn root(&self) -> &StageRoot {
        &self.root
    }

    /// `<root>[/<subdir>]/<rel>`.
    pub fn path(&self, rel: &str) -> Result<StagePath, PathError> {
        let rel = match &self.cfg.subdir {
            Some(sub) => sub.join(rel)?,
            None => RelPath::new(rel)?,
        };
        Ok(self.root.path(rel))
    }

    /// Make sure the bundle is unpacked and complete on the PowerView host. A complete snapshot
    /// costs one function call; an incomplete one is unpacked again. Needs an idle PRACTICE stack.
    pub fn ensure_snapshot(&mut self, session: &mut Session, bundle: &Bundle) -> Result<Snapshot, StageError> {
        let key = bundle.key();
        let dir = self.path(&key.hex())?;
        let marker = dir.join(MARKER_PATH)?;
        if self.exists(session, &marker)? {
            return Ok(Snapshot { key, dir, cached: true, upload: None });
        }
        self.require_idle(session)?;
        let t0 = Instant::now();
        let raw_len = bundle.pack(0).raw_len;
        let b = self.claim(session, &[raw_len])?[0];
        let packed = bundle.pack(b.start());
        let a = self.claim(session, &[packed.raw_len, packed.gz.len() as u32])?[1];

        let transient = self.cfg.retention == Retention::Transient;
        let ledger = &mut self.ledger;
        transfer::ensure_dir(session, &dir.join(RESERVED_DIR)?, |p| {
            if transient { ledger.push(Undo::RemoveDir(p.clone())) } else { Ok(()) }
        })?;
        if transient {
            for d in packed.dirs.iter().filter(|d| d.as_str() != RESERVED_DIR) {
                self.ledger.push(Undo::RemoveDir(dir.join(d)?))?;
            }
            for f in packed.files.iter().map(RelPath::as_str).chain([UNPACK_PATH, MARKER_PATH]) {
                self.ledger.push(Undo::RemoveFile(dir.join(f)?))?;
            }
        }

        // ASSUMED(gzip): `/UNZIP` refuses a corrupt stream; that is the bundle's integrity check.
        let tmp = self.path(&format!("{key}.gz"))?;
        self.ledger.push(Undo::RemoveFile(tmp.clone()))?;
        transfer::put_file(session, &a, &packed.gz, &tmp)?;
        session.cmd(&cmd::data_load_binary(&tmp, b.start(), true))?;
        let unpack = dir.join(UNPACK_PATH)?;
        let script = b.slice(0, packed.unpack_len).ok_or(VmError::ZeroLength)?;
        session.cmd(&cmd::data_save_binary(&unpack, &script, false))?;
        session.cmd(&cmd::rm(&tmp))?;

        session.cmd(&cmd::bind_path(Macro::Snap, &dir))?;
        let t1 = Instant::now();
        let (outcome, polls) = self.execute(session, &unpack, UNPACK_TIMEOUT)?;
        let unpack_time = t1.elapsed();
        if !outcome.is_success() {
            return Err(StageError::UnpackIncomplete { key: key.hex(), detail: format!("{outcome:?}") });
        }
        if !self.exists(session, &marker)? {
            return Err(StageError::UnpackIncomplete { key: key.hex(), detail: "marker missing".into() });
        }
        let upload = UploadStats {
            raw_bytes: packed.raw_len.into(),
            gz_bytes: packed.gz.len() as u64,
            elapsed: t0.elapsed(),
            unpack: unpack_time,
            polls: polls.samples,
        };
        Ok(Snapshot { key, dir, cached: false, upload: Some(upload) })
    }

    /// Run `req.entry` from the snapshot under the runner. Requires an idle PRACTICE stack
    /// (`StageError::Busy` otherwise, with nothing changed); afterwards the stack is empty again
    /// on every outcome.
    pub fn run(&mut self, session: &mut Session, snap: &Snapshot, req: &RunRequest) -> Result<RunReport, StageError> {
        let args = render_args(&req.args)?;
        self.require_idle(session)?;
        let token = self.token();
        let out = if req.collect_outputs {
            let out = self.path(&format!("out-{token}"))?;
            let ledger = &mut self.ledger;
            transfer::ensure_dir(session, &out, |p| ledger.push(Undo::RemoveDir(p.clone())))?;
            session.cmd(&cmd::bind_path(Macro::Out, &out))?;
            Some(out)
        } else {
            None
        };
        session.cmd(&cmd::bind_path(Macro::Snap, &snap.dir))?;
        session.macro_set(&Macro::Entry.name(), req.entry.as_str())?;
        session.macro_set(&Macro::Args.name(), &args)?;
        let runner = snap.dir.join(RUNNER_PATH)?;
        let t0 = Instant::now();
        let (outcome, polls) = self.execute_with(session, &runner, req.timeout, &token)?;
        Ok(RunReport { token, outcome, elapsed: t0.elapsed(), polls, out })
    }

    /// Pack the run's output directory with PowerView's `TAR`, download it, untar it, and
    /// register every listed file and directory for removal.
    pub fn fetch_outputs(&mut self, session: &mut Session, report: &RunReport) -> Result<Outputs, StageError> {
        let Some(out) = &report.out else { return Ok(Outputs::default()) };
        let archive = self.path(&format!("out-{}.tar", report.token))?;
        self.ledger.push(Undo::RemoveFile(archive.clone()))?;
        session.cmd(&cmd::tar(&archive, out))?;
        let bytes = self.download(session, &archive)?;
        let diag = tar::diagnose(&bytes);
        let files = tar::walk(&bytes)?;
        let rels: Vec<RelPath> = files.iter().filter_map(|f| RelPath::new(&f.name).ok()).collect();
        let dirs: BTreeSet<&str> = rels.iter().flat_map(RelPath::parents).collect();
        for d in dirs {
            self.ledger.push(Undo::RemoveDir(out.join(d)?))?;
        }
        for f in &rels {
            self.ledger.push(Undo::RemoveFile(out.join(f.as_str())?))?;
        }
        Ok(Outputs { files, diag })
    }

    /// Store `bytes` as `<stage>/<rel>`. The file is removed at `close`.
    pub fn upload(
        &mut self,
        session: &mut Session,
        put: Put,
        bytes: &[u8],
        rel: &str,
    ) -> Result<StagedFile, StageError> {
        if bytes.is_empty() {
            return Err(VmError::ZeroLength.into());
        }
        let dest = self.path(rel)?;
        self.ledger.push(Undo::RemoveFile(dest.clone()))?;
        match put {
            Put::Plain => {
                let r = self.claim(session, &[bytes.len() as u32])?[0];
                transfer::put_file(session, &r, bytes, &dest)?;
            }
            Put::KeepGz => {
                let gz = transfer::gzip(bytes);
                let r = self.claim(session, &[gz.len() as u32])?[0];
                transfer::put_file(session, &r, &gz, &dest)?;
            }
            Put::Inflate => {
                let gz = transfer::gzip(bytes);
                let tmp = self.path(&format!("{rel}.gz"))?;
                self.ledger.push(Undo::RemoveFile(tmp.clone()))?;
                let r = self.claim(session, &[gz.len() as u32, bytes.len() as u32])?;
                transfer::put_file(session, &r[0], &gz, &tmp)?;
                session.cmd(&cmd::data_load_binary(&tmp, r[1].start(), true))?;
                let plain = r[1].slice(0, bytes.len() as u32).ok_or(VmError::ZeroLength)?;
                session.cmd(&cmd::data_save_binary(&dest, &plain, false))?;
                session.cmd(&cmd::rm(&tmp))?;
            }
        }
        let len = transfer::file_size(session, &dest)?;
        Ok(StagedFile { path: dest, len })
    }

    /// PowerView gzips `src` (`ZIP`), the client reads the `.gz` through `VM:` and gunzips it, so
    /// no decompressed size has to be known in advance.
    pub fn download(&mut self, session: &mut Session, src: &StagePath) -> Result<Vec<u8>, StageError> {
        let gz = src.root().path(RelPath::new(&format!("{}.gz", src.rel()))?);
        self.ledger.push(Undo::RemoveFile(gz.clone()))?;
        session.cmd(&cmd::zip(src, &gz))?;
        let len = transfer::file_size(session, &gz)?;
        let len = u32::try_from(len)
            .map_err(|_| VmError::WindowTooSmall { need: len, have: self.cfg.window.len().into() })?;
        let region = self.claim(session, &[len])?[0];
        let bytes = transfer::get_file(session, &gz, &region, len)?;
        session.cmd(&cmd::rm(&gz))?;
        transfer::gunzip(&bytes)
    }

    /// Register the undo of a side effect the caller performs below the stage root, so it
    /// unwinds in order with the stage's own: files before the directories that hold them.
    /// Call before the side effect.
    pub fn register(&mut self, undo: Undo) -> Result<(), StageError> {
        self.ledger.push(undo)
    }

    /// Undos `close` will run, oldest first.
    pub fn pending(&self) -> &[Undo] {
        self.ledger.pending()
    }

    /// Run every pending undo and release the lease.
    pub fn close(self, session: &mut Session) -> CleanupReport {
        self.ledger.run(session)
    }

    /// Carve regions from the window. With `SnapshotRestore`, memory not claimed before is
    /// captured and its restore registered first.
    fn claim(&mut self, session: &mut Session, sizes: &[u32]) -> Result<Vec<VmRegion>, StageError> {
        let regions = self.cfg.window.carve(sizes)?;
        let end = regions.last().map_or(self.claimed_end, |r| r.start() + r.len());
        if self.cfg.guard == VmGuard::SnapshotRestore && end > self.claimed_end {
            let tail = NonZeroU32::new(end - self.claimed_end).expect("end above claimed_end");
            let snap = VmSnapshot::capture(session, VmRegion::new(self.claimed_end, tail)?)?;
            self.ledger.push(Undo::RestoreVm(snap))?;
            self.claimed_end = end;
        }
        Ok(regions)
    }

    fn exists(&self, session: &mut Session, p: &StagePath) -> Result<bool, StageError> {
        let e = self.cfg.exists;
        Ok(session.eval(&e.call(p))?.into_bool(e.name())?)
    }

    fn require_idle(&self, session: &mut Session) -> Result<(), StageError> {
        if session.practice_state()? != PracticeState::Idle {
            return Err(StageError::Busy { depth: -1 });
        }
        match stack_depth(session)? {
            0 => Ok(()),
            depth => Err(StageError::Busy { depth }),
        }
    }

    fn token(&self) -> String {
        #[cfg(test)]
        if let Some(t) = &self.fixed_token {
            return t.clone();
        }
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        format!("{nanos:x}{:x}{:x}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    fn execute(
        &mut self,
        session: &mut Session,
        script: &StagePath,
        timeout: Duration,
    ) -> Result<(RunOutcome, PollStats), StageError> {
        let token = self.token();
        self.execute_with(session, script, timeout, &token)
    }

    /// Token protocol around `DO <script>`; the stack is unwound on every outcome but `Finished`.
    /// The unwind is registered right before `DO` and withdrawn once the stack is seen empty, so
    /// `close` never ends a script someone else started later.
    fn execute_with(
        &mut self,
        session: &mut Session,
        script: &StagePath,
        timeout: Duration,
        token: &str,
    ) -> Result<(RunOutcome, PollStats), StageError> {
        session.macro_set(&Macro::Token.name(), token)?;
        session.macro_set(&Macro::Done.name(), "")?;
        session.cmd(cmd::ERROR_RESET)?;
        self.ledger.push(Undo::UnwindPractice)?;
        // ASSUMED(bundle): `DO` over ExecuteCommand returns while the script runs; if it returns
        // at the end instead, the first poll sees the token and the outcome is the same.
        session.cmd(&cmd::do_script(script))?;
        let mut watch = ScriptWatch::new(timeout, self.cfg.poll, Instant::now());
        loop {
            let state = session.practice_state()?;
            // `empty`: the stack was seen empty after our script, by a read or a finished unwind.
            let (outcome, empty) = match watch.on_state(Instant::now(), state) {
                Next::PollAfter(d) => {
                    thread::sleep(d);
                    continue;
                }
                Next::Stop => {
                    unwind_practice(session)?;
                    (RunOutcome::DialogAborted, true)
                }
                Next::TimedOut => {
                    unwind_practice(session)?;
                    (RunOutcome::TimedOut { last_state: state }, true)
                }
                Next::NeedIdleDetail => {
                    let done = unquote(&session.macro_get(&Macro::Done.name())?) == token;
                    let depth = stack_depth(session)?;
                    match watch.on_idle_detail(Instant::now(), done, depth) {
                        IdleVerdict::PollAfter(d) => {
                            thread::sleep(d);
                            continue;
                        }
                        IdleVerdict::Finished => (finished(session)?, depth == 0),
                        IdleVerdict::Halted => {
                            let error = halt_error(session);
                            unwind_practice(session)?;
                            (RunOutcome::Halted { error }, true)
                        }
                        IdleVerdict::Vanished => (RunOutcome::Vanished, true),
                    }
                }
            };
            if empty {
                self.ledger.withdraw_unwind()?;
            }
            return Ok((outcome, watch.stats));
        }
    }
}

/// Macro values read back over EXP may carry the quotes of the assignment.
/// ASSUMED(macro): `&m="0"` stores `0`; quotes are stripped either way.
fn unquote(s: &str) -> &str {
    s.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(s)
}

fn finished(session: &mut Session) -> Result<RunOutcome, StageError> {
    let mut get = |m: Macro| session.macro_get(&m.name()).map(|v| unquote(&v).to_string());
    let rc_text = get(Macro::Rc)?;
    let rc = rc_text
        .trim()
        .trim_end_matches('.')
        .parse()
        .map_err(|_| StageError::BadReply { what: "t32ctl_rc", text: rc_text })?;
    Ok(RunOutcome::Finished { rc, message: get(Macro::Msg)?, ret: get(Macro::Ret)? })
}

/// Best effort: a halt is reported even when the error text cannot be read.
/// ASSUMED(script-error): `EVAL ERROR.MESSAGE()` + EvalGetString reads the halting error, the
/// way pyrcl reads `ERROR.*`.
fn halt_error(session: &mut Session) -> Option<PracticeErrorInfo> {
    let message = session.eval_string(fnc::ERROR_MESSAGE).ok()?;
    let cmdline = session.eval_string(fnc::ERROR_CMDLINE).unwrap_or_default();
    (!message.is_empty() || !cmdline.is_empty()).then_some(PracticeErrorInfo { message, cmdline })
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
