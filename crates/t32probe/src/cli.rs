// SPDX-License-Identifier: Apache-2.0
//! Command line. Defaults live here; validation into typed values happens in `Args::plan`, so a
//! probe never reads a flag and an opt-in gate cannot open without its input.

use std::{
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use clap::Parser;
use t32rcl::DevicePrompt;
use t32stage::{PvPath, Symbol, VmWindow};

use crate::{
    env::{Env, WslFacts},
    registry::{self, OptIn, ProbeDef, ProbeId},
};

/// Measure what a live TRACE32 PowerView does over RCL and print a report short enough to paste.
///
/// Run it in WSL2 on the PC that runs PowerView, with RCL enabled (`RCL=NETTCP` and `PORT=` in
/// `config.t32`). The default run uses only the `safe` probes.
#[derive(Debug, Parser)]
#[command(name = "t32probe", version, about, long_about = None)]
pub struct Args {
    /// PowerView host. `auto` tries 127.0.0.1, the WSL default gateway, then the resolv.conf nameserver.
    #[arg(long, default_value = "auto")]
    pub host: String,

    /// RCL port (NETTCP).
    #[arg(long, default_value_t = 20000)]
    pub port: u16,

    /// Device prompt prefixed to every command, e.g. `B::`, if the PowerView prompt was changed.
    #[arg(long)]
    pub prompt: Option<String>,

    /// Run only these probes (comma separated); their prerequisites are added.
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,

    /// Leave these probes out unless a selected probe requires them.
    #[arg(long, value_delimiter = ',')]
    pub skip: Vec<String>,

    /// Print the probe catalogue and exit.
    #[arg(long)]
    pub list: bool,

    /// With --list: the Markdown table kept in README.md.
    #[arg(long, requires = "list")]
    pub markdown: bool,

    /// Also write the full report as JSON, rewritten after every probe.
    #[arg(long, value_name = "PATH")]
    pub json: Option<PathBuf>,

    /// Idle timeout per RCL call in seconds; keepalive frames reset it.
    #[arg(long, default_value_t = 10.0)]
    pub timeout: f64,

    /// Multiply every probe and cleanup deadline, e.g. 2 on a slow PC.
    #[arg(long, default_value_t = 1.0)]
    pub scale: f64,

    /// First VM: address t32probe may use.
    #[arg(long, default_value = "0x01000000", value_parser = parse_u32)]
    pub vm_base: u32,

    /// Size of that VM: window in bytes.
    #[arg(long, default_value = "0x04000000", value_parser = parse_u32)]
    pub vm_window: u32,

    /// Largest decompressed size the gzip probe tries, in MiB.
    #[arg(long, default_value_t = 8)]
    pub gzip_max_mib: u32,

    /// Round trips for the latency probe.
    #[arg(long, default_value_t = 200)]
    pub latency_n: u32,

    /// Directory for the VM: lease and the cleanup journal (default `$XDG_STATE_HOME/t32ctl`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// PowerView-side directory to stage files in instead of `OS.PTD()\t32ctl`.
    #[arg(long, value_name = "PATH")]
    pub stage_root: Option<String>,

    /// Enable `dialog`: shows a dialog window on the PowerView PC for a moment.
    #[arg(long)]
    pub allow_dialog: bool,

    /// Enable `multi-client`: opens a second RCL connection.
    #[arg(long)]
    pub allow_multi_client: bool,

    /// Enable `run-control`: runs the target to --symbol and halts it.
    #[arg(long, requires = "symbol")]
    pub allow_run_control: bool,

    /// Symbol for `run-control` and `target-read`, and the symbol queries after `elf-unzip`.
    #[arg(long)]
    pub symbol: Option<String>,

    /// With `run-control`: a source line `<file>:<line>` inside a loop, for a `/COUNT` breakpoint.
    #[arg(long, value_name = "FILE:LINE", requires = "allow_run_control")]
    pub loop_line: Option<String>,

    /// With `run-control`: a variable to set write breakpoints on.
    #[arg(long, value_name = "VAR", requires = "allow_run_control")]
    pub watch: Option<String>,

    /// With `run-control`: a 4-byte aligned RAM address the probe may write and restore.
    #[arg(long, value_name = "ADDR", requires = "allow_run_control")]
    pub scratch_ram: Option<String>,

    /// Enable `target-read`: reads registers, memory and symbols of a halted target and prints
    /// windows to a file. Leave no PowerView printer file open while it runs.
    #[arg(long, requires = "symbol")]
    pub allow_target_read: bool,

    /// With `target-read`: a virtual address to translate.
    #[arg(long, value_name = "ADDR", requires = "allow_target_read")]
    pub va: Option<String>,

    /// With `target-read`: a peripheral section name for `WinPrint.PER.view`.
    #[arg(long, value_name = "NAME", requires = "allow_target_read")]
    pub per_section: Option<String>,

    /// With `target-read`: `<addr>++<len>` of target memory to read over RCL and save to a file,
    /// at most 16 MiB.
    #[arg(long, value_name = "ADDR++LEN", requires = "allow_target_read")]
    pub read_range: Option<String>,

    /// Enable `elf-unzip` with this ELF. Loads its symbols (not its code) into PowerView.
    #[arg(long, value_name = "PATH")]
    pub elf: Option<PathBuf>,

    /// Enable `load-code`: loads the code of --elf into the halted target, then restores the PC.
    #[arg(long, requires = "elf")]
    pub allow_load_code: bool,
}

/// The ELF, read once at start so a missing file is a usage error, not a probe failure.
#[derive(Clone, Debug)]
pub struct ElfInput {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub symbol: Option<Symbol>,
}

/// What `PvPath` refuses inside a path; a blank is refused too.
const LINE_FILE_FORBIDDEN: &[char] = &['"', '&', '%', ';', '*', '?', '<', '>', '|', '`'];

/// A source line for a breakpoint: `<file>\<line>` in PRACTICE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLine {
    pub file: String,
    pub line: u32,
}

impl SourceLine {
    /// `<file>:<line>`, split at the last `:` so a drive letter stays in the file. The file goes
    /// unquoted into a command: no blank and none of the characters a stage path refuses.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (file, line) = text.rsplit_once(':').ok_or("expected <file>:<line>")?;
        if file.is_empty() {
            return Err("the file is empty".into());
        }
        if let Some(bad) = file.chars().find(|c| !('!'..='~').contains(c) || LINE_FILE_FORBIDDEN.contains(c)) {
            return Err(format!("the file contains {bad:?}"));
        }
        match line.parse::<u32>() {
            Ok(line) if line >= 1 => Ok(Self { file: file.to_string(), line }),
            _ => Err(format!("line {line:?} is not a number of at least 1")),
        }
    }
}

/// `run-control`'s inputs: the symbol to run to and the optional steps' operator-chosen targets.
#[derive(Clone, Debug)]
pub struct RunControlInput {
    pub symbol: Symbol,
    pub loop_line: Option<SourceLine>,
    pub watch: Option<Symbol>,
    pub scratch_ram: Option<u64>,
}

/// `target-read`'s inputs.
#[derive(Clone, Debug)]
pub struct TargetReadInput {
    pub symbol: Symbol,
    pub va: Option<u64>,
    pub per_section: Option<String>,
    /// Start and length.
    pub read_range: Option<(u64, u32)>,
}

/// Largest `--read-range` length.
const READ_RANGE_MAX: u32 = 16 * 1024 * 1024;

fn parse_section(s: &str) -> Result<String, String> {
    if (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.') {
        Ok(s.to_string())
    } else {
        Err(format!("{s:?} is not [A-Za-z0-9_.]{{1,64}}"))
    }
}

fn parse_range(s: &str) -> Result<(u64, u32), String> {
    let (a, n) = s.split_once("++").ok_or("expected <addr>++<len>")?;
    let n = parse_u64(n)?;
    if !(1..=u64::from(READ_RANGE_MAX)).contains(&n) {
        return Err(format!("length {n} is not between 1 and {READ_RANGE_MAX}"));
    }
    Ok((parse_u64(a)?, n as u32))
}

/// What the opt-in flags unlocked, with the inputs those probes need. A gate is open iff its
/// field is set, so "unlocked but missing its input" is not representable.
#[derive(Clone, Debug, Default)]
pub struct Gates {
    pub dialog: bool,
    pub multi_client: bool,
    pub run_control: Option<RunControlInput>,
    pub target_read: Option<TargetReadInput>,
    pub load_code: Option<ElfInput>,
    pub elf: Option<ElfInput>,
}

impl Gates {
    pub fn is_open(&self, o: OptIn) -> bool {
        match o {
            OptIn::Dialog => self.dialog,
            OptIn::MultiClient => self.multi_client,
            OptIn::RunControl => self.run_control.is_some(),
            OptIn::TargetRead => self.target_read.is_some(),
            OptIn::LoadCode => self.load_code.is_some(),
            OptIn::Elf => self.elf.is_some(),
        }
    }
}

/// Upper bounds that keep every derived `Duration` finite; NaN fails the range checks too.
const MAX_TIMEOUT_S: f64 = 3600.0;
const MAX_SCALE: f64 = 100.0;

fn parse_u32(s: &str) -> Result<u32, String> {
    let (digits, radix) = s.strip_prefix("0x").map_or((s, 10), |h| (h, 16));
    u32::from_str_radix(&digits.replace('_', ""), radix).map_err(|e| e.to_string())
}

/// The JSON report is written to `<path>.tmp` and renamed; prove the first step works before
/// anything is sent. A `.tmp` that already exists is opened, never replaced or removed.
fn check_json_dest(path: &Path) -> Result<(), String> {
    let tmp = crate::report::json_tmp(path);
    let err = |e: io::Error| format!("--json {}: {e}", path.display());
    match OpenOptions::new().write(true).create_new(true).open(&tmp) {
        Ok(_) => fs::remove_file(&tmp).map_err(err),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            OpenOptions::new().write(true).open(&tmp).map(drop).map_err(err)
        }
        Err(e) => Err(err(e)),
    }
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let (digits, radix) = s.strip_prefix("0x").map_or((s, 10), |h| (h, 16));
    u64::from_str_radix(&digits.replace('_', ""), radix).map_err(|e| e.to_string())
}

fn parse_ram(s: &str) -> Result<u64, String> {
    let a = parse_u64(s)?;
    if a % 4 != 0 {
        return Err(format!("{s} is not 4-byte aligned"));
    }
    Ok(a)
}

fn ids(names: &[String]) -> Result<Vec<ProbeId>, String> {
    names
        .iter()
        .map(|n| {
            ProbeId::parse(n).ok_or_else(|| {
                let all: Vec<_> = registry::ALL.iter().map(|d| d.id.name()).collect();
                format!("unknown probe {n:?}; known: {}", all.join(" "))
            })
        })
        .collect()
}

impl Args {
    /// Validate everything before anything is sent.
    pub fn plan(&self, wsl: WslFacts, run_id: String) -> Result<(Env, Vec<&'static ProbeDef>), String> {
        let symbol = self.symbol.as_deref().map(Symbol::parse).transpose().map_err(|e| format!("--symbol: {e}"))?;
        let elf = match &self.elf {
            Some(path) => {
                let bytes = std::fs::read(path).map_err(|e| format!("--elf {}: {e}", path.display()))?;
                Some(ElfInput { path: path.clone(), bytes, symbol: symbol.clone() })
            }
            None => None,
        };
        let run_control = match &symbol {
            Some(symbol) if self.allow_run_control => Some(RunControlInput {
                symbol: symbol.clone(),
                loop_line: self
                    .loop_line
                    .as_deref()
                    .map(SourceLine::parse)
                    .transpose()
                    .map_err(|e| format!("--loop-line: {e}"))?,
                watch: self.watch.as_deref().map(Symbol::parse).transpose().map_err(|e| format!("--watch: {e}"))?,
                scratch_ram: self
                    .scratch_ram
                    .as_deref()
                    .map(parse_ram)
                    .transpose()
                    .map_err(|e| format!("--scratch-ram: {e}"))?,
            }),
            _ => None,
        };
        let target_read = match &symbol {
            Some(symbol) if self.allow_target_read => Some(TargetReadInput {
                symbol: symbol.clone(),
                va: self.va.as_deref().map(parse_u64).transpose().map_err(|e| format!("--va: {e}"))?,
                per_section: self
                    .per_section
                    .as_deref()
                    .map(parse_section)
                    .transpose()
                    .map_err(|e| format!("--per-section: {e}"))?,
                read_range: self
                    .read_range
                    .as_deref()
                    .map(parse_range)
                    .transpose()
                    .map_err(|e| format!("--read-range: {e}"))?,
            }),
            _ => None,
        };
        let gates = Gates {
            dialog: self.allow_dialog,
            multi_client: self.allow_multi_client,
            run_control,
            target_read,
            load_code: if self.allow_load_code { elf.clone() } else { None },
            elf,
        };
        let selected = registry::select(&ids(&self.only)?, &ids(&self.skip)?, &gates)?;
        let window = VmWindow::new(self.vm_base, self.vm_window).map_err(|e| format!("--vm-base/--vm-window: {e}"))?;
        if window.len() < 8 * 1024 * 1024 {
            return Err("--vm-window must be at least 8 MiB".into());
        }
        let prompt =
            self.prompt.as_deref().map(DevicePrompt::parse).transpose().map_err(|e| format!("--prompt: {e}"))?;
        let stage_root =
            self.stage_root.as_deref().map(PvPath::parse).transpose().map_err(|e| format!("--stage-root: {e}"))?;
        let state_dir = self
            .state_dir
            .clone()
            .or_else(t32stage::default_state_dir)
            .ok_or("no state directory: set --state-dir, XDG_STATE_HOME or HOME")?;
        if !(self.timeout > 0.0 && self.timeout <= MAX_TIMEOUT_S) {
            return Err(format!("--timeout must be a number of seconds above 0 and at most {MAX_TIMEOUT_S}"));
        }
        if !(self.scale > 0.0 && self.scale <= MAX_SCALE) {
            return Err(format!("--scale must be above 0 and at most {MAX_SCALE}"));
        }
        if let Some(path) = &self.json {
            check_json_dest(path)?;
        }
        let hosts = if self.host == "auto" { wsl.candidates() } else { vec![self.host.clone()] };
        let env = Env::new(crate::env::EnvConfig {
            run_id,
            hosts,
            port: self.port,
            prompt,
            idle: Duration::from_secs_f64(self.timeout),
            scale: self.scale,
            vm_window: window,
            state_dir,
            stage_root,
            gzip_max: self.gzip_max_mib.saturating_mul(1024 * 1024),
            latency_n: self.latency_n.max(1),
            gates,
            wsl,
        });
        Ok((env, selected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(extra: &[&str]) -> Result<(), String> {
        let state = std::env::temp_dir().join("t32probe-cli-never-created");
        let mut argv = vec!["t32probe", "--state-dir", state.to_str().expect("utf-8 temp dir")];
        argv.extend_from_slice(extra);
        Args::try_parse_from(argv).expect("parses").plan(WslFacts::default(), "r1".into()).map(drop)
    }

    #[test]
    fn a_non_finite_or_huge_timeout_or_scale_is_a_usage_error() {
        for bad in
            [["--scale", "inf"], ["--scale", "1e30"], ["--scale", "0"], ["--timeout", "inf"], ["--timeout", "NaN"]]
        {
            assert!(plan(&bad).is_err(), "{bad:?}");
        }
        assert!(plan(&["--timeout", "1e30"]).is_err());
        assert!(plan(&["--scale", "2", "--timeout", "20"]).is_ok());
    }

    #[test]
    fn scratch_ram_and_loop_line_are_validated_before_anything_is_sent() {
        let rc = |extra: &[&str]| {
            let mut v = vec!["--allow-run-control", "--symbol", "main"];
            v.extend_from_slice(extra);
            plan(&v)
        };
        assert_eq!(rc(&[]), Ok(()));
        assert_eq!(
            rc(&["--loop-line", "C:\\src\\main.c:12", "--watch", "counter", "--scratch-ram", "0x80001000"]),
            Ok(())
        );
        for bad in [
            ["--scratch-ram", "0x80001002"],
            ["--scratch-ram", "nope"],
            ["--loop-line", "main.c"],
            ["--loop-line", "main.c:0"],
            ["--loop-line", "main.c:x"],
            ["--loop-line", ":12"],
            ["--loop-line", "my file.c:12"],
            ["--loop-line", "a;b.c:12"],
            ["--watch", "1x"],
        ] {
            let e = rc(&bad).unwrap_err();
            assert!(e.starts_with(bad[0]), "{bad:?}: {e}");
        }
        let parsed = Args::try_parse_from(["t32probe", "--loop-line", "main.c:3"]);
        assert!(parsed.is_err(), "--loop-line needs --allow-run-control");
    }

    #[test]
    fn target_read_and_load_code_need_their_gates() {
        assert_eq!(plan(&["--only", "target-read"]), Err("target-read needs --allow-target-read --symbol <s>".into()));
        assert!(Args::try_parse_from(["t32probe", "--allow-target-read"]).is_err(), "needs --symbol");
        assert_eq!(plan(&["--only", "load-code"]), Err("load-code needs --allow-load-code --elf <path>".into()));
        assert!(Args::try_parse_from(["t32probe", "--allow-load-code"]).is_err(), "needs --elf");
        let tr = |extra: &[&str]| {
            let mut v = vec!["--only", "target-read", "--allow-target-read", "--symbol", "main"];
            v.extend_from_slice(extra);
            plan(&v)
        };
        assert_eq!(
            tr(&["--va", "0xFFFF000000001000", "--per-section", "GIC.D", "--read-range", "0x80000000++0x1000"]),
            Ok(())
        );
        for bad in [
            ["--read-range", "0x80000000++0x1000001"],
            ["--read-range", "0x80000000++0"],
            ["--read-range", "0x80000000"],
            ["--per-section", "a b"],
            ["--va", "zz"],
        ] {
            let e = tr(&bad).unwrap_err();
            assert!(e.starts_with(bad[0]), "{bad:?}: {e}");
        }
    }

    #[test]
    fn an_unwritable_json_path_is_a_usage_error_and_a_writable_one_leaves_nothing() {
        let e = plan(&["--json", "/nonexistent-t32probe-dir/p.json"]).unwrap_err();
        assert!(e.starts_with("--json /nonexistent-t32probe-dir/p.json: "), "{e}");
        let dir = std::env::temp_dir().join(format!("t32probe-cli-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = dir.join("p.json");
        assert_eq!(plan(&["--json", json.to_str().unwrap()]), Ok(()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "the check leaves no file behind");
        std::fs::remove_dir(&dir).unwrap();
    }
}
