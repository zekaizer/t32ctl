// SPDX-License-Identifier: Apache-2.0
use std::time::Duration;

use t32rcl::testkit::{ConnectFailure, Request, Script};

use super::*;
use crate::{
    practice::RelPath,
    testutil::{session, session_with, temp_dir},
};

fn root() -> StageRoot {
    StageRoot::from_ptd("C:\\T").unwrap()
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
    s = s.expect_req(Request::MacroSet { name: "&t32ctl_rc".into(), value: String::new() }).reply_macro("");
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
    s = s.expect_req(Request::MacroSet { name: "&t32ctl_rc".into(), value: String::new() }).reply_macro("");
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
    ledger.push(Undo::RemoveDir(file("probe-1"))).unwrap();
    drop(ledger);

    let s = Script::new().attach().expect_fn("OS.DIR(\"C:\\T\\t32ctl\\probe-1\")").reply_bool(false);
    let report = replay_leftovers(&mut session(s), &dir, ExistsFn::OsFileReadable);
    let labels: Vec<_> = report.items.iter().map(|i| i.outcome.label()).collect();
    assert!(labels[0].starts_with("not-attempted"), "{labels:?}");
    assert_eq!(labels[1], "already-clean");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_crashed_break_is_reported_not_executed_on_replay() {
    let dir = temp_dir("journal-break");
    let mut ledger = Ledger::journaled(Journal::create(&dir, "crashed").unwrap(), ExistsFn::OsFileReadable);
    ledger.push(Undo::BreakTarget).unwrap();
    drop(ledger);

    let report = replay_leftovers(&mut session(Script::new().attach()), &dir, ExistsFn::OsFileReadable);
    assert_eq!(report.items.len(), 1, "{report:?}");
    assert!(matches!(report.items[0].outcome, CleanupOutcome::NotAttempted(_)), "{report:?}");
    assert_eq!(report.items[0].manual.as_deref(), Some("Break"));
    assert!(!dir.join("journal/crashed.undo").exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_withdrawn_unwind_leaves_memory_and_the_journal() {
    let dir = temp_dir("journal-withdraw");
    let mut ledger = Ledger::journaled(Journal::create(&dir, "run").unwrap(), ExistsFn::OsFileReadable);
    ledger.push(Undo::RemoveFile(file("k/a.bin"))).unwrap();
    ledger.push(Undo::UnwindPractice).unwrap();
    ledger.withdraw_unwind().unwrap();
    assert_eq!(fs::read_to_string(dir.join("journal/run.undo")).unwrap(), "rm-file\tLC:\\T\\t32ctl\tk/a.bin\n");
    assert!(matches!(ledger.pending(), [Undo::RemoveFile(_)]));
    ledger.discard();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn unwind_proceeds_when_t32_stop_answers_status_1() {
    let s = Script::new().attach().expect_req(Request::PracticeState).reply_state(1);
    let s = s.expect_req(Request::Stop).reply_error(1, "");
    let s = s.expect_fn("PRACTICE.StackDepth()").reply_decimal(1).expect_fn("PRACTICE.StackDepth()").reply_decimal(1);
    let s = s.expect_cmd("END").reply_ok().expect_fn("PRACTICE.StackDepth()").reply_decimal(0);
    assert_eq!(unwind_practice(&mut session(s)).unwrap(), CleanupOutcome::Done);
}

fn revert(what: &str, commands: &[&str]) -> Undo {
    Undo::Revert { what: what.into(), commands: commands.iter().map(|c| c.to_string()).collect() }
}

#[test]
fn revert_is_never_journaled_and_runs_newest_first() {
    let dir = temp_dir("journal-revert");
    let mut ledger = Ledger::journaled(Journal::create(&dir, "run").unwrap(), ExistsFn::OsFileReadable);
    ledger.push(revert("core", &["CORE.select 0", "SYStem.Mode Attach"])).unwrap();
    ledger.push(revert("area", &["AREA.Delete T32PQ1A2B"])).unwrap();
    assert_eq!(fs::read_to_string(dir.join("journal/run.undo")).unwrap(), "");

    let s = Script::new().attach().expect_cmd("AREA.Delete T32PQ1A2B").reply_ok();
    let s = s.expect_cmd("CORE.select 0").reply_ok().expect_cmd("SYStem.Mode Attach").reply_ok();
    let report = ledger.run(&mut session(s));
    assert!(report.is_clean(), "{report:?}");
    let what: Vec<_> = report.items.iter().map(|i| i.what.as_str()).collect();
    assert_eq!(what, ["revert area", "revert core"]);
    assert!(!dir.join("journal/run.undo").exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_failed_revert_prints_its_commands_as_the_manual_hint() {
    let mut ledger = Ledger::new(ExistsFn::OsFileReadable);
    ledger.push(revert("bp", &["Break.Delete main", "Break.Delete sub"])).unwrap();
    let s = Script::new().attach().expect_cmd("Break.Delete main").reply_error(90, "no breakpoint");
    let report = ledger.run(&mut session(s));
    assert!(matches!(report.items[0].outcome, CleanupOutcome::Failed(_)), "{report:?}");
    assert_eq!(report.items[0].manual.as_deref(), Some("Break.Delete main\nBreak.Delete sub"));
}

#[test]
fn revert_now_applies_and_removes_only_that_undo() {
    let mut ledger = Ledger::new(ExistsFn::OsFileReadable);
    ledger.push(Undo::RemoveFile(file("x.bin"))).unwrap();
    ledger.push(revert("a", &["A.old"])).unwrap();
    ledger.push(revert("a", &["A.new"])).unwrap();
    ledger.push(revert("b", &["B"])).unwrap();
    let s = Script::new().attach().expect_cmd("A.new").reply_ok().expect_cmd("A.old").reply_error(90, "refused");
    let mut session = session(s);

    assert_eq!(ledger.revert_now(&mut session, "a"), CleanupOutcome::Done);
    assert!(matches!(ledger.revert_now(&mut session, "a"), CleanupOutcome::Failed(_)));
    assert_eq!(ledger.revert_now(&mut session, "a"), CleanupOutcome::AlreadyClean);
    let left: Vec<_> = ledger.pending().iter().map(Undo::describe).collect();
    assert_eq!(left, ["remove file C:\\T\\t32ctl\\x.bin", "revert b"]);
    ledger.discard();
}

/// `OS.FIRSTFILE` and `OS.NEXTFILE` over `C:\T\t32ctl\<rel>`: `.`, `..`, `names`, the end.
fn listed(s: Script, rel: &str, names: &[&str]) -> Script {
    let dir = format!("C:\\T\\t32ctl\\{}", rel.replace('/', "\\"));
    let mut s = s.expect_fn(&format!("OS.FIRSTFILE(\"{dir}\\*\")")).reply_text(&format!("{dir}\\."));
    for n in [".."].iter().chain(names).chain(&[""]) {
        s = s.expect_fn("OS.NEXTFILE()").reply_text(n);
    }
    s
}

fn is_dir(s: Script, rel: &str, v: bool) -> Script {
    s.expect_fn(&format!("OS.DIR(\"C:\\T\\t32ctl\\{}\")", rel.replace('/', "\\"))).reply_bool(v)
}

#[test]
fn a_run_dir_is_emptied_deepest_first_then_removed() {
    let mut s = listed(is_dir(Script::new().attach(), "out-tok", true), "out-tok", &["a.log", "sub"]);
    s = is_dir(s, "out-tok/a.log", false);
    s = readable(s, "out-tok/a.log", true).expect_cmd("RM \"C:\\T\\t32ctl\\out-tok\\a.log\"").reply_ok();
    s = readable(s, "out-tok/a.log", false);
    s = listed(is_dir(s, "out-tok/sub", true), "out-tok/sub", &["b.bin"]);
    s = is_dir(s, "out-tok/sub/b.bin", false);
    s = readable(s, "out-tok/sub/b.bin", true).expect_cmd("RM \"C:\\T\\t32ctl\\out-tok\\sub\\b.bin\"").reply_ok();
    s = readable(s, "out-tok/sub/b.bin", false);
    s = is_dir(s, "out-tok/sub", true).expect_cmd("RMDIR \"C:\\T\\t32ctl\\out-tok\\sub\"").reply_ok();
    s = is_dir(s, "out-tok/sub", false);
    s = s.expect_cmd("RMDIR \"C:\\T\\t32ctl\\out-tok\"").reply_ok();
    s = is_dir(s, "out-tok", false);

    let mut ledger = Ledger::new(ExistsFn::OsFileReadable);
    ledger.push(Undo::RemoveRunDir(file("out-tok"))).unwrap();
    let report = ledger.run(&mut session(s));
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].what, "remove run dir C:\\T\\t32ctl\\out-tok");
}

#[test]
fn what_a_run_dir_keeps_is_named_in_the_hint_before_its_rmdir_and_stays_journaled() {
    let dir = temp_dir("journal-run-dir");
    let mut s = listed(is_dir(Script::new().attach(), "out-tok", true), "out-tok", &["a.log", "b.log"]);
    s = is_dir(s, "out-tok/a.log", false);
    s = readable(s, "out-tok/a.log", true).expect_cmd("RM \"C:\\T\\t32ctl\\out-tok\\a.log\"").reply_error(90, "in use");
    s = is_dir(s, "out-tok/b.log", false);
    s = readable(s, "out-tok/b.log", true).expect_cmd("RM \"C:\\T\\t32ctl\\out-tok\\b.log\"").reply_ok();
    s = readable(s, "out-tok/b.log", false);
    s = s.expect_cmd("RMDIR \"C:\\T\\t32ctl\\out-tok\"").reply_error(90, "critical error accessing file");

    let mut ledger = Ledger::journaled(Journal::create(&dir, "run").unwrap(), ExistsFn::OsFileReadable);
    ledger.push(Undo::RemoveRunDir(file("out-tok"))).unwrap();
    let report = ledger.run(&mut session(s));
    assert!(matches!(report.items[0].outcome, CleanupOutcome::Failed(_)), "{report:?}");
    assert_eq!(
        report.items[0].manual.as_deref(),
        Some("RM \"C:\\T\\t32ctl\\out-tok\\a.log\"\nRMDIR \"C:\\T\\t32ctl\\out-tok\"")
    );
    assert_eq!(fs::read_to_string(dir.join("journal/run.undo")).unwrap(), "rm-run-dir\tLC:\\T\\t32ctl\tout-tok\n");
    assert!(
        matches!(Undo::from_line("rm-run-dir\tLC:\\T\\t32ctl\tout-tok"), Some(Undo::RemoveRunDir(p)) if p == file("out-tok"))
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn manual_commands_are_typeable() {
    assert_eq!(Undo::RemoveFile(file("x y.bin")).manual_hint().as_deref(), Some("RM \"C:\\T\\t32ctl\\x y.bin\""));
}

#[test]
fn a_macro_root_line_from_an_older_build_is_reported_not_replayed() {
    let dir = temp_dir("journal-macro-root");
    let journal = dir.join("journal");
    fs::create_dir_all(&journal).unwrap();
    fs::write(journal.join("old.undo"), "rm-dir\tM\tprobe-1\n").unwrap();
    let report = replay_leftovers(&mut session(Script::new().attach()), &dir, ExistsFn::OsFileReadable);
    assert_eq!(report.items.len(), 1, "{report:?}");
    assert_eq!(report.items[0].outcome, CleanupOutcome::NotAttempted("unreadable".into()));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_per_powerview_directory_keys_the_host_as_configured() {
    let base = Path::new("/s/t32ctl");
    assert_eq!(pv_state_dir(base, "auto", 20000), Path::new("/s/t32ctl/pv/auto-20000"));
    assert_eq!(pv_state_dir(base, "127.0.0.1", 20001), Path::new("/s/t32ctl/pv/127.0.0.1-20001"));
    assert_eq!(pv_state_dir(base, "my-pc.local", 1), Path::new("/s/t32ctl/pv/my-pc.local-1"));
}

#[test]
fn bytes_a_file_name_cannot_carry_become_underscores() {
    let base = Path::new("/s");
    assert_eq!(pv_state_dir(base, "../x", 20000), Path::new("/s/pv/.._x-20000"));
    assert_eq!(pv_state_dir(base, "fe80::1%eth0", 20000), Path::new("/s/pv/fe80__1_eth0-20000"));
    assert_eq!(pv_state_dir(base, "h\\ö", 20000), Path::new("/s/pv/h___-20000"));
}
