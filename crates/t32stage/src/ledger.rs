// SPDX-License-Identifier: Apache-2.0
//! Cleanup as data.
//!
//! Rule: register the undo before the side effect. Every undo checks before it acts, so it
//! tolerates "never created" and running twice. Undos are a closed enum rather than closures
//! because a cleanup that cannot run must still print the exact command to type by hand.
//!
//! Undos that name PowerView files, directories, macros or the PRACTICE stack are also written
//! to a write-ahead journal on the WSL side before they are registered. A process that dies
//! leaves its journal behind; the next run replays it (`replay_leftovers`). `VM:` restores stay
//! in memory: their data is too large and too stale to replay. So do setting reverts
//! (`Undo::Revert`).

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Seek, Write},
    path::{Path, PathBuf},
};

use t32rcl::{Health, MacroName, PracticeState, Session, TargetState};

use crate::{
    error::StageError,
    practice::{ExistsFn, PvPath, RelPath, StagePath, StageRoot, cmd, fnc},
    vm::{RestoreOutcome, VmSnapshot},
};

/// How `Undo::describe` begins for a `VM:` restore; reports find restores by it.
pub const RESTORE_VM: &str = "restore VM:";

#[derive(Debug)]
#[non_exhaustive]
pub enum Undo {
    /// Exists? then `RM`, then verify gone.
    RemoveFile(StagePath),
    /// `OS.DIR`? then `RMDIR`, then verify gone. Fails when not empty: never recursive.
    RemoveDir(StagePath),
    /// Write the snapshot back and verify.
    RestoreVm(VmSnapshot),
    /// `T32_Stop` if running, then `END` while the stack is not empty. Registered after the stack
    /// was verified empty and right before our `DO`, and withdrawn once the stack is seen empty
    /// again, so it can only end scripts this process started.
    UnwindPractice,
    /// Set each macro to the empty string: macros cannot be deleted over RCL.
    ClearMacros(Vec<MacroName>),
    /// `Break` if the target runs.
    BreakTarget,
    /// PowerView commands that put back a setting the caller changed, sent in order. Never
    /// journaled: replaying it after a crash would act on a state the operator may have created
    /// since. Register it right after the change succeeded, so a refused change is never reverted.
    Revert { what: String, commands: Vec<String> },
}

impl Undo {
    pub fn describe(&self) -> String {
        match self {
            Undo::RemoveFile(p) => format!("remove file {p}"),
            Undo::RemoveDir(p) => format!("remove dir {p}"),
            Undo::RestoreVm(s) => format!("{RESTORE_VM}{:#x}+{:#x}", s.region().start(), s.region().len()),
            Undo::UnwindPractice => "unwind PRACTICE stack".to_string(),
            Undo::ClearMacros(m) => {
                format!("clear macros {}", m.iter().map(MacroName::as_str).collect::<Vec<_>>().join(","))
            }
            Undo::BreakTarget => "break target".to_string(),
            Undo::Revert { what, .. } => format!("revert {what}"),
        }
    }

    /// What an operator types into PowerView if this undo could not run, one command per line.
    pub fn manual_hint(&self) -> Option<String> {
        match self {
            Undo::RemoveFile(p) => Some(cmd::rm(p)),
            Undo::RemoveDir(p) => Some(cmd::rmdir(p)),
            Undo::UnwindPractice => Some(cmd::END.to_string()),
            Undo::BreakTarget => Some(cmd::BREAK.to_string()),
            Undo::RestoreVm(s) => {
                let clears: Vec<String> = s.uninit_runs().iter().map(cmd::clear_vm).collect();
                (!clears.is_empty()).then(|| clears.join("\n"))
            }
            Undo::ClearMacros(_) => None,
            Undo::Revert { commands, .. } => Some(commands.join("\n")),
        }
    }

    fn apply(&self, session: &mut Session, exists: ExistsFn) -> Result<CleanupOutcome, StageError> {
        match self {
            Undo::RemoveFile(p) => {
                let present = |s: &mut Session| s.eval(&exists.call(p))?.into_bool(exists.name());
                if !present(session)? {
                    return Ok(CleanupOutcome::AlreadyClean);
                }
                session.cmd(&cmd::rm(p))?;
                if present(session)? {
                    return Err(StageError::StillPresent { what: p.to_string() });
                }
                Ok(CleanupOutcome::Done)
            }
            Undo::RemoveDir(p) => {
                let present = |s: &mut Session| s.eval(&fnc::dir_exists(p))?.into_bool("OS.DIR()");
                if !present(session)? {
                    return Ok(CleanupOutcome::AlreadyClean);
                }
                session.cmd(&cmd::rmdir(p))?;
                if present(session)? {
                    return Err(StageError::StillPresent { what: p.to_string() });
                }
                Ok(CleanupOutcome::Done)
            }
            Undo::RestoreVm(s) => match s.restore(session)? {
                RestoreOutcome::Restored { uninit } => Ok(CleanupOutcome::Restored { uninit }),
                RestoreOutcome::Mismatch { first } => {
                    Ok(CleanupOutcome::Failed(format!("reads back different at offset {first:#x}")))
                }
                RestoreOutcome::Unrestored { unwritten, uncleared } => Ok(CleanupOutcome::Failed(format!(
                    "{} units not written back, {} uninitialized units not cleared",
                    unwritten.len(),
                    uncleared.len()
                ))),
            },
            Undo::UnwindPractice => unwind_practice(session),
            Undo::ClearMacros(names) => {
                for n in names {
                    session.macro_set(n, "")?;
                }
                Ok(CleanupOutcome::Done)
            }
            Undo::BreakTarget => {
                if session.target_state()? != TargetState::Running {
                    return Ok(CleanupOutcome::AlreadyClean);
                }
                session.cmd(cmd::BREAK)?;
                Ok(CleanupOutcome::Done)
            }
            Undo::Revert { commands, .. } => {
                for c in commands {
                    session.cmd(c)?;
                }
                Ok(CleanupOutcome::Done)
            }
        }
    }

    /// Why a human decides this undo when a dead run left it: replaying it would act on a state
    /// the operator may have created since (their own scripts, a target they started).
    fn left_for_a_human(&self) -> Option<&'static str> {
        match self {
            Undo::UnwindPractice => Some("check PowerView; type END if a t32ctl script is halted"),
            Undo::BreakTarget => Some("check the target; type Break if t32ctl left it running"),
            _ => None,
        }
    }

    /// Journal line, tab separated. `None` for undos that stay in memory.
    fn to_line(&self) -> Option<String> {
        let path = |p: &StagePath| format!("L{}\t{}", p.root().as_path().as_str(), p.rel());
        match self {
            Undo::RemoveFile(p) => Some(format!("rm-file\t{}", path(p))),
            Undo::RemoveDir(p) => Some(format!("rm-dir\t{}", path(p))),
            Undo::UnwindPractice => Some("unwind".to_string()),
            Undo::ClearMacros(m) => {
                Some(format!("clear-macros\t{}", m.iter().map(MacroName::as_str).collect::<Vec<_>>().join(",")))
            }
            Undo::BreakTarget => Some("break".to_string()),
            Undo::RestoreVm(_) | Undo::Revert { .. } => None,
        }
    }

    fn from_line(line: &str) -> Option<Undo> {
        let fields: Vec<&str> = line.split('\t').collect();
        let path = |root: &str, rel: &str| -> Option<StagePath> {
            let root = StageRoot::new(PvPath::parse(root.strip_prefix('L')?).ok()?);
            Some(root.path(RelPath::new(rel).ok()?))
        };
        match fields.as_slice() {
            ["rm-file", root, rel] => Some(Undo::RemoveFile(path(root, rel)?)),
            ["rm-dir", root, rel] => Some(Undo::RemoveDir(path(root, rel)?)),
            ["unwind"] => Some(Undo::UnwindPractice),
            ["clear-macros", names] => {
                names.split(',').map(|n| MacroName::new(n).ok()).collect::<Option<Vec<_>>>().map(Undo::ClearMacros)
            }
            ["break"] => Some(Undo::BreakTarget),
            _ => None,
        }
    }
}

/// `T32_Stop` if a script runs, then `END` until the stack is empty. Only for callers that saw an
/// empty stack before they started their own script.
pub fn unwind_practice(session: &mut Session) -> Result<CleanupOutcome, StageError> {
    let running = session.practice_state()? != PracticeState::Idle;
    if running {
        session.stop_practice()?;
    }
    if stack_depth(session)? == 0 && !running {
        return Ok(CleanupOutcome::AlreadyClean);
    }
    for _ in 0..3 {
        if stack_depth(session)? == 0 {
            return Ok(CleanupOutcome::Done);
        }
        session.cmd(cmd::END)?;
    }
    match stack_depth(session)? {
        0 => Ok(CleanupOutcome::Done),
        depth => Err(StageError::UnwindFailed { depth }),
    }
}

pub(crate) fn stack_depth(session: &mut Session) -> Result<i64, StageError> {
    Ok(session.eval(fnc::STACK_DEPTH)?.into_int(fnc::STACK_DEPTH)?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CleanupOutcome {
    Done,
    AlreadyClean,
    /// `VM:` restored and verified; `uninit` units were cleared again.
    Restored {
        uninit: usize,
    },
    Failed(String),
    /// The step did not run: no connection, or a journal entry left for a human.
    NotAttempted(String),
}

impl CleanupOutcome {
    pub fn is_clean(&self) -> bool {
        matches!(self, CleanupOutcome::Done | CleanupOutcome::AlreadyClean | CleanupOutcome::Restored { .. })
    }

    /// Short stable label for reports.
    pub fn label(&self) -> String {
        match self {
            CleanupOutcome::Done => "done".into(),
            CleanupOutcome::AlreadyClean => "already-clean".into(),
            CleanupOutcome::Restored { uninit: 0 } => "restored".into(),
            CleanupOutcome::Restored { uninit } => format!("restored({uninit} cleared)"),
            CleanupOutcome::Failed(why) => format!("failed: {why}"),
            CleanupOutcome::NotAttempted(why) => format!("not-attempted: {why}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupItem {
    pub what: String,
    pub outcome: CleanupOutcome,
    /// PowerView commands that undo it by hand, one per line.
    pub manual: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub items: Vec<CleanupItem>,
}

impl CleanupReport {
    /// Items that did not end clean.
    pub fn residue(&self) -> impl Iterator<Item = &CleanupItem> {
        self.items.iter().filter(|i| !i.outcome.is_clean())
    }

    pub fn is_clean(&self) -> bool {
        self.residue().next().is_none()
    }

    pub fn extend(&mut self, other: CleanupReport) {
        self.items.extend(other.items);
    }
}

/// `$XDG_STATE_HOME/t32ctl`, else `~/.local/state/t32ctl`.
pub fn default_state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("t32ctl"))
}

const JOURNAL_DIR: &str = "journal";
const JOURNAL_EXT: &str = "undo";

/// Write-ahead file of one ledger, locked by this process for its lifetime so that a replay by
/// another process skips it.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: File,
}

impl Journal {
    /// `<state_dir>/journal/<name>.undo`. `name` must be unique per process (run id, pid).
    pub fn create(state_dir: &Path, name: &str) -> Result<Self, StageError> {
        let dir = state_dir.join(JOURNAL_DIR);
        let path = dir.join(format!("{name}.{JOURNAL_EXT}"));
        let err = |source| StageError::Journal { path: path.clone(), source };
        fs::create_dir_all(&dir).map_err(err)?;
        let file = OpenOptions::new().write(true).create(true).truncate(true).open(&path).map_err(err)?;
        file.try_lock().map_err(|e| err(io::Error::other(e.to_string())))?;
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Replace the content with `lines` and flush it to disk before returning.
    fn write(&mut self, lines: &[String]) -> Result<(), StageError> {
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        let result = self
            .file
            .set_len(0)
            .and_then(|()| self.file.rewind())
            .and_then(|()| self.file.write_all(text.as_bytes()))
            .and_then(|()| self.file.sync_data());
        result.map_err(|source| StageError::Journal { path: self.path.clone(), source })
    }

    fn remove(self) -> io::Result<()> {
        fs::remove_file(&self.path)
    }
}

/// Pending undos of one scope (a probe, one `Stage`). Run on every exit path.
#[derive(Debug, Default)]
pub struct Ledger {
    pending: Vec<Undo>,
    journal: Option<Journal>,
    exists: ExistsFn,
}

impl Ledger {
    /// In memory only.
    pub fn new(exists: ExistsFn) -> Self {
        Self { pending: Vec::new(), journal: None, exists }
    }

    pub fn journaled(journal: Journal, exists: ExistsFn) -> Self {
        Self { pending: Vec::new(), journal: Some(journal), exists }
    }

    /// Register before performing the side effect. A journal write failure is returned and the
    /// caller must not act.
    pub fn push(&mut self, undo: Undo) -> Result<(), StageError> {
        if undo.to_line().is_some()
            && let Some(j) = self.journal.as_mut()
        {
            let lines: Vec<String> = self.pending.iter().chain([&undo]).filter_map(Undo::to_line).collect();
            j.write(&lines)?;
        }
        self.pending.push(undo);
        Ok(())
    }

    /// Withdraw the newest `UnwindPractice` once our script is gone from the stack. Journal first;
    /// a failed write leaves the undo pending.
    pub fn withdraw_unwind(&mut self) -> Result<(), StageError> {
        let Some(i) = self.pending.iter().rposition(|u| matches!(u, Undo::UnwindPractice)) else { return Ok(()) };
        if let Some(j) = self.journal.as_mut() {
            let lines: Vec<String> =
                self.pending.iter().enumerate().filter(|(k, _)| *k != i).filter_map(|(_, u)| u.to_line()).collect();
            j.write(&lines)?;
        }
        self.pending.remove(i);
        Ok(())
    }

    /// Apply the newest `Revert` named `what` now and drop it whatever the outcome, so `close`
    /// never sends it a second time. `AlreadyClean` when there is none.
    pub fn revert_now(&mut self, session: &mut Session, what: &str) -> CleanupOutcome {
        let newest = self.pending.iter().rposition(|u| matches!(u, Undo::Revert { what: w, .. } if w == what));
        let Some(i) = newest else { return CleanupOutcome::AlreadyClean };
        let undo = self.pending.remove(i);
        match undo.apply(session, self.exists) {
            Ok(o) => o,
            Err(e) => CleanupOutcome::Failed(e.to_string()),
        }
    }

    pub fn pending(&self) -> &[Undo] {
        &self.pending
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Run every undo newest first. Never stops at a failure. A dead session gets one revive
    /// attempt (the session enforces one per poisoning); if that fails the remaining undos are
    /// `NotAttempted` and stay in the journal for the next run.
    pub fn run(self, session: &mut Session) -> CleanupReport {
        let Ledger { pending, journal, exists } = self;
        let mut report = CleanupReport::default();
        let mut left: Vec<String> = Vec::new();
        for undo in pending.into_iter().rev() {
            let outcome = if session.health() != Health::Live
                && let Err(e) = session.try_revive()
            {
                CleanupOutcome::NotAttempted(format!("no connection: {e}"))
            } else {
                match undo.apply(session, exists) {
                    Ok(o) => o,
                    Err(e) => CleanupOutcome::Failed(e.to_string()),
                }
            };
            if !outcome.is_clean()
                && let Some(line) = undo.to_line()
            {
                left.push(line);
            }
            report.items.push(CleanupItem { what: undo.describe(), manual: undo.manual_hint(), outcome });
        }
        if let Some(mut j) = journal {
            let result = if left.is_empty() {
                let path = j.path.clone();
                j.remove().map_err(|source| StageError::Journal { path, source })
            } else {
                j.write(&left)
            };
            if let Err(e) = result {
                report.items.push(CleanupItem {
                    what: "update journal".into(),
                    outcome: CleanupOutcome::Failed(e.to_string()),
                    manual: None,
                });
            }
        }
        report
    }

    /// Drop the pending undos without running them, after the effects were proven absent.
    pub fn discard(self) {
        if let Some(j) = self.journal {
            let _ = j.remove();
        }
    }
}

/// Replay journals that dead processes left in `<state_dir>/journal`. A journal another live
/// process holds is locked and skipped. `UnwindPractice` and `BreakTarget` are reported, never
/// executed: since the crash the stack may hold the operator's own scripts and the target may run
/// because the operator started it.
pub fn replay_leftovers(session: &mut Session, state_dir: &Path, exists: ExistsFn) -> CleanupReport {
    let mut report = CleanupReport::default();
    let Ok(entries) = fs::read_dir(state_dir.join(JOURNAL_DIR)) else { return report };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == JOURNAL_EXT))
        .collect();
    paths.sort();
    for path in paths {
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else { continue };
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(_)) => continue,
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        let journal = Journal { path: path.clone(), file };
        let mut ledger = Ledger { pending: Vec::new(), journal: Some(journal), exists };
        for line in text.lines().filter(|l| !l.is_empty()) {
            match Undo::from_line(line) {
                Some(undo) => match undo.left_for_a_human() {
                    Some(why) => report.items.push(CleanupItem {
                        what: format!("{} (left by an earlier run)", undo.describe()),
                        outcome: CleanupOutcome::NotAttempted(why.into()),
                        manual: undo.manual_hint(),
                    }),
                    None => ledger.pending.push(undo),
                },
                None => report.items.push(CleanupItem {
                    what: format!("journal line {line:?} in {}", path.display()),
                    outcome: CleanupOutcome::NotAttempted("unreadable".into()),
                    manual: None,
                }),
            }
        }
        report.extend(ledger.run(session));
    }
    report
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
