// SPDX-License-Identifier: Apache-2.0
use std::time::Duration;

use t32rcl::testkit::{ConnectFailure, Request, Script};

use super::*;
use crate::{
    practice::RelPath,
    testutil::{session, session_with, temp_dir},
};

fn root() -> StageRoot {
    StageRoot::from_ptd("C:\\T")
}

fn file(rel: &str) -> StagePath {
    root().path(RelPath::new(rel).unwrap())
}

fn readable(s: Script, rel: &str, v: bool) -> Script {
    s.expect_fn(&format!("OS.FILE.readable(\"C:\\T\\t32ctl\\{}\")", rel.replace('/', "\\"))).reply_bool(v)
}

#[test]
fn undos_run_newest_first_and_continue_after_a_failure() {
    let mut s = Script::new().attach().expect_req(Request::TargetState).reply_state(2);
    s = s.expect_req(Request::MacroSet { name: "t32ctl_rc".into(), value: String::new() }).reply_macro("");
    s = s.expect_fn("OS.DIR(\"C:\\T\\t32ctl\\d\")").reply_bool(true);
    s = s.expect_cmd("RMDIR \"C:\\T\\t32ctl\\d\"").reply_error(90, "directory not empty");
    s = readable(s, "d/a.bin", true).expect_cmd("RM \"C:\\T\\t32ctl\\d\\a.bin\"").reply_ok();
    s = readable(s, "d/a.bin", false);
    let mut session = session(s);

    let mut ledger = Ledger::new(ExistsFn::OsFileReadable);
    ledger.push(Undo::RemoveFile(file("d/a.bin"))).unwrap();
    ledger.push(Undo::RemoveDir(file("d"))).unwrap();
    ledger.push(Undo::ClearMacros(vec![MacroName::new("t32ctl_rc").unwrap()])).unwrap();
    ledger.push(Undo::BreakTarget).unwrap();
    let report = ledger.run(&mut session);

    let outcomes: Vec<_> = report.items.iter().map(|i| i.outcome.label()).collect();
    assert_eq!(outcomes[0], "already-clean");
    assert_eq!(outcomes[1], "done");
    assert!(outcomes[2].starts_with("failed: "), "{}", outcomes[2]);
    assert_eq!(outcomes[3], "done");
    let residue: Vec<_> = report.residue().collect();
    assert_eq!(residue.len(), 1);
    assert_eq!(residue[0].manual.as_deref(), Some("RMDIR \"C:\\T\\t32ctl\\d\""));
}

#[test]
fn a_dead_session_gets_one_revive_then_undos_are_not_attempted_with_hints() {
    let mut session = session_with(
        vec![
            Ok(Script::new().attach().expect_cmd("WAIT 9.s").silence(Duration::from_secs(2))),
            Err(ConnectFailure::Refused),
        ],
        |_| {},
    );
    assert!(session.cmd("WAIT 9.s").is_err());
    let mut ledger = Ledger::new(ExistsFn::OsFileReadable);
    ledger.push(Undo::RemoveFile(file("x.bin"))).unwrap();
    ledger.push(Undo::UnwindPractice).unwrap();
    let report = ledger.run(&mut session);
    assert!(report.items.iter().all(|i| matches!(i.outcome, CleanupOutcome::NotAttempted(_))));
    let hints: Vec<_> = report.items.iter().map(|i| i.manual.clone().unwrap()).collect();
    assert_eq!(hints, ["END", "RM \"C:\\T\\t32ctl\\x.bin\""]);
}

#[test]
fn the_journal_is_written_before_the_effect_and_removed_once_clean() {
    let dir = temp_dir("journal-clean");
    let mut ledger = Ledger::journaled(Journal::create(&dir, "run-a").unwrap(), ExistsFn::OsFileReadable);
    let path = dir.join("journal/run-a.undo");
    ledger.push(Undo::RemoveFile(file("k/a.bin"))).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "rm-file\tLC:\\T\\t32ctl\tk/a.bin\n");
    ledger.push(Undo::UnwindPractice).unwrap();
    ledger.push(Undo::ClearMacros(vec![MacroName::new("t32ctl_rc").unwrap()])).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 3);

    let mut s = Script::new().attach();
    s = s.expect_req(Request::MacroSet { name: "t32ctl_rc".into(), value: String::new() }).reply_macro("");
    s = s.expect_req(Request::PracticeState).reply_state(0).expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    s = readable(s, "k/a.bin", false);
    let report = ledger.run(&mut session(s));
    assert!(report.is_clean(), "{report:?}");
    assert!(!path.exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn residue_stays_journaled_and_the_next_run_replays_it_but_skips_live_journals() {
    let dir = temp_dir("journal-replay");
    let mut ledger = Ledger::journaled(Journal::create(&dir, "dead").unwrap(), ExistsFn::OsFileReadable);
    ledger.push(Undo::RemoveFile(file("k/a.bin"))).unwrap();
    ledger.push(Undo::UnwindPractice).unwrap();
    let s = Script::new().attach().expect_req(Request::PracticeState).reply_state(0);
    let s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    let s = readable(s, "k/a.bin", true).expect_cmd("RM \"C:\\T\\t32ctl\\k\\a.bin\"").reply_error(90, "in use");
    let report = ledger.run(&mut session(s));
    assert_eq!(report.residue().count(), 1);
    let dead = dir.join("journal/dead.undo");
    assert_eq!(fs::read_to_string(&dead).unwrap(), "rm-file\tLC:\\T\\t32ctl\tk/a.bin\n");

    let mut live = Ledger::journaled(Journal::create(&dir, "live").unwrap(), ExistsFn::OsFileReadable);
    live.push(Undo::RemoveFile(file("other.bin"))).unwrap();

    let s = readable(Script::new().attach(), "k/a.bin", true).expect_cmd("RM \"C:\\T\\t32ctl\\k\\a.bin\"").reply_ok();
    let s = readable(s, "k/a.bin", false);
    let report = replay_leftovers(&mut session(s), &dir, ExistsFn::OsFileReadable);
    assert_eq!(report.items.len(), 1);
    assert!(report.is_clean());
    assert!(!dead.exists());
    assert!(dir.join("journal/live.undo").exists());
    live.discard();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_crashed_unwind_is_reported_not_executed_on_replay() {
    let dir = temp_dir("journal-unwind");
    let mut ledger = Ledger::journaled(Journal::create(&dir, "crashed").unwrap(), ExistsFn::OsFileReadable);
    ledger.push(Undo::UnwindPractice).unwrap();
    ledger.push(Undo::RemoveDir(StageRoot::Macro.path(RelPath::new("probe-1").unwrap()))).unwrap();
    drop(ledger);

    let s = Script::new().attach().expect_cmd("GLOBAL &t32ctl_root").reply_ok();
    let s = s.expect_cmd("&t32ctl_root=OS.PTD()+\"\\t32ctl\"").reply_ok();
    let s = s.expect_fn("OS.DIR(\"&t32ctl_root\\probe-1\")").reply_bool(false);
    let report = replay_leftovers(&mut session(s), &dir, ExistsFn::OsFileReadable);
    let labels: Vec<_> = report.items.iter().map(|i| i.outcome.label()).collect();
    assert!(labels[0].starts_with("not-attempted"), "{labels:?}");
    assert_eq!(labels[1], "already-clean");
    let _ = fs::remove_dir_all(dir);
}
