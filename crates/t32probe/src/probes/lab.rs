// SPDX-License-Identifier: Apache-2.0
//! The Lab: probe scripts PowerView runs with `DO`, answering through files.
//!
//! A case body is PRACTICE text plus `@` directives that `render` expands deterministically;
//! the rendered text is exactly what PowerView runs. Every path in it is a literal absolute path
//! below the Lab directory `{S}`: a macro may expand to nothing inside a `DO`'d script (third
//! live run).
//!
//! Oracle: the script saves the one-byte marker region to `{S}\<id>-<name>.bin` with
//! `Data.SAVE.Binary`, the client asks the file-exists function. It uses only what the third run
//! proved inside a `DO`'d script: literal-path `Data.SAVE.Binary VM:`, `IF`, `OS.FILE.SIZE`.
//!
//! A Lab sends `END` only to a stack it filled itself, and only after a marker file showed how far
//! the script got (`LabWatch`), after the case timeout, or after the probe stopped it on purpose.
//! `GetPracticeState` alone never ends a script: a halted script and a `WAIT` may look alike.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use t32rcl::{CallBudget, FuncValue, PracticeState};
use t32stage::{
    StagePath, Undo, VmRegion,
    practice::{cmd, fnc},
    transfer,
};

use super::{Listing, hex, list_dir, scratch_regions, soft, stage_root, workdir};
use crate::{
    ctx::{ProbeCtx, ProbeFail},
    env::ScriptOut,
    outcome::{SkipReason, Value},
};

const KIB: u32 = 1024;
const SCRIPT_LEN: u32 = 128 * KIB;
const MARKER_LEN: u32 = 4 * KIB;
const RESULT_LEN: u32 = 4 * KIB;
const INPUT_LEN: u32 = 16 * KIB;
const BLOB_LEN: u32 = 64 * KIB;
/// One `{Rk}` or `{Pk}` slot.
const SLOT: u32 = 0x100;
/// The marker byte every oracle file holds.
const MARKER: u8 = 0x4D;
/// What result slots hold before a script runs: a first byte still 0xEE means "never written".
const UNTOUCHED: u8 = 0xEE;
const SEQ_MAX: usize = 64;
/// Time kept back from the probe deadline for cleanup after the last case.
pub(crate) const LAB_RESERVE: Duration = Duration::from_secs(8);
/// A case is not started with less time than this left.
const MIN_CASE: Duration = Duration::from_secs(1);

/// How often the Lab polls and how long a halt must hold before it counts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabTiming {
    pub sample: Duration,
    pub marker_every: Duration,
    pub hold: Duration,
    pub hold_samples: u32,
    pub idle_grace: Duration,
    /// Between rounds of RCL calls a probe makes while a script runs.
    pub round: Duration,
    /// Factor on the pauses a probe takes between its own steps.
    pub pause: f64,
}

impl LabTiming {
    pub const DEFAULT: LabTiming = LabTiming {
        sample: Duration::from_millis(5),
        marker_every: Duration::from_millis(50),
        hold: Duration::from_millis(300),
        hold_samples: 3,
        idle_grace: Duration::from_secs(2),
        round: Duration::from_millis(100),
        pause: 1.0,
    };
    #[cfg(test)]
    pub const INSTANT: LabTiming = LabTiming {
        sample: Duration::ZERO,
        marker_every: Duration::ZERO,
        hold: Duration::ZERO,
        hold_samples: 3,
        idle_grace: Duration::ZERO,
        round: Duration::ZERO,
        pause: 0.0,
    };
}

/// Regions a probe needs besides the Lab's own: `large` is `{L}`, dropped when the window cannot
/// hold it; `extra` come back from `Lab::open` in order.
#[derive(Clone, Debug, Default)]
pub(crate) struct LabSizes {
    pub large: Option<u32>,
    pub extra: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expect {
    End,
    /// The case may halt once this marker exists, and only then.
    HaltAfter(&'static str),
}

#[derive(Clone, Debug)]
pub(crate) struct Case {
    pub id: String,
    pub body: String,
    pub expect: Expect,
    pub timeout: Duration,
    /// Rendered with the same placeholders and appended to the `DO` line.
    pub args: Option<String>,
    /// Script path below `{S}`, default `<id>.cmm`.
    pub script: Option<String>,
    /// Files below `{S}` the case writes besides markers and value files.
    pub outputs: Vec<String>,
    /// Directories below `{S}` the case creates.
    pub dirs: Vec<String>,
    /// Only for the case that measures the `#8` value channel itself.
    pub allow_oracle_buffer: bool,
}

impl Case {
    pub fn new(id: impl Into<String>, body: impl Into<String>, expect: Expect, timeout: Duration) -> Self {
        Self {
            id: id.into(),
            body: body.into(),
            expect,
            timeout,
            args: None,
            script: None,
            outputs: Vec::new(),
            dirs: Vec::new(),
            allow_oracle_buffer: false,
        }
    }

    pub fn outputs(mut self, names: &[&str]) -> Self {
        self.outputs.extend(names.iter().map(|n| n.to_string()));
        self
    }

    pub fn oracle_buffer(mut self) -> Self {
        self.allow_oracle_buffer = true;
        self
    }

    pub fn args(mut self, args: impl Into<String>) -> Self {
        self.args = Some(args.into());
        self
    }

    pub fn script(mut self, path: impl Into<String>) -> Self {
        self.script = Some(path.into());
        self
    }

    pub fn dirs(mut self, dirs: &[&str]) -> Self {
        self.dirs.extend(dirs.iter().map(|d| d.to_string()));
        self
    }

    pub(crate) fn script_name(&self) -> String {
        self.script.clone().unwrap_or_else(|| format!("{}.cmm", self.id))
    }
}

/// Placeholder values of one Lab.
#[derive(Clone, Debug)]
pub(crate) struct Fill {
    pub s: String,
    pub m: u32,
    pub r: u32,
    pub p: u32,
    pub b: u32,
    pub l: Option<u32>,
    pub run: String,
}

impl Fill {
    fn value(&self, name: &str, id: &str) -> Option<String> {
        let addr = |a: u32| format!("0x{a:08X}");
        let slot = |base: u32, k: &str, n: u32| {
            let i: u32 = k.parse().ok().filter(|i: &u32| *i < n && i.to_string() == k)?;
            Some(addr(base + SLOT * i))
        };
        match name {
            "S" => Some(self.s.clone()),
            "M" => Some(addr(self.m)),
            "PL" => Some(addr(self.p + 0x1000)),
            "B" => Some(addr(self.b)),
            "L" => self.l.map(addr),
            "q" => Some(self.q()),
            "run" => Some(self.run.clone()),
            "A" => Some(format!("T32PQ{}", self.run.to_ascii_uppercase())),
            "id" => Some(id.to_string()),
            _ => match (name.strip_prefix('R'), name.strip_prefix('P')) {
                (Some(k), _) => slot(self.r, k, 16),
                (_, Some(k)) => slot(self.p, k, 8),
                _ => None,
            },
        }
    }

    /// Prefix of the macros a probe creates: `t32ctl_q<run>`.
    pub fn q(&self) -> String {
        super::macro_prefix(&self.run)
    }
}

/// Replace every `{name}`; an unknown name or a `{` left over is an error.
pub(crate) fn fill_text(text: &str, f: &Fill, id: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let j = after.find('}').ok_or("a { is not closed")?;
        let name = &after[..j];
        if !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("a { is not closed".into());
        }
        out.push_str(&f.value(name, id).ok_or_else(|| format!("unknown placeholder {{{name}}}"))?);
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    if out.contains('{') {
        return Err("a { is left after rendering".into());
    }
    Ok(out)
}

/// What `render` produced: the script text and the names it answers through.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rendered {
    pub text: String,
    /// In order of first appearance.
    pub markers: Vec<String>,
    pub asks: Vec<String>,
    pub puts: Vec<u8>,
}

impl Rendered {
    /// Files below `{S}` the script may write, value files only for the `Write` channel.
    pub(crate) fn outputs(&self, id: &str, out: ScriptOut) -> Vec<String> {
        let mut v: Vec<String> = self.markers.iter().map(|m| format!("{id}-{m}.bin")).collect();
        for k in &self.asks {
            v.push(format!("{id}-{k}-yes.bin"));
            v.push(format!("{id}-{k}-no.bin"));
        }
        if out == ScriptOut::Write {
            v.extend(self.puts.iter().map(|n| format!("{id}-p{n}.txt")));
        }
        v
    }
}

fn check_name(what: &str, s: &str) -> Result<(), String> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        Ok(())
    } else {
        Err(format!("{what} {s:?} is not [a-z0-9-]+"))
    }
}

/// Expand the directives of `case.body` and fill the placeholders.
///
/// | Directive | Output |
/// |---|---|
/// | `@mark m` | `Data.SAVE.Binary "{S}\<id>-m.bin" VM:{M}++0x0` |
/// | `@a` `@h` `@w` `@z` `@v` | `@mark a` ... |
/// | `@ask k L == R` | `IF L==R` + save `<id>-k-yes.bin`, `IF L!=R` + save `<id>-k-no.bin` |
/// | `@put n "T"` | `VmSet`: `Data.Set VM:{Rn} "T" 0`; `Write`: `OPEN #8`/`WRITE #8`/`CLOSE #8` into `<id>-pn.txt`; `Off`: nothing |
///
/// Markers keep the directive's indentation; `@ask` and `@put` only at column 0, `@put` only
/// after `@z`, so a failing value line never costs the answers.
pub(crate) fn render(case: &Case, f: &Fill, out: ScriptOut) -> Result<Rendered, String> {
    check_name("case id", &case.id)?;
    let id = &case.id;
    let save = |name: &str| format!("Data.SAVE.Binary \"{{S}}\\{id}-{name}\" VM:{{M}}++0x0");
    let halt_marker = match case.expect {
        Expect::HaltAfter(m) => Some(m),
        Expect::End => None,
    };
    let mut r = Rendered::default();
    let mut lines: Vec<String> = Vec::new();
    let (mut after_z, mut after_halt) = (false, false);
    for line in case.body.lines() {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        let Some(directive) = trimmed.strip_prefix('@') else {
            if !case.allow_oracle_buffer && line.contains("#8") {
                return Err(format!("{id}: #8 is the value channel's buffer: {line:?}"));
            }
            let upper = trimmed.to_ascii_uppercase();
            if after_halt && (upper.starts_with("WAIT") || upper.starts_with("DO ")) {
                return Err(format!("{id}: {line:?} after the halt marker"));
            }
            lines.push(line.to_string());
            continue;
        };
        let (word, arg) = directive.split_once(' ').map_or((directive, ""), |(w, a)| (w, a.trim()));
        let marker = match word {
            "a" | "h" | "w" | "z" | "v" => Some(word),
            "mark" => Some(arg),
            _ => None,
        };
        if let Some(m) = marker {
            check_name("marker", m)?;
            lines.push(format!("{indent}{}", save(&format!("{m}.bin"))));
            if !r.markers.iter().any(|x| x == m) {
                r.markers.push(m.to_string());
            }
            after_z |= m == "z";
            after_halt |= halt_marker == Some(m);
            continue;
        }
        if !indent.is_empty() {
            return Err(format!("{id}: @{word} must start at column 0"));
        }
        match word {
            "ask" => {
                let (k, cond) = arg.split_once(' ').ok_or_else(|| format!("{id}: @ask without a condition"))?;
                check_name("ask", k)?;
                let (left, right) = cond.split_once(" == ").ok_or_else(|| format!("{id}: @ask {k} without ` == `"))?;
                lines.push(format!("IF {left}=={right}"));
                lines.push(format!("  {}", save(&format!("{k}-yes.bin"))));
                lines.push(format!("IF {left}!={right}"));
                lines.push(format!("  {}", save(&format!("{k}-no.bin"))));
                r.asks.push(k.to_string());
            }
            "put" => {
                if !after_z {
                    return Err(format!("{id}: @put before @z"));
                }
                let (n, text) = arg.split_once(' ').ok_or_else(|| format!("{id}: @put without a value"))?;
                let n: u8 = n.parse().ok().filter(|n| *n < 16).ok_or_else(|| format!("{id}: @put slot {n:?}"))?;
                match out {
                    ScriptOut::VmSet => lines.push(format!("Data.Set VM:{{R{n}}} {text} 0")),
                    ScriptOut::Write => {
                        lines.push(format!("OPEN #8 \"{{S}}\\{id}-p{n}.txt\" /Create"));
                        lines.push(format!("WRITE #8 {text}"));
                        lines.push("CLOSE #8".into());
                    }
                    ScriptOut::Off => {}
                }
                r.puts.push(n);
            }
            other => return Err(format!("{id}: unknown directive @{other}")),
        }
    }
    r.text = fill_text(&(lines.join("\n") + "\n"), f, id).map_err(|e| format!("{id}: {e}"))?;
    Ok(r)
}

/// The two oracle files of one `@ask`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Yes,
    No,
    Unreached,
    Both,
}

impl Answer {
    fn of(yes: bool, no: bool) -> Self {
        match (yes, no) {
            (true, false) => Answer::Yes,
            (false, true) => Answer::No,
            (false, false) => Answer::Unreached,
            (true, true) => Answer::Both,
        }
    }

    pub fn token(self) -> char {
        match self {
            Answer::Yes => 'y',
            Answer::No => 'n',
            Answer::Unreached => '-',
            Answer::Both => '!',
        }
    }
}

/// A value the script handed back through the `ScriptOut` channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Untouched,
    Empty,
    Text(String),
    /// No NUL inside the slot (`VmSet`), or a file larger than the blob region (`Write`).
    Overflow,
    NoChannel,
}

impl Slot {
    pub fn token(&self) -> String {
        match self {
            Slot::Untouched => "-".into(),
            Slot::Empty => "\"\"".into(),
            Slot::Text(t) => format!("{t:?}"),
            Slot::Overflow => "overflow".into(),
            Slot::NoChannel => "n/a".into(),
        }
    }
}

/// A `{Rk}` slot read back after the script: the 0xEE fill, an empty string, text up to the NUL,
/// or no NUL at all.
pub(crate) fn vm_slot(bytes: &[u8]) -> Slot {
    match bytes.first() {
        None | Some(&UNTOUCHED) => Slot::Untouched,
        Some(0) => Slot::Empty,
        _ => match bytes.iter().position(|&b| b == 0) {
            Some(n) => Slot::Text(String::from_utf8_lossy(&bytes[..n]).into_owned()),
            None => Slot::Overflow,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Until {
    Done,
    /// Return as soon as this marker exists; the script keeps running.
    Marker(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// `z` exists and the stack is empty.
    Ended,
    /// Idle with a non-empty stack, held, after `after` (a marker, or `stop` for our T32_Stop).
    Halted {
        after: String,
    },
    /// Empty stack for the grace period and no `a`.
    NotStarted,
    /// Empty stack for the grace period, `a` but no `z`.
    Vanished,
    TimedOut,
}

impl Ending {
    pub fn token(&self) -> String {
        match self {
            Ending::Ended => "ended".into(),
            Ending::Halted { after } => format!("halted({after})"),
            Ending::NotStarted => "not-started".into(),
            Ending::Vanished => "vanished".into(),
            Ending::TimedOut => "timeout".into(),
        }
    }
}

/// What one `LabWatch` sample concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    /// The marker `Until::Marker` waited for exists.
    Reached,
    End(Ending),
}

fn state_code(s: PracticeState) -> u8 {
    match s {
        PracticeState::Idle => 0,
        PracticeState::Running => 1,
        PracticeState::Dialog => 2,
        PracticeState::Other(n) => n,
    }
}

/// Pure judgement over `(state, depth)` samples and the markers seen so far.
///
/// An idle state with a non-empty stack is a halt only after a marker that allows it (the case's
/// `HaltAfter`, `z`, or the probe's own stop) and only once it held for `hold` and `hold_samples`;
/// before that it is a dip, counted and never acted on.
#[derive(Clone, Debug)]
pub(crate) struct LabWatch {
    timing: LabTiming,
    halt_marker: Option<&'static str>,
    t0: Instant,
    timeout: Duration,
    seen: BTreeSet<String>,
    stopped: bool,
    /// Consecutive idle samples with a non-empty stack since a marker allowed a halt.
    hold_run: Option<(Instant, u32)>,
    empty_since: Option<Instant>,
    dip: Option<Instant>,
    pub dips: u32,
    pub dip_max: Duration,
    /// Empty-stack samples before `a`.
    pub pre0: u32,
    /// First sample that was not idle with an empty stack.
    pub first1: Option<Duration>,
    /// Change points: time since `t0`, state, depth.
    pub seq: Vec<(Duration, u8, i64)>,
    last: Option<(u8, i64)>,
}

impl LabWatch {
    pub fn new(timing: LabTiming, expect: Expect, t0: Instant, timeout: Duration) -> Self {
        let halt_marker = match expect {
            Expect::HaltAfter(m) => Some(m),
            Expect::End => None,
        };
        Self {
            timing,
            halt_marker,
            t0,
            timeout,
            seen: BTreeSet::new(),
            stopped: false,
            hold_run: None,
            empty_since: None,
            dip: None,
            dips: 0,
            dip_max: Duration::ZERO,
            pre0: 0,
            first1: None,
            seq: Vec::new(),
            last: None,
        }
    }

    pub fn marker(&mut self, name: &str) {
        self.seen.insert(name.to_string());
    }

    pub fn has(&self, name: &str) -> bool {
        self.seen.contains(name)
    }

    /// The latest sample as `<state>/<depth>`.
    pub fn last(&self) -> Option<String> {
        self.last.map(|(s, d)| format!("{s}/{d}"))
    }

    /// The probe stopped the script on purpose: a held halt concludes without a marker.
    pub fn stopped(&mut self) {
        self.stopped = true;
        self.hold_run = None;
    }

    /// The script was continued: a halt has to hold again from the next sample.
    pub fn resumed(&mut self) {
        self.hold_run = None;
    }

    fn close_dip(&mut self, now: Instant) {
        if let Some(t) = self.dip.take() {
            self.dips += 1;
            self.dip_max = self.dip_max.max(now.saturating_duration_since(t));
        }
    }

    pub fn sample(&mut self, now: Instant, until: Until, state: PracticeState, depth: i64) -> Option<Seen> {
        let at = now.saturating_duration_since(self.t0);
        let code = state_code(state);
        if self.last != Some((code, depth)) {
            self.last = Some((code, depth));
            if self.seq.len() < SEQ_MAX {
                self.seq.push((at, code, depth));
            }
        }
        let idle = state == PracticeState::Idle;
        let (empty, halted) = (idle && depth == 0, idle && depth > 0);
        if !empty && self.first1.is_none() {
            self.first1 = Some(at);
        }
        if empty && !self.has("a") {
            self.pre0 += 1;
        }
        if halted {
            self.dip.get_or_insert(now);
        } else {
            self.close_dip(now);
        }
        if empty {
            self.empty_since.get_or_insert(now);
        } else {
            self.empty_since = None;
        }

        if let Until::Marker(m) = until
            && self.has(m)
        {
            return Some(Seen::Reached);
        }
        if empty && self.has("z") {
            return Some(Seen::End(Ending::Ended));
        }
        let after = self
            .halt_marker
            .filter(|m| self.has(m))
            .or(self.has("z").then_some("z"))
            .or(self.stopped.then_some("stop"));
        match after.filter(|_| halted) {
            Some(after) => {
                let (since, n) = self.hold_run.unwrap_or((now, 0));
                self.hold_run = Some((since, n + 1));
                if now.saturating_duration_since(since) >= self.timing.hold && n + 1 >= self.timing.hold_samples {
                    self.dip = None;
                    return Some(Seen::End(Ending::Halted { after: after.to_string() }));
                }
            }
            None => self.hold_run = None,
        }
        if let Some(since) = self.empty_since
            && now.saturating_duration_since(since) >= self.timing.idle_grace
        {
            if !self.has("a") {
                return Some(Seen::End(Ending::NotStarted));
            }
            if !self.has("z") {
                return Some(Seen::End(Ending::Vanished));
            }
        }
        (at >= self.timeout).then_some(Seen::End(Ending::TimedOut))
    }
}

/// `ERROR.*` and `OS.PPF()` read three ways after a halt; the order is part of the measurement.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Diag {
    /// ExecuteFunction, in order: message, cmdline, occurred, id, first id, PPF.
    pub functions: Vec<(String, Option<String>)>,
    /// `EVAL` + `T32_EvalGetString`: message, cmdline.
    pub eval: Vec<(String, Option<String>)>,
    /// ExecuteFunction again: message, cmdline.
    pub again: Vec<(String, Option<String>)>,
}

const DIAG_FUNCTIONS: [&str; 6] = [
    fnc::ERROR_MESSAGE,
    fnc::ERROR_CMDLINE,
    "ERROR.OCCURRED()",
    "ERROR.ID(\"PLAIN\")",
    "ERROR.FIRSTID(\"PLAIN\")",
    "OS.PPF()",
];

impl Diag {
    pub fn function(&self, expr: &str) -> Option<&str> {
        self.functions.iter().find(|(e, _)| e == expr).and_then(|(_, v)| v.as_deref())
    }

    pub fn evaluated(&self, expr: &str) -> Option<&str> {
        self.eval.iter().find(|(e, _)| e == expr).and_then(|(_, v)| v.as_deref())
    }

    fn text(&self) -> String {
        let mut parts = Vec::new();
        for (tag, v) in [("", &self.functions), ("eval:", &self.eval), ("again:", &self.again)] {
            for (e, r) in v {
                parts.push(format!("{tag}{e}={}", r.as_ref().map_or("refused".to_string(), |t| format!("{t:?}"))));
            }
        }
        parts.join(" ")
    }
}

/// A rendered case whose script is on disk.
#[derive(Debug)]
pub(crate) struct Staged {
    pub id: String,
    script: StagePath,
    rendered: Rendered,
    expect: Expect,
    timeout: Duration,
    args: Option<String>,
}

/// A case after its `DO`.
#[derive(Debug)]
pub(crate) struct Run {
    pub id: String,
    rendered: Rendered,
    pub watch: LabWatch,
    pub do_time: Option<Duration>,
    pub ka: u32,
    pub ending: Option<(Ending, Duration)>,
    pub diags: Vec<Diag>,
}

/// One case as `measure case.<id>` records it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CaseRecord {
    pub id: String,
    /// `ok`, `FAIL` (script not staged) or `cut` (no time left).
    pub status: &'static str,
    pub do_time: Option<Duration>,
    pub ka: u32,
    pub pre0: u32,
    pub first1: Option<Duration>,
    pub dips: u32,
    pub dip_max: Duration,
    pub ending: Option<(Ending, Duration)>,
    pub markers: BTreeSet<String>,
    pub answers: Vec<(String, Answer)>,
    pub puts: Vec<(u8, Slot)>,
    pub seq: Vec<(Duration, u8, i64)>,
    /// Stack depth after `finish`.
    pub depth: i64,
}

fn ms(d: Duration) -> String {
    format!("{}", d.as_millis())
}

impl CaseRecord {
    fn not_run(id: &str, status: &'static str) -> Self {
        Self { id: id.to_string(), status, ..Self::default() }
    }

    pub fn ran(&self) -> bool {
        self.status == "ok"
    }

    pub fn has(&self, marker: &str) -> bool {
        self.markers.contains(marker)
    }

    pub fn answer(&self, k: &str) -> Answer {
        self.answers.iter().find(|(x, _)| x == k).map_or(Answer::Unreached, |(_, a)| *a)
    }

    pub fn ended(&self) -> bool {
        matches!(self.ending, Some((Ending::Ended, _)))
    }

    pub fn halted(&self) -> bool {
        matches!(self.ending, Some((Ending::Halted { .. }, _)))
    }

    pub fn text(&self) -> String {
        if self.status != "ok" {
            return format!("stage={}", if self.status == "cut" { "ok end=cut" } else { self.status });
        }
        let opt = |d: Option<Duration>| d.map_or("-".into(), ms);
        let end = self.ending.as_ref().map_or("-".into(), |(e, at)| format!("{}@{}", e.token(), ms(*at)));
        let mut t = format!(
            "stage=ok do={} ka={} pre0={} first1={} dip={}/{} end={end} m={}",
            opt(self.do_time),
            self.ka,
            self.pre0,
            opt(self.first1),
            self.dips,
            ms(self.dip_max),
            self.markers.iter().cloned().collect::<Vec<_>>().join(","),
        );
        if !self.answers.is_empty() {
            t += " ans=";
            t += &self.answers.iter().map(|(k, a)| format!("{k}:{}", a.token())).collect::<Vec<_>>().join(" ");
        }
        if !self.puts.is_empty() {
            t += " put=";
            t += &self.puts.iter().map(|(n, s)| format!("{n}:{}", s.token())).collect::<Vec<_>>().join(" ");
        }
        let seq: Vec<String> = self.seq.iter().map(|(at, s, d)| format!("{}:{s}/{d}", ms(*at))).collect();
        t + " seq=" + &seq.join(",")
    }
}

/// Probe scripts in one work directory, with the `VM:` regions their placeholders name.
#[derive(Debug)]
pub(crate) struct Lab {
    dir: StagePath,
    fill: Fill,
    script: VmRegion,
    r: VmRegion,
    p: VmRegion,
    b: VmRegion,
    pub l: Option<VmRegion>,
    pub out: ScriptOut,
    pub timing: LabTiming,
    /// No case starts after this, on the session clock.
    limit: Instant,
    /// Lowercase names below `{S}` with a registered removal.
    registered: BTreeSet<String>,
    staged: BTreeSet<String>,
    /// Markers and asks of sub-scripts, by the id of the case that runs them.
    subs: BTreeMap<String, Rendered>,
    /// A script this Lab started may still be on the stack.
    ours_on_stack: bool,
    pub cut: Vec<String>,
    pub records: Vec<CaseRecord>,
}

fn sample(ctx: &mut ProbeCtx<'_>) -> Result<(PracticeState, i64), ProbeFail> {
    let state = ctx.session.practice_state()?;
    Ok((state, stack_depth(ctx)?))
}

pub(crate) fn stack_depth(ctx: &mut ProbeCtx<'_>) -> Result<i64, ProbeFail> {
    Ok(ctx.session.eval(fnc::STACK_DEPTH)?.into_int(fnc::STACK_DEPTH)?)
}

fn template_error(e: String) -> ProbeFail {
    ProbeFail::Check(format!("lab template: {e}"))
}

impl Lab {
    /// `workdir(tag)`, one snapshot over [script 128 KiB, M, R, P, B, L?, extra...], and the
    /// marker byte in `{M}`. `{L}` is left out when the scratch window cannot hold it.
    pub(crate) fn open(ctx: &mut ProbeCtx<'_>, tag: &str, sizes: LabSizes) -> Result<(Lab, Vec<VmRegion>), ProbeFail> {
        let root = stage_root(ctx)?;
        if !root.as_path().as_str().contains('\\') {
            let why = "script probes need a backslash stage root".to_string();
            return Err(ProbeFail::Skip(SkipReason::NotApplicable { why }));
        }
        let dir = workdir(ctx, &root, tag)?;
        let base = [SCRIPT_LEN, MARKER_LEN, RESULT_LEN, INPUT_LEN, BLOB_LEN];
        let with_large: Vec<u32> = base.iter().chain(&sizes.large).chain(&sizes.extra).copied().collect();
        let large = sizes.large.filter(|_| ctx.env.scratch_window().carve(&with_large).is_ok());
        if sizes.large.is_some() {
            ctx.measure("large", Value::Text(large.map_or("none".into(), |n| format!("{n}"))));
        }
        let wanted: Vec<u32> = base.iter().chain(&large).chain(&sizes.extra).copied().collect();
        let regions = scratch_regions(ctx, &wanted)?;
        let [script, m, r, p, b] = [regions[0], regions[1], regions[2], regions[3], regions[4]];
        let l = large.map(|_| regions[5]);
        let extra = regions[5 + usize::from(l.is_some())..].to_vec();
        ctx.session.mem_write(&m.addr(), &[MARKER])?;
        #[cfg(test)]
        let timing = ctx.env.lab_timing;
        #[cfg(not(test))]
        let timing = LabTiming::DEFAULT;
        let fill = Fill {
            s: dir.text(),
            m: m.start(),
            r: r.start(),
            p: p.start(),
            b: b.start(),
            l: l.map(|l| l.start()),
            run: ctx.env.cfg.run_id.clone(),
        };
        let lab = Lab {
            dir,
            fill,
            script,
            r,
            p,
            b,
            l,
            out: ctx.learned.script_out.unwrap_or(ScriptOut::Off),
            timing,
            limit: ctx.deadline.checked_sub(LAB_RESERVE).unwrap_or(ctx.deadline),
            registered: BTreeSet::new(),
            staged: BTreeSet::new(),
            subs: BTreeMap::new(),
            ours_on_stack: false,
            cut: Vec::new(),
            records: Vec::new(),
        };
        Ok((lab, extra))
    }

    /// `{S}`.
    pub fn dir(&self) -> &StagePath {
        &self.dir
    }

    pub fn path(&self, name: &str) -> Result<StagePath, ProbeFail> {
        Ok(self.dir.join(name)?)
    }

    /// Placeholders only, for client commands and expected values.
    pub fn fill(&self, text: &str) -> Result<String, ProbeFail> {
        fill_text(text, &self.fill, "").map_err(template_error)
    }

    pub fn q(&self) -> String {
        self.fill.q()
    }

    pub fn r_slot(&self, k: u32) -> VmRegion {
        self.r.slice(SLOT * k, SLOT).expect("16 slots fit the result region")
    }

    /// The 64 KiB blob region `{B}`.
    pub fn blob(&self) -> VmRegion {
        self.b
    }

    /// Input slot `{Pk}`, at most 0x100 bytes.
    pub fn input(&self, ctx: &mut ProbeCtx<'_>, k: u32, bytes: &[u8]) -> Result<(), ProbeFail> {
        assert!(k < 8 && bytes.len() <= SLOT as usize, "input slot {k} holds 0x100 bytes");
        Ok(ctx.session.mem_write(&self.p.addr().offset(u64::from(SLOT * k)).expect("inside P"), bytes)?)
    }

    /// The long input `{PL}`, at most 12 KiB.
    pub fn input_long(&self, ctx: &mut ProbeCtx<'_>, bytes: &[u8]) -> Result<(), ProbeFail> {
        assert!(bytes.len() <= (INPUT_LEN - 0x1000) as usize, "{{PL}} holds 12 KiB");
        Ok(ctx.session.mem_write(&self.p.addr().offset(0x1000).expect("inside P"), bytes)?)
    }

    /// `MKDIR` below `{S}`, its removal registered first; `Check` when it does not appear.
    pub fn mkdir(&mut self, ctx: &mut ProbeCtx<'_>, rel: &str) -> Result<(), ProbeFail> {
        let p = self.path(rel)?;
        ctx.push(Undo::RemoveDir(p.clone()))?;
        self.registered.insert(rel.to_ascii_lowercase());
        ctx.session.cmd(&cmd::mkdir(&p))?;
        if !self.dir_exists(ctx, rel)? {
            return Err(ProbeFail::Check(format!("MKDIR did not create {p}")));
        }
        Ok(())
    }

    pub fn dir_exists(&self, ctx: &mut ProbeCtx<'_>, rel: &str) -> Result<bool, ProbeFail> {
        let expr = fnc::dir_exists(&self.path(rel)?);
        Ok(ctx.session.eval(&expr)?.into_bool(&expr)?)
    }

    /// A pause between the probe's own steps; none in tests.
    pub fn pause(&self, d: Duration) {
        std::thread::sleep(d.mul_f64(self.timing.pause));
    }

    /// Samples for `d`, at least one, fed to the watch; the last as `<state>/<depth>`.
    pub fn observe(&self, ctx: &mut ProbeCtx<'_>, run: &mut Run, d: Duration) -> Result<String, ProbeFail> {
        let t = ctx.session.now();
        loop {
            let now = ctx.session.now();
            let (state, depth) = sample(ctx)?;
            run.watch.sample(now, Until::Done, state, depth);
            if self.timing.sample.is_zero() || now.saturating_duration_since(t) >= d {
                return Ok(format!("{}/{depth}", state_code(state)));
            }
            std::thread::sleep(self.timing.sample);
        }
    }

    /// Every result slot back to the "never written" fill.
    pub fn clear_results(&self, ctx: &mut ProbeCtx<'_>) -> Result<(), ProbeFail> {
        Ok(ctx.session.mem_write(&self.r.addr(), &[UNTOUCHED; RESULT_LEN as usize])?)
    }

    pub fn readable(&self, ctx: &mut ProbeCtx<'_>, name: &str) -> Result<bool, ProbeFail> {
        let p = self.path(name)?;
        let e = ctx.exists_fn();
        Ok(ctx.session.eval(&e.call(&p))?.into_bool(e.name())?)
    }

    /// Register the removal of a file or directory below `{S}` that a case may create but does
    /// not name in its outputs, before any case runs.
    pub fn register(&mut self, ctx: &mut ProbeCtx<'_>, rel: &str, dir: bool) -> Result<(), ProbeFail> {
        let p = self.path(rel)?;
        ctx.push(if dir { Undo::RemoveDir(p) } else { Undo::RemoveFile(p) })?;
        self.registered.insert(rel.to_ascii_lowercase());
        Ok(())
    }

    /// List the directory `rel` below `{S}`: a file nobody registered gets its removal registered.
    /// The unregistered names as `rel/<name>`; `None` when listing is refused.
    pub fn sweep(&mut self, ctx: &mut ProbeCtx<'_>, rel: &str) -> Result<Option<Vec<String>>, ProbeFail> {
        let Listing::Names { names, .. } = list_dir(ctx.session, &self.path(rel)?)? else { return Ok(None) };
        let mut stray = Vec::new();
        for n in names {
            let r = format!("{rel}/{n}");
            if self.registered.insert(r.to_ascii_lowercase()) {
                ctx.push(Undo::RemoveFile(self.path(&r)?))?;
                stray.push(r);
            }
        }
        Ok(Some(stray))
    }

    /// Refuse a name that exists before anything writes it, then register its removal.
    fn claim(&mut self, ctx: &mut ProbeCtx<'_>, names: &[String]) -> Result<(), ProbeFail> {
        for n in names {
            if self.readable(ctx, n)? {
                return Err(ProbeFail::Check(format!("stale output {n}")));
            }
        }
        for n in names {
            ctx.push(Undo::RemoveFile(self.path(n)?))?;
            self.registered.insert(n.to_ascii_lowercase());
        }
        Ok(())
    }

    fn record(&mut self, ctx: &mut ProbeCtx<'_>, rec: CaseRecord) -> CaseRecord {
        ctx.measure(&format!("case.{}", rec.id), Value::Text(rec.text()));
        self.records.push(rec.clone());
        rec
    }

    /// A helper file without markers, placeholders filled; `Check` when it does not arrive.
    pub fn stage_file(&mut self, ctx: &mut ProbeCtx<'_>, name: &str, text: &str) -> Result<(), ProbeFail> {
        let text = self.fill(text)?;
        self.claim(ctx, &[name.to_string()])?;
        let p = self.path(name)?;
        if soft(transfer::put_file(ctx.session, &self.script, text.as_bytes(), &p))?.is_none() {
            return Err(ProbeFail::Check(format!("{name} could not be staged")));
        }
        Ok(())
    }

    /// A script a case `DO`s, rendered with the directives under the id of that case, before the
    /// case is staged. Its markers and asks are read with the case's own.
    pub fn stage_sub(&mut self, ctx: &mut ProbeCtx<'_>, parent: &str, name: &str, body: &str) -> Result<(), ProbeFail> {
        let case = Case::new(parent, body, Expect::End, Duration::ZERO);
        let rendered = render(&case, &self.fill, self.out).map_err(template_error)?;
        let mut names = rendered.outputs(parent, self.out);
        names.push(name.to_string());
        self.claim(ctx, &names)?;
        let p = self.path(name)?;
        if soft(transfer::put_file(ctx.session, &self.script, rendered.text.as_bytes(), &p))?.is_none() {
            return Err(ProbeFail::Check(format!("{name} could not be staged")));
        }
        let sub = self.subs.entry(parent.to_string()).or_default();
        sub.markers.extend(rendered.markers);
        sub.asks.extend(rendered.asks);
        sub.puts.extend(rendered.puts);
        Ok(())
    }

    /// Render, refuse stale outputs, register every removal, write the script. `Err(record)` when
    /// the script did not arrive intact; the case is then not run.
    pub fn stage(&mut self, ctx: &mut ProbeCtx<'_>, case: &Case) -> Result<Result<Staged, CaseRecord>, ProbeFail> {
        if !self.staged.insert(case.id.clone()) {
            return Err(template_error(format!("{}: id staged twice", case.id)));
        }
        let mut rendered = render(case, &self.fill, self.out).map_err(template_error)?;
        let args =
            case.args.as_deref().map(|a| fill_text(a, &self.fill, &case.id)).transpose().map_err(template_error)?;
        for d in &case.dirs {
            ctx.push(Undo::RemoveDir(self.path(d)?))?;
            self.registered.insert(d.to_ascii_lowercase());
        }
        let script_name = case.script_name();
        let mut names = rendered.outputs(&case.id, self.out);
        names.extend(case.outputs.iter().cloned());
        names.push(script_name.clone());
        self.claim(ctx, &names)?;
        if let Some(sub) = self.subs.remove(&case.id) {
            for m in sub.markers {
                if !rendered.markers.contains(&m) {
                    rendered.markers.push(m);
                }
            }
            rendered.asks.extend(sub.asks);
            rendered.puts.extend(sub.puts);
        }
        let script = self.path(&script_name)?;
        if soft(transfer::put_file(ctx.session, &self.script, rendered.text.as_bytes(), &script))?.is_none() {
            return Ok(Err(self.record(ctx, CaseRecord::not_run(&case.id, "FAIL"))));
        }
        Ok(Ok(Staged { id: case.id.clone(), script, rendered, expect: case.expect, timeout: case.timeout, args }))
    }

    /// Idle with an empty stack, or our own leftover unwound once; anything else is not ours.
    fn ensure_empty_stack(&mut self, ctx: &mut ProbeCtx<'_>) -> Result<(), ProbeFail> {
        let (state, depth) = sample(ctx)?;
        if state == PracticeState::Idle && depth == 0 {
            return Ok(());
        }
        if !self.ours_on_stack {
            return Err(ProbeFail::Skip(SkipReason::PowerViewBusy { depth }));
        }
        t32stage::unwind_practice(ctx.session)?;
        let (state, depth) = sample(ctx)?;
        if state != PracticeState::Idle || depth != 0 {
            return Err(ProbeFail::Check("our script could not be unwound".into()));
        }
        ctx.ledger.withdraw_unwind()?;
        self.ours_on_stack = false;
        Ok(())
    }

    /// `DO` the staged script. `Err(record)` when the probe deadline leaves no time for it.
    pub fn start(&mut self, ctx: &mut ProbeCtx<'_>, staged: Staged) -> Result<Result<Run, CaseRecord>, ProbeFail> {
        let timeout = staged.timeout.min(self.limit.saturating_duration_since(ctx.session.now()));
        if timeout < MIN_CASE {
            self.cut.push(staged.id.clone());
            return Ok(Err(self.record(ctx, CaseRecord::not_run(&staged.id, "cut"))));
        }
        self.ensure_empty_stack(ctx)?;
        if self.out == ScriptOut::VmSet {
            self.clear_results(ctx)?;
        }
        ctx.push(Undo::UnwindPractice)?;
        let mut line = cmd::do_script(&staged.script);
        if let Some(a) = &staged.args {
            line = format!("{line} {a}");
        }
        let idle = timeout + Duration::from_secs(5);
        ctx.session.take_trace();
        let t0 = ctx.session.now();
        let sent = soft(
            ctx.session.with_budget(CallBudget::idle(idle).with_total(idle + Duration::from_secs(60))).cmd(&line),
        )?;
        let do_time = ctx.session.now().saturating_duration_since(t0);
        let ka = ctx.session.take_trace().keepalives_total;
        self.ours_on_stack = true;
        Ok(Ok(Run {
            id: staged.id,
            rendered: staged.rendered,
            watch: LabWatch::new(self.timing, staged.expect, t0, timeout),
            do_time: sent.map(|()| do_time),
            ka,
            ending: None,
            diags: Vec::new(),
        }))
    }

    /// One marker file, remembered by the watch once seen.
    pub fn poll_marker(&self, ctx: &mut ProbeCtx<'_>, run: &mut Run, m: &str) -> Result<bool, ProbeFail> {
        if !run.watch.has(m) && self.readable(ctx, &format!("{}-{m}.bin", run.id))? {
            run.watch.marker(m);
        }
        Ok(run.watch.has(m))
    }

    fn poll_markers(&self, ctx: &mut ProbeCtx<'_>, run: &mut Run, until: Until) -> Result<(), ProbeFail> {
        let mut wanted = vec!["a"];
        wanted.extend(run.watch.halt_marker);
        if let Until::Marker(m) = until {
            wanted.push(m);
        }
        wanted.push("z");
        let mut done: Vec<&str> = Vec::new();
        for m in wanted {
            if !done.contains(&m) {
                done.push(m);
                self.poll_marker(ctx, run, m)?;
            }
        }
        Ok(())
    }

    /// Sample until the watch concludes. `Halted` reads the diagnostics; `TimedOut` also stops
    /// and ends the script.
    pub fn wait(&mut self, ctx: &mut ProbeCtx<'_>, run: &mut Run, until: Until) -> Result<Seen, ProbeFail> {
        let mut checked: Option<Instant> = None;
        loop {
            let now = ctx.session.now();
            let (state, depth) = sample(ctx)?;
            if state == PracticeState::Idle
                || checked.is_none_or(|t| now.saturating_duration_since(t) >= self.timing.marker_every)
            {
                self.poll_markers(ctx, run, until)?;
                checked = Some(now);
            }
            if let Some(seen) = run.watch.sample(now, until, state, depth) {
                if let Seen::End(e) = &seen {
                    self.conclude(ctx, run, e.clone(), now)?;
                }
                return Ok(seen);
            }
            std::thread::sleep(self.timing.sample);
        }
    }

    fn conclude(&mut self, ctx: &mut ProbeCtx<'_>, run: &mut Run, e: Ending, now: Instant) -> Result<(), ProbeFail> {
        run.ending = Some((e.clone(), now.saturating_duration_since(run.watch.t0)));
        match e {
            Ending::Halted { .. } => {
                let d = self.diag(ctx, &run.id)?;
                run.diags.push(d);
            }
            Ending::TimedOut => {
                let d = self.diag(ctx, &run.id)?;
                run.diags.push(d);
                self.stop(ctx, run)?;
                self.end(ctx, 3)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `T32_Stop`: `st<status>` or `err<code>`, and how long it took. The watch then accepts a
    /// held halt without a marker.
    pub fn stop(&mut self, ctx: &mut ProbeCtx<'_>, run: &mut Run) -> Result<(String, Duration), ProbeFail> {
        run.watch.stopped();
        let t = ctx.session.now();
        let r = ctx.session.stop_practice();
        let took = ctx.session.now().saturating_duration_since(t);
        Ok((stop_token(r)?, took))
    }

    /// `CONTinue` (practice_ref) a halted script; `false` when PowerView refused it.
    pub fn cont(&mut self, ctx: &mut ProbeCtx<'_>, run: &mut Run) -> Result<bool, ProbeFail> {
        run.watch.resumed();
        Ok(soft(ctx.session.cmd("CONTinue"))?.is_some())
    }

    /// `END` while the stack is not empty, at most `max` times; the depth left.
    pub fn end(&mut self, ctx: &mut ProbeCtx<'_>, max: u32) -> Result<i64, ProbeFail> {
        for _ in 0..max {
            if stack_depth(ctx)? == 0 {
                return Ok(0);
            }
            soft(ctx.session.cmd(cmd::END))?;
        }
        stack_depth(ctx)
    }

    /// The `ERROR.*` reads of 3.7, each refused one recorded as such, as `measure diag.<label>`.
    pub fn diag(&mut self, ctx: &mut ProbeCtx<'_>, label: &str) -> Result<Diag, ProbeFail> {
        let mut d = Diag::default();
        let call = |ctx: &mut ProbeCtx<'_>, e: &str| {
            Ok::<_, ProbeFail>(soft(ctx.session.eval(e))?.map(|v| match v {
                FuncValue::Text(t) => t,
                FuncValue::Empty => String::new(),
                other => other.to_string(),
            }))
        };
        for e in DIAG_FUNCTIONS {
            let v = call(ctx, e)?;
            d.functions.push((e.to_string(), v));
        }
        for e in [fnc::ERROR_MESSAGE, fnc::ERROR_CMDLINE] {
            let v = soft(ctx.session.eval_string(e))?;
            d.eval.push((e.to_string(), v));
        }
        for e in [fnc::ERROR_MESSAGE, fnc::ERROR_CMDLINE] {
            let v = call(ctx, e)?;
            d.again.push((e.to_string(), v));
        }
        ctx.measure(&format!("diag.{label}"), Value::Text(d.text()));
        Ok(d)
    }

    /// End what is left on the stack, withdraw the unwind once it is empty, and read markers,
    /// answers and values.
    pub fn finish(&mut self, ctx: &mut ProbeCtx<'_>, mut run: Run) -> Result<CaseRecord, ProbeFail> {
        let mut depth = stack_depth(ctx)?;
        if depth != 0 {
            depth = self.end(ctx, 3)?;
        }
        if depth == 0 {
            ctx.ledger.withdraw_unwind()?;
            self.ours_on_stack = false;
        }
        run.watch.close_dip(ctx.session.now());
        for m in run.rendered.markers.clone() {
            self.poll_marker(ctx, &mut run, &m)?;
        }
        let mut answers = Vec::new();
        for k in &run.rendered.asks {
            let yes = self.readable(ctx, &format!("{}-{k}-yes.bin", run.id))?;
            let no = self.readable(ctx, &format!("{}-{k}-no.bin", run.id))?;
            answers.push((k.clone(), Answer::of(yes, no)));
        }
        let mut puts = Vec::new();
        for &n in &run.rendered.puts {
            let slot = match self.out {
                ScriptOut::VmSet => {
                    let bytes = ctx.session.mem_read(&self.r_slot(n.into()).addr(), SLOT as usize)?;
                    ctx.measure(&format!("put.{}.{n}", run.id), Value::Text(hex(&bytes[..64])));
                    vm_slot(&bytes)
                }
                ScriptOut::Write => {
                    let (slot, raw) = self.file_slot(ctx, &format!("{}-p{n}.txt", run.id))?;
                    ctx.measure(&format!("put.{}.{n}", run.id), Value::Text(raw_note(&raw)));
                    slot
                }
                ScriptOut::Off => Slot::NoChannel,
            };
            puts.push((n, slot));
        }
        let w = &run.watch;
        let rec = CaseRecord {
            id: run.id.clone(),
            status: "ok",
            do_time: run.do_time,
            ka: run.ka,
            pre0: w.pre0,
            first1: w.first1,
            dips: w.dips,
            dip_max: w.dip_max,
            ending: run.ending.clone(),
            markers: w.seen.clone(),
            answers,
            puts,
            seq: w.seq.clone(),
            depth,
        };
        Ok(self.record(ctx, rec))
    }

    /// A text file below `{S}` loaded through the blob region: the slot (trailing CR/LF cut) and
    /// the raw bytes.
    pub fn file_slot(&self, ctx: &mut ProbeCtx<'_>, name: &str) -> Result<(Slot, Vec<u8>), ProbeFail> {
        if !self.readable(ctx, name)? {
            return Ok((Slot::Untouched, Vec::new()));
        }
        let p = self.path(name)?;
        let Some(size) = soft(transfer::file_size(ctx.session, &p))? else { return Ok((Slot::Untouched, Vec::new())) };
        if size == 0 {
            return Ok((Slot::Empty, Vec::new()));
        }
        if size > u64::from(self.b.len()) {
            return Ok((Slot::Overflow, Vec::new()));
        }
        let Some(raw) = soft(transfer::get_file(ctx.session, &p, &self.b, size as u32))? else {
            return Ok((Slot::Untouched, Vec::new()));
        };
        let text = String::from_utf8_lossy(&raw).trim_end_matches(['\r', '\n']).to_string();
        Ok((Slot::Text(text), raw))
    }

    /// `stage` + `start`; `None` when the case did not get that far (its record says why).
    pub fn begin(&mut self, ctx: &mut ProbeCtx<'_>, case: &Case) -> Result<Option<Run>, ProbeFail> {
        let staged = match self.stage(ctx, case)? {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        Ok(self.start(ctx, staged)?.ok())
    }

    /// A file below `{S}` loaded through the blob region; `None` when absent, empty or refused.
    pub fn fetch(&self, ctx: &mut ProbeCtx<'_>, name: &str) -> Result<Option<Vec<u8>>, ProbeFail> {
        if !self.readable(ctx, name)? {
            return Ok(None);
        }
        let p = self.path(name)?;
        match soft(transfer::file_size(ctx.session, &p))? {
            Some(size) if size > 0 && size <= u64::from(self.b.len()) => {
                Ok(soft(transfer::get_file(ctx.session, &p, &self.b, size as u32))?)
            }
            _ => Ok(None),
        }
    }

    /// `stage + start + wait(Done) + finish`.
    pub fn run(&mut self, ctx: &mut ProbeCtx<'_>, case: &Case) -> Result<CaseRecord, ProbeFail> {
        let staged = match self.stage(ctx, case)? {
            Ok(s) => s,
            Err(rec) => return Ok(rec),
        };
        let mut run = match self.start(ctx, staged)? {
            Ok(r) => r,
            Err(rec) => return Ok(rec),
        };
        self.wait(ctx, &mut run, Until::Done)?;
        self.finish(ctx, run)
    }

    /// List `{S}`: a file nobody registered (a `.bak`, a name PowerView chose) gets its removal
    /// registered and is reported as `stray`. Also `cut=<n>` when cases were cut.
    pub fn close(self, ctx: &mut ProbeCtx<'_>) -> Result<(), ProbeFail> {
        if !self.cut.is_empty() {
            ctx.headline("cut", Value::Count(self.cut.len() as u64));
            ctx.measure("cut_ids", Value::Text(self.cut.join(",")));
        }
        match list_dir(ctx.session, &self.dir)? {
            Listing::Unavailable(why) => ctx.measure("listing", Value::Text(format!("unavailable: {why}"))),
            Listing::Names { names, first_raw } => {
                ctx.measure("listing_first", Value::Text(first_raw));
                let stray: Vec<String> =
                    names.into_iter().filter(|n| !self.registered.contains(&n.to_ascii_lowercase())).collect();
                for n in &stray {
                    if let Ok(p) = self.dir.join(n) {
                        ctx.push(Undo::RemoveFile(p))?;
                    }
                }
                ctx.measure("stray_names", Value::Text(stray.join(",")));
                if !stray.is_empty() {
                    ctx.headline("stray", Value::Count(stray.len() as u64));
                }
            }
        }
        Ok(())
    }
}

/// A function's value as text; `None` when PowerView refused it.
pub(crate) fn eval_text(ctx: &mut ProbeCtx<'_>, expr: &str) -> Result<Option<String>, ProbeFail> {
    Ok(soft(ctx.session.eval(expr))?.map(|v| match v {
        FuncValue::Text(t) => t,
        FuncValue::Empty => String::new(),
        other => other.to_string(),
    }))
}

/// `st<status>` for an answered `T32_Stop`, `err<code>` for a refused one.
pub(crate) fn stop_token(r: Result<u8, t32rcl::Error>) -> Result<String, ProbeFail> {
    match r {
        Ok(s) => Ok(format!("st{s}")),
        Err(e) => match e.remote() {
            Some(remote) => Ok(format!("err{}", remote.code)),
            None => Err(e.into()),
        },
    }
}

/// Raw value bytes as hex (first 64) and their line ending.
pub(crate) fn raw_note(raw: &[u8]) -> String {
    let eol = if raw.windows(2).any(|w| w == b"\r\n") {
        "crlf"
    } else if raw.contains(&b'\n') {
        "lf"
    } else {
        "none"
    };
    format!("hex={} eol={eol}", hex(&raw[..raw.len().min(64)]))
}

#[cfg(test)]
#[path = "lab_tests.rs"]
pub(crate) mod lab_tests;
