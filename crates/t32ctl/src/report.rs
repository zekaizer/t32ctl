// SPDX-License-Identifier: Apache-2.0
//! One report value per call, rendered as text or as one JSON object (`t32ctl.report/1`), so the
//! two renderings cannot disagree.
//!
//! Text, in this order: the header (connecting calls only), `! ` notices, body lines,
//! `error[<tag>]: ` with indented detail lines (never for exit 1), and `next: ` hints.

use std::fmt;

use serde::{Serialize, Serializer};
use t32rcl::FuncValue;
use t32stage::ScriptRc;

use crate::{
    error::{CtlError, Exit},
    notation::{Size, display_ms, shell_word},
};

pub const SCHEMA: &str = "t32ctl.report/1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The leaf path, `read status`.
    pub command: String,
    pub header: Option<Header>,
    pub notices: Vec<Notice>,
    pub body: Option<Body>,
    pub error: Option<CtlError>,
    pub next: Vec<Hint>,
}

impl Report {
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into(), header: None, notices: Vec::new(), body: None, error: None, next: Vec::new() }
    }

    /// Record a failure: its hints become the report's, and what the call did before it, its body.
    pub fn fail(&mut self, mut e: CtlError) {
        self.next.append(&mut e.next);
        if let Some(b) = e.body.take() {
            self.body = Some(*b);
        }
        self.error = Some(e);
    }

    /// An error decides the exit; otherwise the body's outcome does.
    pub fn exit(&self) -> Exit {
        match (&self.error, &self.body) {
            (Some(e), _) => e.exit(),
            (None, Some(b)) => b.outcome(),
            (None, None) => Exit::Ok,
        }
    }

    pub fn text(&self) -> String {
        let mut lines = Vec::new();
        lines.extend(self.header.as_ref().map(Header::to_string));
        lines.extend(self.notices.iter().map(|n| format!("! {}", n.text)));
        lines.extend(self.body.iter().flat_map(Body::lines));
        if let Some(e) = &self.error {
            lines.push(e.to_string());
            lines.extend(e.details.iter().map(|d| format!("  {d}")));
        }
        lines.extend(self.next.iter().map(|h| format!("next: {}", h.render())));
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    /// One line of JSON. Members of planned features are absent, not `null`.
    pub fn json(&self) -> String {
        let exit = self.exit();
        let doc = JsonReport {
            schema: SCHEMA,
            command: &self.command,
            exit: JsonExit { code: exit.code(), class: exit.class() },
            header: self.header.as_ref(),
            notices: &self.notices,
            body: self.body.as_ref(),
            truncated: None,
            error: self.error.as_ref().map(|e| JsonError {
                kind: e.kind.tag(),
                message: &e.message,
                details: &e.details,
            }),
            next: self.next.iter().map(Hint::render).collect(),
        };
        let mut text = serde_json::to_string(&doc).expect("a report serializes");
        text.push('\n');
        text
    }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    schema: &'static str,
    command: &'a str,
    exit: JsonExit,
    header: Option<&'a Header>,
    notices: &'a [Notice],
    body: Option<&'a Body>,
    truncated: Option<()>,
    error: Option<JsonError<'a>>,
    next: Vec<String>,
}

#[derive(Serialize)]
struct JsonExit {
    code: u8,
    class: &'static str,
}

#[derive(Serialize)]
struct JsonError<'a> {
    kind: &'static str,
    message: &'a str,
    details: &'a [String],
}

/// A 64-bit value printed in `0x` hex, also in JSON, so it survives every JSON consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hex(pub u64);

impl fmt::Display for Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// Where the target is after the call, read best effort: a value PowerView refused is absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "run", rename_all = "lowercase")]
pub enum Header {
    Down,
    Stopped {
        /// PowerView's selected core.
        #[serde(skip_serializing_if = "Option::is_none")]
        core: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        at: Option<At>,
    },
    Running {
        /// The number of cores.
        #[serde(skip_serializing_if = "Option::is_none")]
        cores: Option<i64>,
    },
    /// A `T32_GetState` value without a run state t32ctl knows.
    Other {
        state: u8,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct At {
    pub pc: Hex,
}

impl fmt::Display for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Header::Down => f.write_str("down"),
            Header::Stopped { core, at } => {
                f.write_str("stopped")?;
                if let Some(c) = core {
                    write!(f, " core={c}")?;
                }
                if let Some(at) = at {
                    write!(f, " pc={}", at.pc)?;
                }
                Ok(())
            }
            Header::Running { cores: None } => f.write_str("running"),
            Header::Running { cores: Some(n) } if *n <= 1 => f.write_str("running cores=0"),
            Header::Running { cores: Some(n) } => write!(f, "running cores=0-{}", n - 1),
            Header::Other { state } => write!(f, "state={state}"),
        }
    }
}

/// Something the caller did not ask for but must not miss.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub text: String,
}

impl Notice {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// A complete command, as the words after `t32ctl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint(pub Vec<String>);

impl Hint {
    pub fn render(&self) -> String {
        std::iter::once("t32ctl").chain(self.0.iter().map(String::as_str)).map(shell_word).collect::<Vec<_>>().join(" ")
    }
}

/// The body of a report, tagged by `kind` in JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Body {
    Status(Box<StatusBody>),
    Mem(MemBody),
    Regs(RegsBody),
    Eval(EvalBody),
    Cmd(CmdBody),
    Procs(ProcsBody),
    Run(Box<RunBody>),
}

impl Body {
    /// Exit 0 or 1: whether the command ended as asked.
    pub fn outcome(&self) -> Exit {
        match self {
            Body::Mem(MemBody { data: MemData::Stopped(_), .. }) => Exit::Outcome,
            Body::Run(r) => r.exit(),
            Body::Status(_) | Body::Mem(_) | Body::Regs(_) | Body::Eval(_) | Body::Cmd(_) | Body::Procs(_) => Exit::Ok,
        }
    }

    pub fn lines(&self) -> Vec<String> {
        match self {
            Body::Status(s) => s.lines(),
            Body::Mem(m) => m.lines(),
            Body::Regs(r) => r.lines(),
            Body::Eval(e) => vec![e.to_string()],
            Body::Cmd(_) => vec!["cmd: done".into()],
            Body::Procs(p) => p.lines(),
            Body::Run(r) => r.lines(),
        }
    }
}

/// `read mem`: the place, the physical address of a translated place, the regions the read touched,
/// and the bytes as cells or in a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemBody {
    /// The place with its class: `D:0x40107e40`.
    pub place: String,
    /// The place as text prints it: without the class when it is the profile's default.
    #[serde(skip)]
    pub shown: String,
    #[serde(skip)]
    pub class: String,
    #[serde(skip)]
    pub addr: u64,
    /// The physical address of the first byte, for a translated class.
    pub phys: Option<Phys>,
    /// `None` before the memory map checked the place, unless the caller gave it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub len: Option<u64>,
    /// `None` before the memory map checked the place, unless the caller gave it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u8>,
    /// Empty before the memory map checked the place.
    pub regions: Vec<RegionRef>,
    #[serde(flatten)]
    pub data: MemData,
}

/// A physical address: `0x...` in text, `A:0x...` in JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Phys(pub u64);

impl Serialize for Phys {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&format_args!("A:{:#x}", self.0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RegionRef {
    pub name: String,
    pub kind: &'static str,
}

/// The bytes of a read: cells at the access width, little-endian; a file; or a transfer its bound
/// stopped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemData {
    Cells(Vec<String>),
    File(FileStats),
    Stopped(StoppedTransfer),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileStats {
    pub path: String,
    pub bytes: u64,
    pub after_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StoppedTransfer {
    pub bound_ms: u64,
    /// Bytes written before the bound; the partial file was removed.
    pub bytes: u64,
}

/// `0x` and two hex digits per byte of each `width`-byte little-endian cell. AArch64 is
/// little-endian; another byte order needs a profile key.
pub fn cells(data: &[u8], width: u8) -> Vec<String> {
    let w = usize::from(width.max(1));
    data.chunks(w)
        .map(|c| {
            let v = c.iter().rev().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
            format!("0x{v:0digits$x}", digits = 2 * c.len())
        })
        .collect()
}

/// Bytes per cell line.
const LINE: usize = 16;

impl MemBody {
    fn lines(&self) -> Vec<String> {
        let mut head = format!("mem {}", self.shown);
        if let Some(len) = self.len {
            head.push_str(&format!(" len={}", Size(len)));
        }
        if let Some(width) = self.width {
            head.push_str(&format!(" width={width}"));
        }
        head.push_str(&format!(" class={}", self.class));
        if let Some(p) = self.phys {
            head.push_str(&format!(" phys={:#x}", p.0));
        }
        if !self.regions.is_empty() {
            let regions: Vec<String> = self.regions.iter().map(|r| format!("{} {:?}", r.kind, r.name)).collect();
            let noun = if regions.len() == 1 { "region" } else { "regions" };
            head.push_str(&format!(" ({noun} {})", regions.join(", ")));
        }
        let mut v = vec![head];
        let prefix = self.shown.strip_suffix(&format!("{:#x}", self.addr)).unwrap_or_default();
        match &self.data {
            MemData::Cells(cells) => {
                let per_line = (LINE / usize::from(self.width.unwrap_or(1).max(1))).max(1);
                for (i, line) in cells.chunks(per_line).enumerate() {
                    let at = self.addr.wrapping_add((i * LINE) as u64);
                    v.push(format!("{prefix}{at:#x}: {}", line.join(" ")));
                }
            }
            MemData::File(f) => {
                v.push(format!("file: {} ({} in {})", f.path, Size(f.bytes), display_ms(f.after_ms)));
            }
            MemData::Stopped(s) => v.push(format!(
                "transfer stopped at the {} bound after {}; the partial file was removed",
                display_ms(s.bound_ms),
                Size(s.bytes)
            )),
        }
        v
    }
}

/// `read reg`: values of PowerView's selected core, named as the caller typed them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RegsBody {
    /// `CORE()`; absent when PowerView refused it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core: Option<i64>,
    pub regs: Vec<RegValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RegValue {
    pub name: String,
    pub value: Reg64,
}

/// A register value: `0x` and 16 hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reg64(pub u64);

impl fmt::Display for Reg64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}

impl Serialize for Reg64 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl RegsBody {
    /// Four `name=value` tokens per line.
    fn lines(&self) -> Vec<String> {
        let tokens: Vec<String> = self.regs.iter().map(|r| format!("{}={}", r.name, r.value)).collect();
        tokens.chunks(4).map(|c| c.join(" ")).collect()
    }
}

/// `raw eval`: the value in argument notation and its type (`bool`, `decimal`, `hex`, `float`,
/// `time`, `string`, `empty`, or `type 0x<word>` for a type t32rcl does not interpret).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EvalBody {
    #[serde(rename = "type")]
    pub ty: String,
    /// A string without its quotes.
    pub value: String,
}

impl EvalBody {
    pub fn new(v: &FuncValue) -> Self {
        let (ty, value) = match v {
            FuncValue::Bool(b) => ("bool".into(), if *b { "TRUE()" } else { "FALSE()" }.into()),
            FuncValue::Int(n) => ("decimal".into(), n.to_string()),
            FuncValue::Uint(n) => ("hex".into(), format!("{n:#x}")),
            FuncValue::Float(x) => ("float".into(), format!("{x:?}")),
            FuncValue::Seconds(x) => ("time".into(), format!("{x}s")),
            FuncValue::Text(t) => ("string".into(), t.clone()),
            FuncValue::Empty => ("empty".into(), String::new()),
            FuncValue::Raw { ty, text } => (format!("type {ty:#x}"), text.clone()),
        };
        Self { ty, value }
    }
}

impl fmt::Display for EvalBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty.as_str() {
            "empty" => f.write_str("(empty)"),
            "string" => write!(f, "\"{}\" (string)", self.value),
            ty => write!(f, "{} ({ty})", self.value),
        }
    }
}

/// `raw cmd`: the PRACTICE state after the command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CmdBody {
    pub practice: Practice,
    /// `PRACTICE.StackDepth()`.
    pub depth: i64,
}

fn labeled(label: &str, rest: impl fmt::Display) -> String {
    format!("{label:<7} {rest}")
}

/// `read status`: the target, the profile and PowerView, and with `--full` the profile hash, the
/// binding, the config and the state directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatusBody {
    pub target: TargetView,
    pub profile: ProfileView,
    pub pv: PvStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<BindingView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<String>,
}

impl StatusBody {
    fn lines(&self) -> Vec<String> {
        let mut v = vec![labeled("target", &self.target), labeled("profile", &self.profile)];
        if let Some(b) = &self.binding {
            v.push(labeled("binding", b));
        }
        if let Some(c) = &self.config {
            v.push(labeled("config", c));
        }
        if let Some(s) = &self.state_dir {
            v.push(labeled("state", s));
        }
        v.push(labeled("pv", &self.pv));
        v
    }
}

/// The connected target against the profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TargetView {
    /// The profile's `target.name`; this and the other `profile_` fields are absent without a
    /// profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `SYStem.CPU()`.
    pub cpu: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_cpu: Option<String>,
    pub cpu_check: CpuMatch,
    /// `CORE.NUMBER()`.
    pub cores: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_cores: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CpuMatch {
    Match,
    Mismatch,
    /// The target is down: the prelude does not check.
    Skipped,
    /// No profile to compare with.
    #[serde(rename = "no-profile")]
    NoProfile,
}

impl fmt::Display for TargetView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            write!(f, "{name} ")?;
        }
        write!(f, "cpu={} ", self.cpu)?;
        match self.cpu_check {
            CpuMatch::Match => f.write_str("(matches profile)")?,
            CpuMatch::Mismatch => write!(f, "(profile: {}, mismatch)", self.profile_cpu.as_deref().unwrap_or("?"))?,
            CpuMatch::Skipped => f.write_str("(not checked while down)")?,
            CpuMatch::NoProfile => f.write_str("(no profile)")?,
        }
        write!(f, " cores={}", self.cores)?;
        if let Some(want) = self.profile_cores.filter(|&n| self.cores != i64::from(n)) {
            write!(f, " (profile: {want})")?;
        }
        Ok(())
    }
}

/// The profile directory as printed; `--full` adds the hash (all 64 digits) and the file count.
/// Without a profile only `missing` is set: why there is none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
}

impl fmt::Display for ProfileView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(why) = &self.missing {
            return write!(f, "none: {why}");
        }
        f.write_str(self.dir.as_deref().unwrap_or_default())?;
        if let (Some(hash), Some(n)) = (&self.hash, self.files) {
            let files = if n == 1 { "file" } else { "files" };
            write!(f, " profile={} ({n} {files})", &hash[..hash.len().min(8)])?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PvStatus {
    pub build: u32,
    pub base: Option<u32>,
    /// RCL clients; `None` when PowerView refused both spellings of the function.
    pub clients: Option<i64>,
    pub practice: Practice,
    /// `PRACTICE.StackDepth()`.
    pub depth: i64,
}

impl fmt::Display for PvStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "build={} clients=", self.build)?;
        match self.clients {
            Some(n) => write!(f, "{n}")?,
            None => f.write_str("?")?,
        }
        write!(f, " practice={}", self.practice)?;
        if self.depth > 0 {
            write!(f, " (depth {})", self.depth)?;
        }
        Ok(())
    }
}

/// The PRACTICE state; `Halted` is idle with a non-empty stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Practice {
    Idle,
    Running,
    Dialog,
    Halted,
    /// A `T32_GetPracticeState` value t32ctl does not know.
    Other(u8),
}

impl fmt::Display for Practice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Practice::Idle => f.write_str("idle"),
            Practice::Running => f.write_str("running"),
            Practice::Dialog => f.write_str("dialog"),
            Practice::Halted => f.write_str("halted"),
            Practice::Other(n) => write!(f, "state{n}"),
        }
    }
}

impl Serialize for Practice {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// A resolved path as printed, and the rule or file it came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sourced {
    pub path: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BindingView {
    pub build: Option<Sourced>,
    /// The rule or file that named the profile directory; `None` without a profile.
    pub profile_from: Option<String>,
    /// The `[variant."<board>"]` applied; `None` for the base profile.
    pub variant: Option<String>,
    /// `BOARD` from the build's `CMakeCache.txt`.
    pub board: Option<String>,
}

impl fmt::Display for BindingView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.build {
            Some(s) => write!(f, "build={} ({})", s.path, s.source)?,
            None => f.write_str("build none")?,
        }
        let Some(from) = &self.profile_from else { return f.write_str(", profile none") };
        write!(f, ", profile from {from}, variant ")?;
        match (&self.variant, &self.board) {
            (Some(v), _) => f.write_str(v),
            (None, Some(b)) => write!(f, "base (BOARD={b})"),
            (None, None) => f.write_str("base (no BOARD)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConfigView {
    /// The config file as printed; `None` when there is none and the defaults apply.
    pub file: Option<String>,
    pub host: String,
    /// The candidate that answered, for `host = "auto"`.
    pub attached: Option<String>,
    pub port: u16,
}

impl fmt::Display for ConfigView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.file {
            Some(p) => write!(f, "{p}: ")?,
            None => f.write_str("none (defaults): ")?,
        }
        write!(f, "host={}", self.host)?;
        if let Some(a) = self.attached.as_ref().filter(|a| **a != self.host) {
            write!(f, " ({a})")?;
        }
        write!(f, " port={}", self.port)
    }
}

/// `read procs`: the profile's procedures, or the one named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProcsBody {
    pub profile: ProfileRef,
    pub procs: Vec<ProcView>,
    /// A name was asked for: the declaration in full instead of the table.
    #[serde(skip)]
    pub one: bool,
}

/// The profile directory as printed, its hash (all 64 digits) and its file count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileRef {
    pub dir: String,
    pub hash: String,
    pub files: usize,
}

impl fmt::Display for ProfileRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} profile={}", self.dir, &self.hash[..self.hash.len().min(8)])
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProcView {
    pub name: String,
    /// The effective grade: the word the caller types.
    pub grade: &'static str,
    pub timeout_ms: u64,
    pub about: String,
    pub effects: Vec<&'static str>,
    pub outputs: Vec<String>,
    pub params: Vec<ParamView>,
    #[serde(skip)]
    pub usage: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ParamView {
    pub name: String,
    pub flag: String,
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub about: String,
    /// In the caller's notation; `{build}` stays literal.
    pub default: Option<String>,
    pub values: Vec<String>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub required: bool,
}

impl ParamView {
    /// The about text with the enum values, the bounds, and the default or `(required)`.
    fn described(&self) -> String {
        let mut s = self.about.clone();
        if !self.values.is_empty() {
            s.push_str(&format!(": {}", self.values.join(", ")));
        }
        match (self.min, self.max) {
            (Some(lo), Some(hi)) => s.push_str(&format!(", {lo} to {hi}")),
            (Some(lo), None) => s.push_str(&format!(", at least {lo}")),
            (None, Some(hi)) => s.push_str(&format!(", at most {hi}")),
            (None, None) => {}
        }
        match &self.default {
            Some(d) => s.push_str(&format!(" (default: {d})")),
            None => s.push_str(" (required)"),
        }
        s
    }
}

/// `none` for an empty list.
fn listed(items: &[impl AsRef<str>]) -> String {
    if items.is_empty() { "none".into() } else { items.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(", ") }
}

impl ProcsBody {
    fn lines(&self) -> Vec<String> {
        match (self.one, self.procs.first()) {
            (true, Some(p)) => self.declaration(p),
            _ => self.table(),
        }
    }

    fn table(&self) -> Vec<String> {
        if self.procs.is_empty() {
            return vec![format!("no procedures in {}", self.profile)];
        }
        let footer = format!("{}; parameters: t32ctl read procs NAME", self.profile);
        let rows: Vec<[String; 4]> = std::iter::once(["proc", "grade", "timeout", "about"].map(String::from))
            .chain(
                self.procs
                    .iter()
                    .map(|p| [p.name.clone(), p.grade.to_string(), display_ms(p.timeout_ms), p.about.clone()]),
            )
            .collect();
        let width = |i: usize| rows.iter().map(|r| r[i].len()).max().unwrap_or(0) + 2;
        let (w0, w1, w2) = (width(0), width(1), width(2));
        let mut v: Vec<String> =
            rows.iter().map(|r| format!("{:<w0$}{:<w1$}{:<w2$}{}", r[0], r[1], r[2], r[3])).collect();
        v.push(footer);
        v
    }

    fn declaration(&self, p: &ProcView) -> Vec<String> {
        let label = |l: &str, rest: String| format!("{l:<8} {rest}");
        let mut v = vec![
            label("proc", format!("{}  grade {}  timeout {}", p.name, p.grade, display_ms(p.timeout_ms))),
            label("about", p.about.clone()),
        ];
        let flags: Vec<String> = p.params.iter().map(|a| format!("{} {}", a.flag, a.ty.to_ascii_uppercase())).collect();
        let w = flags.iter().map(String::len).max().unwrap_or(0) + 2;
        if p.params.is_empty() {
            v.push(label("params", "none".into()));
        }
        for (i, (flag, a)) in flags.iter().zip(&p.params).enumerate() {
            let rest = format!("{flag:<w$}{}", a.described());
            v.push(if i == 0 { label("params", rest) } else { format!("{:9}{rest}", "") });
        }
        v.push(label("outputs", listed(&p.outputs)));
        v.push(label("effects", listed(&p.effects)));
        v.push(label("profile", self.profile.to_string()));
        v.push(label("usage", p.usage.clone()));
        v
    }
}

/// `<grade> proc` and `raw script`: what ran, how it ended, and the files it wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunBody {
    #[serde(flatten)]
    pub subject: Subject,
    /// `None` when the run ended without a result: a dialog, a vanished runner.
    pub result: Option<RunResult>,
    pub outputs: OutputsView,
}

/// What ran: a procedure of the profile, or a script bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "what", rename_all = "lowercase")]
pub enum Subject {
    Proc { name: String, grade: &'static str, profile: Bundled, params: Vec<ParamShown> },
    Script { entry: String, bundle: Bundled },
}

/// The bundle a run used: for a procedure the profile (`hash`), for a script its root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Bundled {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub files: usize,
    /// The snapshot was already on the PowerView host.
    pub cached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ParamShown {
    pub name: String,
    /// In the caller's notation; a file as a local path.
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<Upload>,
}

/// A file parameter's copy on the PowerView host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Upload {
    pub path: String,
    pub bytes: u64,
    pub slices: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunResult {
    pub outcome: Outcome,
    /// `&t32ctl_rc`: an integer, text, or `null` when the script set none.
    #[serde(serialize_with = "rc_json")]
    pub rc: ScriptRc,
    /// `&t32ctl_msg`, or the caught error's message.
    pub message: String,
    /// The halting error's message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The halting command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    pub after_ms: u64,
    /// A burn procedure must confirm success with an explicit rc.
    #[serde(skip)]
    pub burn: bool,
}

fn rc_json<S: Serializer>(rc: &ScriptRc, s: S) -> Result<S::Ok, S::Error> {
    match rc {
        ScriptRc::Unset => s.serialize_none(),
        ScriptRc::Int(n) => s.serialize_i64(*n),
        ScriptRc::Text(t) => s.serialize_str(t),
    }
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The runner reached its end without catching an error.
    Finished,
    /// The runner caught a PRACTICE error.
    Failed,
    /// A PRACTICE error halted the script; it was unwound.
    Halted,
    TimedOut {
        timeout_ms: u64,
    },
    /// A file parameter's upload passed `[calls] transfer_timeout`; nothing ran.
    UploadStopped {
        flag: String,
        bound_ms: u64,
    },
}

impl Serialize for Outcome {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            Outcome::Finished => "finished",
            Outcome::Failed => "failed",
            Outcome::Halted => "halted",
            Outcome::TimedOut { .. } => "timed-out",
            Outcome::UploadStopped { .. } => "upload-stopped",
        })
    }
}

impl RunResult {
    /// Ended as asked: finished with rc 0, or with no rc except for a burn procedure.
    pub fn ok(&self) -> bool {
        self.outcome == Outcome::Finished
            && match &self.rc {
                ScriptRc::Int(0) => true,
                ScriptRc::Unset => !self.burn,
                _ => false,
            }
    }

    fn lines(&self) -> Vec<String> {
        let after = display_ms(self.after_ms);
        let msg = if self.message.is_empty() { String::new() } else { format!(": \"{}\"", self.message) };
        match &self.outcome {
            Outcome::Finished => vec![match &self.rc {
                ScriptRc::Unset if self.burn => format!(
                    "result: finished without rc after {after}: a burn procedure must set &t32ctl_rc=\"0\" after its \
                     verification"
                ),
                ScriptRc::Unset => format!("result: finished after {after}{msg}"),
                rc => format!("result: finished rc={rc} after {after}{msg}"),
            }],
            Outcome::Failed => vec![
                format!("result: failed by a PRACTICE error after {after}"),
                format!("  message: {}", self.message),
                "  the runner caught it; staged files were removed".into(),
            ],
            Outcome::Halted => {
                let mut v = vec![format!("result: halted by a PRACTICE error after {after}")];
                v.extend(self.error.as_ref().map(|e| format!("  message: {e}")));
                v.extend(self.line.as_ref().filter(|l| !l.is_empty()).map(|l| format!("  line:    {l}")));
                v.push("  the script was unwound; staged files were removed".into());
                v
            }
            Outcome::TimedOut { timeout_ms } => {
                vec![format!("result: timed out after {}; the script was stopped and unwound", display_ms(*timeout_ms))]
            }
            Outcome::UploadStopped { flag, bound_ms } => vec![format!(
                "result: the upload of {flag} stopped at the {} bound; the procedure did not run",
                display_ms(*bound_ms)
            )],
        }
    }
}

/// The files a run wrote into its output directory, and the declared ones it did not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutputsView {
    /// The output directory as printed; `None` when the run wrote nothing.
    pub dir: Option<String>,
    pub files: Vec<OutputFile>,
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutputFile {
    pub name: String,
    pub bytes: u64,
}

impl RunBody {
    pub fn exit(&self) -> Exit {
        let ok = self.result.as_ref().is_none_or(RunResult::ok);
        if ok && self.outputs.missing.is_empty() { Exit::Ok } else { Exit::Outcome }
    }

    fn lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        let count = |n: usize| if n == 1 { "1 file".to_string() } else { format!("{n} files") };
        let cached = |b: &Bundled| if b.cached { "cached" } else { "uploaded" };
        match &self.subject {
            Subject::Proc { name, profile, params, .. } => {
                let hash = profile.hash.as_deref().unwrap_or_default();
                v.push(format!(
                    "proc {name} (profile={}, {}, {})",
                    &hash[..hash.len().min(8)],
                    count(profile.files),
                    cached(profile)
                ));
                if !params.is_empty() {
                    let shown: Vec<String> = params
                        .iter()
                        .map(|p| match &p.file {
                            Some(f) => {
                                let slices =
                                    if f.slices == 1 { "1 slice".into() } else { format!("{} slices", f.slices) };
                                format!("{}={} ({}, {slices})", p.name, p.value, Size(f.bytes))
                            }
                            None => format!("{}={}", p.name, p.value),
                        })
                        .collect();
                    v.push(format!("params: {}", shown.join(" ")));
                }
            }
            Subject::Script { entry, bundle } => v.push(format!(
                "script {entry} (bundle {}, {}, {})",
                bundle.root.as_deref().unwrap_or_default(),
                count(bundle.files),
                cached(bundle)
            )),
        }
        v.extend(self.result.iter().flat_map(RunResult::lines));
        let o = &self.outputs;
        match &o.dir {
            Some(dir) if !o.files.is_empty() => {
                v.push(format!("outputs: {dir} ({})", count(o.files.len())));
                let w = o.files.iter().map(|f| f.name.len()).max().unwrap_or(0) + 2;
                v.extend(o.files.iter().map(|f| format!("  {:<w$}{}", f.name, Size(f.bytes))));
            }
            _ => v.push("outputs: none".into()),
        }
        if !o.missing.is_empty() {
            v.push(format!("missing: {}", o.missing.join(", ")));
        }
        v
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
