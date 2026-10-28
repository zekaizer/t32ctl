// SPDX-License-Identifier: Apache-2.0
//! Rendering: two projections of the same outcomes, the paste-back text and the JSON.
//!
//! Text contract:
//! - every line but a `RESIDUE` command is at most `WIDTH` columns; a long line is cut at a
//!   token boundary with `…`;
//! - one line per probe: `<id:12> <TAG:5> <tokens>`, tokens are `key=value` with units;
//! - a FAIL adds at most two indented lines (`why:`, `hint:`); consecutive SKIPs with one
//!   reason collapse into `skipped(n) <reason>: <ids>`;
//! - `RESIDUE <probe>: <command>` lines carry the exact PowerView command to type by hand,
//!   one per line and uncut; a residue without one reads `RESIDUE <probe>: <what> (<result>)`;
//! - a `config` line with the tunables Phase 1 pins, then a `summary` line.

use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use t32rcl::PowerViewBuild;
use t32stage::CleanupReport;

use crate::{
    env::{Env, WslFacts},
    exec::RunResult,
    outcome::{CleanupRecord, Outcome, Value, Verdict, cleanup_records, exit_code, key},
    registry::ProbeId,
};

pub const WIDTH: usize = 100;

/// `8.1MB/s`, `1MiB`, `0.31ms`, `3.00s`, `yes`: the only value formatter.
pub fn format_value(v: &Value) -> String {
    match v {
        Value::Count(n) => n.to_string(),
        Value::Bytes(b) => {
            let b = *b;
            if b >= 1 << 20 && b % (1 << 20) == 0 {
                format!("{}MiB", b >> 20)
            } else if b >= 1 << 20 {
                format!("{:.1}MiB", b as f64 / f64::from(1 << 20))
            } else if b >= 1024 && b % 1024 == 0 {
                format!("{}KiB", b >> 10)
            } else if b >= 1024 {
                format!("{:.1}KiB", b as f64 / 1024.0)
            } else {
                format!("{b}B")
            }
        }
        Value::Seconds(s) => {
            let s = *s;
            if s < 1e-3 {
                format!("{:.0}us", s * 1e6)
            } else if s < 1.0 {
                format!("{:.2}ms", s * 1e3)
            } else {
                format!("{s:.2}s")
            }
        }
        Value::MbPerSec(r) => format!("{r:.1}MB/s"),
        Value::Flag(b) => (if *b { "yes" } else { "no" }).to_string(),
        Value::Text(t) => t.clone(),
    }
}

/// Append tokens while they fit; a token that does not fit ends the line with `…`.
fn fit(prefix: String, tokens: impl IntoIterator<Item = String>, width: usize) -> String {
    let mut line = prefix;
    for t in tokens {
        let sep = usize::from(!line.ends_with(' ') && !line.is_empty());
        if line.chars().count() + sep + t.chars().count() > width {
            if line.chars().count() < width {
                line.push('…');
            }
            return line;
        }
        if sep == 1 {
            line.push(' ');
        }
        line.push_str(&t);
    }
    line
}

/// Split long free text into tokens at spaces so `fit` can cut it cleanly.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

fn ms(d: Duration) -> String {
    format_value(&Value::Seconds(d.as_secs_f64()))
}

/// Headline measurements, the reconnect, VM restores and the residue count, in line order.
fn tokens(o: &Outcome) -> Vec<String> {
    let mut tokens: Vec<String> =
        o.measurements.iter().filter(|m| m.headline).map(|m| format!("{}={}", m.key, format_value(&m.value))).collect();
    if let Some(r) = &o.revive {
        tokens.push(format!("revive={}", if r.ok { ms(r.connect) } else { "FAILED".into() }));
        if let Some(f) = r.first_call {
            tokens.push(format!("first_call={}", ms(f)));
        }
    }
    for c in o.cleanup.iter().filter(|c| c.what.starts_with(t32stage::RESTORE_VM)) {
        tokens.push(format!("restore={}", if c.clean { "ok" } else { "FAILED" }));
    }
    let residue = o.residue().count();
    if residue > 0 {
        tokens.push(format!("residue={residue}"));
    }
    tokens
}

/// A line without tokens carries the error or skip reason itself; otherwise `render_extra` adds
/// it below.
pub fn render_line(o: &Outcome, width: usize) -> String {
    let prefix = format!("{:<12} {:<5} ", o.probe.name(), o.verdict.tag());
    let mut tokens = tokens(o);
    if tokens.is_empty() {
        if let Some(e) = &o.error {
            tokens = words(&e.message);
        } else if let Some(s) = &o.skip {
            tokens = words(&s.text());
        }
    }
    fit(prefix, tokens, width)
}

/// `why:` and `hint:` for a failure; at most two lines.
pub fn render_extra(o: &Outcome, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(e) = &o.error
        && matches!(o.verdict, Verdict::Fail | Verdict::Unsupported | Verdict::Interrupted)
    {
        if !tokens(o).is_empty() {
            out.push(fit("  why:".into(), words(&e.message), width));
        }
        if let Some(h) = &e.hint {
            out.push(fit("  hint:".into(), words(h), width));
        }
    }
    out.truncate(2);
    out
}

/// Values Phase 1 pins, one key per tunable. `None` means "not measured".
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Config {
    pub read_chunk: Option<u64>,
    pub write_chunk: Option<u64>,
    pub macro_spelling: Option<String>,
    pub keepalive: Option<String>,
    pub exists_fn: Option<String>,
    pub stage_root: Option<String>,
    pub script_out: Option<String>,
}

fn find<'a>(outcomes: &'a [Outcome], probe: ProbeId, key: &str) -> Option<&'a Value> {
    outcomes.iter().find(|o| o.probe == probe && o.verdict == Verdict::Pass)?.lookup(key)
}

pub fn config(outcomes: &[Outcome]) -> Config {
    let count = |p, k| match find(outcomes, p, k) {
        Some(Value::Count(n) | Value::Bytes(n)) => Some(*n),
        _ => None,
    };
    let text = |p, k| match find(outcomes, p, k) {
        Some(Value::Text(t)) => Some(t.clone()),
        _ => None,
    };
    Config {
        read_chunk: count(ProbeId::VmRw, key::CHUNK_R),
        write_chunk: count(ProbeId::VmRw, key::CHUNK_W),
        macro_spelling: text(ProbeId::Macro, key::SPELLING),
        keepalive: find(outcomes, ProbeId::Keepalive, key::KEEPALIVE_INTERVAL).map(format_value),
        exists_fn: text(ProbeId::Tempdir, key::EXISTS),
        stage_root: text(ProbeId::Tempdir, key::ROOT),
        script_out: text(ProbeId::ScriptLab, key::SCRIPT_OUT),
    }
}

fn config_line(c: &Config) -> String {
    let q = |v: Option<String>| v.unwrap_or_else(|| "?".into());
    let tokens = [
        format!("read_chunk={}", q(c.read_chunk.map(|n| n.to_string()))),
        format!("write_chunk={}", q(c.write_chunk.map(|n| n.to_string()))),
        format!("macro={}", q(c.macro_spelling.clone())),
        format!("keepalive={}", q(c.keepalive.clone())),
        format!("exists={}", q(c.exists_fn.clone())),
        format!("stage_root={}", q(c.stage_root.clone())),
        format!("script_out={}", q(c.script_out.clone())),
    ];
    fit("config      ".into(), tokens, WIDTH)
}

pub fn header(env: &Env, build: Option<PowerViewBuild>) -> String {
    let pv = match build {
        Some(b) => format!("PowerView build={b} {}", b.release_hint().unwrap_or_default()),
        None => "PowerView build=?".to_string(),
    };
    let target = match &env.endpoint {
        Some(ep) => ep.to_string(),
        None => format!("tried {}:{}", env.cfg.hosts.join(","), env.cfg.port),
    };
    let tokens = words(&pv).into_iter().chain([
        target,
        format!("run={}", env.cfg.run_id),
        format!("wsl={}", env.cfg.wsl.mode.tag()),
    ]);
    fit(format!("t32probe {} ", env!("T32PROBE_VERSION")), tokens, WIDTH)
}

/// One line per manual command, never cut: the operator types it, and a cut command is worse
/// than a long line. A residue without a command gets what and why, cut to the width.
fn residue_lines(label: &str, records: &[CleanupRecord]) -> Vec<String> {
    records
        .iter()
        .filter(|c| !c.clean)
        .flat_map(|c| match c.manual.as_deref() {
            Some(m) => m.lines().map(|cmd| format!("RESIDUE {label}: {cmd}")).collect(),
            None => vec![fit(format!("RESIDUE {label}: "), words(&format!("{} ({})", c.what, c.result)), WIDTH)],
        })
        .collect()
}

/// The paste-back report.
pub fn render_text(
    run: &RunResult,
    env: &Env,
    build: Option<PowerViewBuild>,
    not_run: &[ProbeId],
    elapsed: Duration,
) -> String {
    let mut lines = vec![header(env, build)];
    let replay = cleanup_records(&run.replay);
    if !replay.is_empty() {
        let left = replay.iter().filter(|c| !c.clean).count();
        lines.push(format!("residue     earlier runs: undone={} left={left}", replay.len() - left));
        lines.extend(residue_lines("earlier", &replay));
    }
    let mut i = 0;
    let outcomes = &run.outcomes;
    while i < outcomes.len() {
        let o = &outcomes[i];
        if o.verdict == Verdict::Skipped {
            let why = o.skip.as_ref().map(|s| s.text()).unwrap_or_default();
            let mut j = i;
            while j < outcomes.len()
                && outcomes[j].verdict == Verdict::Skipped
                && outcomes[j].skip.as_ref().map(|s| s.text()).unwrap_or_default() == why
            {
                j += 1;
            }
            if j - i > 1 {
                let ids = outcomes[i..j].iter().map(|o| o.probe.name().to_string());
                lines.push(fit(format!("skipped({}) {why}: ", j - i), ids, WIDTH));
                i = j;
                continue;
            }
        }
        lines.push(render_line(o, WIDTH));
        lines.extend(render_extra(o, WIDTH));
        i += 1;
    }
    for o in outcomes {
        lines.extend(residue_lines(o.probe.name(), &o.cleanup));
    }
    if !not_run.is_empty() {
        lines.push(fit("not run (opt-in):".into(), not_run.iter().map(|p| p.name().to_string()), WIDTH));
    }
    lines.push(config_line(&config(outcomes)));
    let n = |v: Verdict| outcomes.iter().filter(|o| o.verdict == v).count();
    let residue: usize = outcomes.iter().map(|o| o.residue().count()).sum();
    let cleanup = if residue == 0 { "clean".to_string() } else { format!("residue({residue})") };
    lines.push(format!(
        "summary: pass={} fail={} unsup={} skip={} intr={} | cleanup={cleanup} | {:.1}s | exit={}",
        n(Verdict::Pass),
        n(Verdict::Fail),
        n(Verdict::Unsupported),
        n(Verdict::Skipped),
        n(Verdict::Interrupted),
        elapsed.as_secs_f64(),
        exit_code(outcomes, run.interrupted)
    ));
    lines.join("\n") + "\n"
}

#[derive(Serialize)]
pub struct JsonReport<'a> {
    pub schema: &'static str,
    pub tool: &'static str,
    pub version: &'static str,
    pub environment: Environment<'a>,
    pub replay: Vec<CleanupRecord>,
    pub probes: &'a [Outcome],
    pub config: Config,
    pub exit_code: u8,
    pub interrupted: bool,
}

#[derive(Serialize)]
pub struct Environment<'a> {
    pub run_id: &'a str,
    pub endpoint: Option<String>,
    pub hosts: &'a [String],
    pub port: u16,
    pub wsl: &'a WslFacts,
    pub build: Option<u32>,
    pub build_base: Option<u32>,
    pub state_dir: String,
    pub unix_ms: u64,
}

pub fn json_report<'a>(
    env: &'a Env,
    outcomes: &'a [Outcome],
    replay: &CleanupReport,
    build: Option<PowerViewBuild>,
    interrupted: bool,
) -> JsonReport<'a> {
    JsonReport {
        schema: "t32probe.report/1",
        tool: "t32probe",
        version: env!("T32PROBE_VERSION"),
        environment: Environment {
            run_id: &env.cfg.run_id,
            endpoint: env.endpoint.as_ref().map(|e| e.to_string()),
            hosts: &env.cfg.hosts,
            port: env.cfg.port,
            wsl: &env.cfg.wsl,
            build: build.map(|b| b.build),
            build_base: build.and_then(|b| b.base),
            state_dir: env.cfg.state_dir.display().to_string(),
            unix_ms: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
        },
        replay: cleanup_records(replay),
        probes: outcomes,
        config: config(outcomes),
        exit_code: exit_code(outcomes, interrupted),
        interrupted,
    }
}

/// Where `write_json_atomic` writes before it renames.
pub fn json_tmp(path: &Path) -> std::path::PathBuf {
    path.with_extension("tmp")
}

/// Write `<path>.tmp`, then rename: a reader never sees half a file.
pub fn write_json_atomic(path: &Path, doc: &JsonReport<'_>) -> std::io::Result<()> {
    let tmp = json_tmp(path);
    let mut f = fs::File::create(&tmp)?;
    f.write_all(&serde_json::to_vec_pretty(doc).map_err(std::io::Error::other)?)?;
    f.sync_data()?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{ErrorRecord, Measurement, SkipReason};

    fn line(o: &Outcome) -> String {
        render_line(o, WIDTH)
    }

    #[test]
    fn values_have_one_format_with_units() {
        let cases = [
            (Value::Bytes(1 << 20), "1MiB"),
            (Value::Bytes(1536), "1.5KiB"),
            (Value::Bytes(4097), "4.0KiB"),
            (Value::Bytes(7), "7B"),
            (Value::Seconds(0.00031), "310us"),
            (Value::Seconds(0.0123), "12.30ms"),
            (Value::Seconds(3.0), "3.00s"),
            (Value::MbPerSec(8.123), "8.1MB/s"),
            (Value::Flag(true), "yes"),
            (Value::Count(42), "42"),
        ];
        for (v, want) in cases {
            assert_eq!(format_value(&v), want);
        }
    }

    #[test]
    fn lines_never_exceed_the_width_and_never_split_a_token() {
        let mut o = Outcome::new(ProbeId::Macro, Verdict::Pass);
        for i in 0..40 {
            o.measurements.push(Measurement {
                key: format!("key{i}"),
                value: Value::Text("v".repeat(i)),
                headline: true,
            });
        }
        let l = line(&o);
        assert!(l.chars().count() <= WIDTH, "{l}");
        assert!(l.ends_with('…'));
        let last = l.trim_end_matches('…').split(' ').next_back().unwrap();
        assert!(last.starts_with("key") && last.contains('='), "cut at a token boundary: {last}");
        assert_eq!(&l[..19], "macro        PASS  ");
    }

    #[test]
    fn a_failure_shows_its_message_inline_and_its_hint_below() {
        let mut o = Outcome::new(ProbeId::Connect, Verdict::Fail);
        o.error = Some(ErrorRecord {
            hint: Some("PowerView not running".into()),
            ..ErrorRecord::new("refused", "pv:20000: connection refused")
        });
        assert_eq!(line(&o), "connect      FAIL  pv:20000: connection refused");
        assert_eq!(render_extra(&o, WIDTH), ["  hint: PowerView not running"]);
    }

    #[test]
    fn a_failure_with_only_restore_tokens_still_shows_its_message() {
        let mut o = Outcome::new(ProbeId::Bundle, Verdict::Fail);
        o.error = Some(ErrorRecord::new("remote", "command: PowerView refused: DO failed"));
        o.cleanup.push(CleanupRecord {
            what: format!("{} 0x01000000++0xFFF", t32stage::RESTORE_VM),
            result: "clean".into(),
            clean: true,
            manual: None,
        });
        assert_eq!(line(&o), "bundle       FAIL  restore=ok");
        assert_eq!(render_extra(&o, WIDTH), ["  why: command: PowerView refused: DO failed"]);
    }

    #[test]
    fn consecutive_skips_with_one_reason_collapse() {
        let reason = SkipReason::Prereq { probe: "connect".into(), verdict: Verdict::Fail };
        let mut outcomes = vec![Outcome::new(ProbeId::Connect, Verdict::Fail)];
        for p in [ProbeId::Version, ProbeId::Latency, ProbeId::Clients] {
            outcomes.push(Outcome::skipped(p, reason.clone()));
        }
        let run = RunResult { outcomes, ..RunResult::default() };
        let env = crate::exec::tests::test_env("report");
        let text = render_text(&run, &env, None, &[ProbeId::Dialog], Duration::from_secs(2));
        assert!(text.contains("skipped(3) prereq connect FAIL: version latency clients"), "{text}");
        assert!(text.contains("not run (opt-in): dialog"));
        assert!(text.contains("config      read_chunk=? write_chunk=? macro=?"));
        assert!(text.lines().last().unwrap().starts_with("summary: pass=0 fail=1 unsup=0 skip=3"));
        assert!(text.lines().all(|l| l.chars().count() <= WIDTH));
    }

    #[test]
    fn the_config_line_comes_from_passing_measurements_only() {
        let mut vm = Outcome::new(ProbeId::VmRw, Verdict::Pass);
        vm.measurements.push(Measurement { key: key::CHUNK_R.into(), value: Value::Count(4088), headline: false });
        let mut mac = Outcome::new(ProbeId::Macro, Verdict::Fail);
        mac.measurements.push(Measurement {
            key: key::SPELLING.into(),
            value: Value::Text("amp".into()),
            headline: true,
        });
        let c = config(&[vm, mac]);
        assert_eq!((c.read_chunk, c.macro_spelling), (Some(4088), None));
    }

    #[test]
    fn a_vm_restore_from_a_real_ledger_shows_on_the_line_as_restore_ok_or_failed() {
        use t32rcl::testkit::{Request, Script};
        use t32stage::{ExistsFn, Ledger, Undo, VmRegion, VmSnapshot};
        let at = 0x0100_0000u32;
        let read = |s: Script| s.expect_req(Request::MemRead { access: "VM".into(), addr: at.into(), len: 16 });
        for (back, want) in [([1u8; 16], "restore=ok"), ([2u8; 16], "restore=FAILED")] {
            let s = read(Script::new().attach()).reply_mem(&[1; 16]);
            let s = s.expect_req(Request::MemWrite { access: "VM".into(), addr: at.into(), data: vec![1; 16] });
            let s = read(s.reply_ok()).reply_mem(&back);
            let mut session = crate::exec::tests::connect(vec![Ok(s)]);
            let region = VmRegion::new(at, std::num::NonZeroU32::new(16).unwrap()).unwrap();
            let mut ledger = Ledger::new(ExistsFn::default());
            ledger.push(Undo::RestoreVm(VmSnapshot::capture(&mut session, region).unwrap())).unwrap();
            let mut o = Outcome::new(ProbeId::VmRw, Verdict::Pass);
            o.cleanup = cleanup_records(&ledger.run(&mut session));
            assert!(line(&o).split(' ').any(|t| t == want), "{}", line(&o));
        }
    }

    #[test]
    fn a_residue_line_is_the_whole_command_even_past_the_width() {
        let path = "C:\\Users\\operator\\AppData\\Local\\Temp\\t32ctl\\probe-a41c-literal\\with space\\x y.bin";
        let rm = format!("RM \"{path}\"");
        let rec = CleanupRecord {
            what: format!("remove file {path}"),
            result: "failed: still present after removal".into(),
            clean: false,
            manual: Some(rm.clone()),
        };
        assert_eq!(residue_lines("tempdir", &[rec]), [format!("RESIDUE tempdir: {rm}")]);
    }

    #[test]
    fn residue_lines_carry_the_manual_command() {
        let mut o = Outcome::new(ProbeId::Tempdir, Verdict::Pass);
        o.cleanup.push(CleanupRecord {
            what: "remove file x".into(),
            result: "failed: busy".into(),
            clean: false,
            manual: Some("RM \"C:\\x\"".into()),
        });
        assert_eq!(residue_lines("tempdir", &o.cleanup), ["RESIDUE tempdir: RM \"C:\\x\""]);
        assert!(line(&o).ends_with("residue=1"));
        o.cleanup[0].manual = Some("Data.CLEARVM VM:0x1000++0xFFF\nData.CLEARVM VM:0x3000++0xFFF".into());
        assert_eq!(
            residue_lines("vm-rw", &o.cleanup),
            ["RESIDUE vm-rw: Data.CLEARVM VM:0x1000++0xFFF", "RESIDUE vm-rw: Data.CLEARVM VM:0x3000++0xFFF"]
        );
        o.cleanup[0].manual = None;
        assert_eq!(residue_lines("macro", &o.cleanup), ["RESIDUE macro: remove file x (failed: busy)"]);
    }
}
