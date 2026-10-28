// SPDX-License-Identifier: Apache-2.0
//! The command tree: the clap derive is the only list of commands. Each leaf's argument struct
//! carries its `LeafSpec` through `leaf!`, and dispatch requires `Leaf<G>`, so a leaf without a
//! spec does not compile. `--help`, `read describe`, the generated blocks and the permission
//! snippet all come from this definition.
//!
//! Flags exist only after the grade word, and there are no aliases or abbreviations, so the first
//! two words of a command line decide its permission rule.

use std::{ffi::OsString, path::PathBuf};

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum, error::ErrorKind as ClapKind};

use crate::{
    error::{CtlError, ErrorKind},
    grade::{Grade, GradeId},
    notation::NotationError,
};

/// TRACE32 control over RCL for coding agents. The first word is the permission grade.
#[derive(Debug, Parser)]
#[command(name = "t32ctl", version, disable_help_subcommand = true)]
pub struct Cli {
    #[command(subcommand)]
    pub grade: GradeCmd,
}

#[derive(Debug, Subcommand)]
pub enum GradeCmd {
    /// Read target and debugger state; nothing changes
    Read(ReadCmd),
}

/// Flags every leaf of a grade accepts.
#[derive(Clone, Debug, Args)]
pub struct Globals {
    /// Print one JSON object (schema t32ctl.report/1) instead of text
    #[arg(long, global = true)]
    pub json: bool,
    /// The build directory, relative to the current directory; overrides the binding
    #[arg(long, value_name = "DIR", global = true)]
    pub build: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReadCmd {
    #[command(flatten)]
    pub globals: Globals,
    #[command(subcommand)]
    pub leaf: ReadLeaf,
}

#[derive(Debug, Subcommand)]
pub enum ReadLeaf {
    /// Header and PowerView; `--full` adds the binding, the config, and the state directory
    Status(ReadStatus),
    /// Static command schema
    Describe(ReadDescribe),
}

#[derive(Debug, Args)]
pub struct ReadStatus {
    /// Add the binding, the config, and the state directory
    #[arg(long)]
    pub full: bool,
}

#[derive(Debug, Args)]
pub struct ReadDescribe {
    /// `json`: the schema (t32ctl.describe/1); `skill`: the generated Markdown blocks;
    /// `permissions`: the Claude Code permission rules
    #[arg(long, value_enum, default_value_t = DescribeFormat::Json)]
    pub format: DescribeFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DescribeFormat {
    Json,
    Skill,
    Permissions,
}

/// What the definition says about a leaf besides its arguments.
#[derive(Debug, PartialEq, Eq)]
pub struct LeafSpec {
    pub grade: GradeId,
    /// The words after `t32ctl`: `["read", "status"]`.
    pub path: &'static [&'static str],
    /// The "Waits for" column; `-` when the leaf waits for nothing in particular.
    pub waits: &'static str,
    pub connects: bool,
    /// Complete command lines, shown by `--help` and `read describe`.
    pub examples: &'static [&'static str],
    /// Usage text after the arguments clap knows; only `proc` has one.
    pub usage_tail: Option<&'static str>,
}

/// A leaf under grade `G`. Implemented only through `leaf!`.
pub trait Leaf<G: Grade>: Args {
    const SPEC: LeafSpec;
}

macro_rules! leaf {
    ($grade:ident, $ty:ty, path: $path:expr, waits: $waits:expr, connects: $connects:expr,
     examples: $examples:expr $(, usage_tail: $tail:expr)? $(,)?) => {
        impl Leaf<crate::grade::$grade> for $ty {
            const SPEC: LeafSpec = LeafSpec {
                grade: <crate::grade::$grade as Grade>::ID,
                path: &$path,
                waits: $waits,
                connects: $connects,
                examples: &$examples,
                usage_tail: leaf!(@tail $($tail)?),
            };
        }
    };
    (@tail) => { None };
    (@tail $tail:expr) => { Some($tail) };
}

macro_rules! leaves {
    ($(($grade:ident, $ty:ty)),* $(,)?) => {
        /// Every leaf's spec, in the order of the command table.
        pub const SPECS: &[&LeafSpec] = &[$(&<$ty as Leaf<crate::grade::$grade>>::SPEC),*];
    };
}

leaf!(Read, ReadStatus, path: ["read", "status"], waits: "-", connects: true,
      examples: ["t32ctl read status", "t32ctl read status --full", "t32ctl read status --json"]);
leaf!(Read, ReadDescribe, path: ["read", "describe"], waits: "no connection", connects: false,
      examples: ["t32ctl read describe", "t32ctl read describe --format permissions"]);

leaves! {
    (Read, ReadStatus),
    (Read, ReadDescribe),
}

impl Cli {
    /// The spec of the leaf this command line names.
    pub fn spec(&self) -> &'static LeafSpec {
        use crate::grade::Read;
        match &self.grade {
            GradeCmd::Read(g) => match g.leaf {
                ReadLeaf::Status(_) => &<ReadStatus as Leaf<Read>>::SPEC,
                ReadLeaf::Describe(_) => &<ReadDescribe as Leaf<Read>>::SPEC,
            },
        }
    }
}

/// The clap command with each leaf's examples under its `--help`. Without arguments, a grade or
/// the root is a usage error, not a help page.
pub fn command() -> clap::Command {
    SPECS.iter().fold(Cli::command(), |cmd, spec| {
        let examples = spec.examples.iter().map(|e| format!("  {e}")).collect::<Vec<_>>().join("\n");
        with_examples(cmd, spec.path, format!("Examples:\n{examples}"))
    })
}

fn with_examples(cmd: clap::Command, path: &[&str], help: String) -> clap::Command {
    match path.split_first() {
        None => cmd.after_help(help),
        Some((head, rest)) => cmd.arg_required_else_help(false).mut_subcommand(*head, |c| with_examples(c, rest, help)),
    }
}

/// A parsed command line, text clap prints for `--help` or `--version`, or a usage error.
#[derive(Debug)]
pub enum Parsed {
    Cli(Cli),
    Print(String),
    Usage(CtlError),
}

pub fn parse(argv: &[OsString]) -> Parsed {
    let parsed = command().try_get_matches_from(argv).and_then(|m| Cli::from_arg_matches(&m));
    match parsed {
        Ok(cli) => Parsed::Cli(cli),
        Err(e) if matches!(e.kind(), ClapKind::DisplayHelp | ClapKind::DisplayVersion) => {
            Parsed::Print(e.render().to_string())
        }
        Err(e) => Parsed::Usage(usage_error(&e)),
    }
}

/// clap's first line is the message and the rest are details; a value a notation type refused
/// is `notation`, with that type's message.
pub fn usage_error(e: &clap::Error) -> CtlError {
    let notation = std::error::Error::source(e).and_then(|s| s.downcast_ref::<NotationError>());
    if let Some(n) = notation {
        return CtlError::new(ErrorKind::Notation, n.0.clone());
    }
    let text = e.render().to_string();
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or_default();
    let message = first.strip_prefix("error: ").unwrap_or(first).to_string();
    CtlError::new(ErrorKind::Usage, message).with_details(lines.map(String::from).collect())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
pub(crate) mod tests;
