// SPDX-License-Identifier: Apache-2.0
//! One report value per call, rendered as text or as one JSON object (`t32ctl.report/1`), so the
//! two renderings cannot disagree.
//!
//! Text, in this order: the header (connecting calls only), `! ` notices, body lines,
//! `error[<tag>]: ` with indented detail lines (never for exit 1), and `next: ` hints.

use std::fmt;

use serde::{Serialize, Serializer};

use crate::{
    error::{CtlError, Exit},
    notation::shell_word,
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
    Status(StatusBody),
}

impl Body {
    /// Exit 0 or 1: whether the command ended as asked.
    pub fn outcome(&self) -> Exit {
        match self {
            Body::Status(_) => Exit::Ok,
        }
    }

    pub fn lines(&self) -> Vec<String> {
        match self {
            Body::Status(s) => s.lines(),
        }
    }
}

fn labeled(label: &str, rest: impl fmt::Display) -> String {
    format!("{label:<7} {rest}")
}

/// `read status`: PowerView, and with `--full` the binding, the config and the state directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatusBody {
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
        let mut v = Vec::new();
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
    pub profile: Option<Sourced>,
    /// `BOARD` from the build's `CMakeCache.txt`.
    pub board: Option<String>,
}

impl fmt::Display for BindingView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (name, v)) in [("build", &self.build), ("profile", &self.profile)].into_iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            match v {
                Some(s) => write!(f, "{name}={} ({})", s.path, s.source)?,
                None => write!(f, "{name} none")?,
            }
        }
        match &self.board {
            Some(b) => write!(f, ", BOARD={b}"),
            None => f.write_str(", BOARD none"),
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

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
