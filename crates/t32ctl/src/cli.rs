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
use t32stage::RegName;

use crate::{
    error::{CtlError, ErrorKind},
    grade::{Grade, GradeId},
    notation::{NotationError, OutName, Place, PracticeText, ScriptTimeout, Size, reg_name},
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
    /// Run-grade board procedures: the target may run, no target data is written
    Run(RunCmd),
    /// Write-grade board procedures: target memory and registers
    Write(WriteCmd),
    /// Sys-grade board procedures, such as DDR or clock setup
    Sys(SysCmd),
    /// Burn-grade board procedures: persistent storage; always asks
    Burn(BurnCmd),
    /// PRACTICE text and scripts outside every policy; always asks
    Raw(RawCmd),
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
    /// Header, target and CPU check, profile, PowerView; `--full` adds `profile=<hash>`, the binding, the config,
    /// and the state directory
    Status(ReadStatus),
    /// Memory as `0x` cells, or into a file in the output directory
    Mem(ReadMem),
    /// Registers of PowerView's selected core; no name: the general-purpose set
    Reg(ReadReg),
    /// Board procedures, grade, parameters, `profile=<hash>`
    Procs(ReadProcs),
    /// Static command schema
    Describe(ReadDescribe),
    /// Run a read-grade board procedure
    Proc(ProcArgs),
}

#[derive(Debug, Args)]
pub struct ReadStatus {
    /// Add the profile hash, the binding, the config, and the state directory
    #[arg(long)]
    pub full: bool,
}

#[derive(Debug, Args)]
pub struct ReadMem {
    /// `[CLASS:]0x<hex>`, or `@<reg>` with an optional `+0x<hex>` or `-0x<hex>` offset
    #[arg(value_name = "LOC")]
    pub loc: Place,
    /// Bytes to read; default one cell. At most 4K without --to, at most [calls] transfer_max with it
    #[arg(long, value_name = "SIZE")]
    pub len: Option<Size>,
    /// Access width in bytes; default the touched mmio regions' width, else the profile's word
    #[arg(long, value_enum)]
    pub width: Option<Width>,
    /// Write the bytes to a new file of this name in a fresh output directory instead
    #[arg(long, value_name = "NAME")]
    pub to: Option<OutName>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Width {
    #[value(name = "1")]
    W1,
    #[value(name = "2")]
    W2,
    #[value(name = "4")]
    W4,
    #[value(name = "8")]
    W8,
}

impl Width {
    pub fn bytes(self) -> u8 {
        match self {
            Width::W1 => 1,
            Width::W2 => 2,
            Width::W4 => 4,
            Width::W8 => 8,
        }
    }
}

#[derive(Debug, Args)]
pub struct ReadReg {
    /// Register names as PowerView spells them, in any case: `x0`, `sp`, `esr_el1`
    #[arg(value_name = "NAME", value_parser = reg_name)]
    pub names: Vec<RegName>,
}

#[derive(Debug, Args)]
pub struct ReadProcs {
    /// A procedure: its parameters, outputs, effects and usage
    #[arg(value_name = "NAME")]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReadDescribe {
    /// `json`: the schema (t32ctl.describe/1); `skill`: the generated Markdown blocks;
    /// `permissions`: the Claude Code permission rules
    #[arg(long, value_enum, default_value_t = DescribeFormat::Json)]
    pub format: DescribeFormat,
}

/// A procedure call: the name and the tokens after it, which a second parser matches against the
/// profile's declaration (`procs::split`). `--json`, `--build` and `--help` after the name are
/// taken out there.
#[derive(Debug, Args)]
pub struct ProcArgs {
    /// The procedure, as `t32ctl read procs` lists it
    #[arg(value_name = "NAME")]
    pub name: String,
    /// `--PARAM VALUE` as the profile declares them
    #[arg(value_name = "PARAM", trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub tokens: Vec<String>,
}

/// A grade whose only leaf in this t32ctl is `proc`.
macro_rules! proc_grade {
    ($cmd:ident, $leaf:ident, $doc:literal) => {
        #[derive(Debug, Args)]
        pub struct $cmd {
            #[command(flatten)]
            pub globals: Globals,
            #[command(subcommand)]
            pub leaf: $leaf,
        }

        #[derive(Debug, Subcommand)]
        pub enum $leaf {
            #[doc = $doc]
            Proc(ProcArgs),
        }
    };
}

proc_grade!(RunCmd, RunLeaf, "Run a run-grade board procedure");
proc_grade!(WriteCmd, WriteLeaf, "Run a write-grade board procedure");
proc_grade!(SysCmd, SysLeaf, "Run a sys-grade board procedure");
proc_grade!(BurnCmd, BurnLeaf, "Run a burn-grade board procedure (persistent storage)");

#[derive(Debug, Args)]
pub struct RawCmd {
    #[command(flatten)]
    pub globals: Globals,
    #[command(subcommand)]
    pub leaf: RawLeaf,
}

#[derive(Debug, Subcommand)]
pub enum RawLeaf {
    /// One PRACTICE command
    Cmd(RawCommand),
    /// One PRACTICE function
    Eval(RawEval),
    /// Run a `.cmm` set from the repository, collect outputs
    Script(RawScript),
}

#[derive(Debug, Args)]
pub struct RawCommand {
    /// The command, sent literally: PowerView expands no macros in it
    #[arg(value_name = "'TEXT'")]
    pub text: PracticeText,
}

#[derive(Debug, Args)]
pub struct RawEval {
    /// The function, sent literally: PowerView expands no macros in it, and a file argument is a
    /// literal name
    #[arg(value_name = "'EXPR'")]
    pub text: PracticeText,
}

#[derive(Debug, Args)]
pub struct RawScript {
    /// The entry script, a local file below the root
    #[arg(value_name = "ENTRY.cmm")]
    pub entry: PathBuf,
    /// The directory bundled with the entry; default the entry's directory
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,
    /// Bound of the run, 1s to 2h
    #[arg(long, value_name = "DUR", default_value = "5m")]
    pub timeout: ScriptTimeout,
    /// A new local directory for the outputs; default a fresh one in t32ctl's output directory
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Arguments for the entry script's ENTRY line: letters, digits and _ - . / : + = , @
    #[arg(value_name = "ARG", last = true)]
    pub args: Vec<String>,
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
leaf!(Read, ReadMem, path: ["read", "mem"], waits: "`--to`: the transfer, bounded", connects: true,
      examples: ["t32ctl read mem @sp --len 64", "t32ctl read mem A:0x40000000 --len 16",
                 "t32ctl read mem 0x40000000 --len 64M --to ddr.bin"]);
leaf!(Read, ReadReg, path: ["read", "reg"], waits: "-", connects: true,
      examples: ["t32ctl read reg", "t32ctl read reg x0 sp", "t32ctl read reg esr_el1 far_el1 --json"]);
leaf!(Read, ReadProcs, path: ["read", "procs"], waits: "no connection", connects: false,
      examples: ["t32ctl read procs", "t32ctl read procs flash", "t32ctl read procs --json"]);
leaf!(Read, ReadDescribe, path: ["read", "describe"], waits: "no connection", connects: false,
      examples: ["t32ctl read describe", "t32ctl read describe --format permissions"]);

leaf!(Read, ProcArgs, path: ["read", "proc"], waits: "the script", connects: true,
      examples: ["t32ctl read proc selftest", "t32ctl read proc selftest --json"],
      usage_tail: "NAME [--PARAM VALUE...]");
leaf!(Run, ProcArgs, path: ["run", "proc"], waits: "the script", connects: true,
      examples: ["t32ctl run proc attach"], usage_tail: "NAME [--PARAM VALUE...]");
leaf!(Write, ProcArgs, path: ["write", "proc"], waits: "the script", connects: true,
      examples: ["t32ctl write proc reset"], usage_tail: "NAME [--PARAM VALUE...]");
leaf!(Sys, ProcArgs, path: ["sys", "proc"], waits: "the script", connects: true,
      examples: ["t32ctl sys proc ddr_init --speed 1600"], usage_tail: "NAME [--PARAM VALUE...]");
leaf!(Burn, ProcArgs, path: ["burn", "proc"], waits: "the script", connects: true,
      examples: ["t32ctl burn proc flash --part user", "t32ctl burn proc flash --image build/zephyr/zephyr.bin --json"],
      usage_tail: "NAME [--PARAM VALUE...]");

leaf!(Raw, RawCommand, path: ["raw", "cmd"], waits: "the command", connects: true,
      examples: ["t32ctl raw cmd 'PRINT \"hello\"'", "t32ctl raw cmd 'SYStem.Mode Attach' --json"]);
leaf!(Raw, RawEval, path: ["raw", "eval"], waits: "the function", connects: true,
      examples: ["t32ctl raw eval 'OS.PTD()'", "t32ctl raw eval 'Register(X0)' --json"]);
leaf!(Raw, RawScript, path: ["raw", "script"], waits: "the script", connects: true,
      examples: ["t32ctl raw script tools/t32/ddr_stress.cmm -- 0x40000000 0x100000",
                 "t32ctl raw script tools/t32/dump.cmm --timeout 30s --out dump-1 --json"]);

leaves! {
    (Read, ReadStatus),
    (Read, ReadMem),
    (Read, ReadReg),
    (Read, ReadProcs),
    (Read, ReadDescribe),
    (Read, ProcArgs),
    (Run, ProcArgs),
    (Write, ProcArgs),
    (Sys, ProcArgs),
    (Burn, ProcArgs),
    (Raw, RawCommand),
    (Raw, RawEval),
    (Raw, RawScript),
}

impl Cli {
    /// The arguments of a `proc` leaf, under any grade.
    pub fn proc_args_mut(&mut self) -> Option<&mut ProcArgs> {
        match &mut self.grade {
            GradeCmd::Read(ReadCmd { leaf: ReadLeaf::Proc(a), .. })
            | GradeCmd::Run(RunCmd { leaf: RunLeaf::Proc(a), .. })
            | GradeCmd::Write(WriteCmd { leaf: WriteLeaf::Proc(a), .. })
            | GradeCmd::Sys(SysCmd { leaf: SysLeaf::Proc(a), .. })
            | GradeCmd::Burn(BurnCmd { leaf: BurnLeaf::Proc(a), .. }) => Some(a),
            _ => None,
        }
    }

    /// The spec of the leaf this command line names.
    pub fn spec(&self) -> &'static LeafSpec {
        use crate::grade::{Burn, Raw, Read, Run, Sys, Write};
        match &self.grade {
            GradeCmd::Read(g) => match g.leaf {
                ReadLeaf::Status(_) => &<ReadStatus as Leaf<Read>>::SPEC,
                ReadLeaf::Mem(_) => &<ReadMem as Leaf<Read>>::SPEC,
                ReadLeaf::Reg(_) => &<ReadReg as Leaf<Read>>::SPEC,
                ReadLeaf::Procs(_) => &<ReadProcs as Leaf<Read>>::SPEC,
                ReadLeaf::Describe(_) => &<ReadDescribe as Leaf<Read>>::SPEC,
                ReadLeaf::Proc(_) => &<ProcArgs as Leaf<Read>>::SPEC,
            },
            GradeCmd::Run(g) => match g.leaf {
                RunLeaf::Proc(_) => &<ProcArgs as Leaf<Run>>::SPEC,
            },
            GradeCmd::Write(g) => match g.leaf {
                WriteLeaf::Proc(_) => &<ProcArgs as Leaf<Write>>::SPEC,
            },
            GradeCmd::Sys(g) => match g.leaf {
                SysLeaf::Proc(_) => &<ProcArgs as Leaf<Sys>>::SPEC,
            },
            GradeCmd::Burn(g) => match g.leaf {
                BurnLeaf::Proc(_) => &<ProcArgs as Leaf<Burn>>::SPEC,
            },
            GradeCmd::Raw(g) => match g.leaf {
                RawLeaf::Cmd(_) => &<RawCommand as Leaf<Raw>>::SPEC,
                RawLeaf::Eval(_) => &<RawEval as Leaf<Raw>>::SPEC,
                RawLeaf::Script(_) => &<RawScript as Leaf<Raw>>::SPEC,
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

/// `--help` after a procedure's name is the procedure's help: clap takes it when it follows the
/// name directly, so a command line that is a `proc` call without it gets it back as a token.
pub fn parse(argv: &[OsString]) -> Parsed {
    let parse_cli = |argv: &[OsString]| command().try_get_matches_from(argv).and_then(|m| Cli::from_arg_matches(&m));
    match parse_cli(argv) {
        Ok(cli) => Parsed::Cli(cli),
        Err(e) if e.kind() == ClapKind::DisplayHelp => {
            let without: Vec<OsString> = argv.iter().filter(|a| *a != "--help").cloned().collect();
            if let Ok(mut cli) = parse_cli(&without)
                && let Some(args) = cli.proc_args_mut()
            {
                args.tokens.push("--help".into());
                return Parsed::Cli(cli);
            }
            Parsed::Print(e.render().to_string())
        }
        Err(e) if e.kind() == ClapKind::DisplayVersion => Parsed::Print(e.render().to_string()),
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
