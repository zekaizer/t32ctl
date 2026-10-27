// SPDX-License-Identifier: Apache-2.0
//! Command line. Defaults live here; validation into typed values happens in `Args::plan`, so a
//! probe never reads a flag and an opt-in gate cannot open without its input.

use std::{path::PathBuf, time::Duration};

use clap::Parser;
use t32rcl::DevicePrompt;
use t32stage::{PvPath, Symbol, VmWindow};

use crate::{
    env::{Env, WslFacts},
    registry::{self, OptIn, ProbeDef, ProbeId},
};

/// Measure what a live TRACE32 PowerView does over RCL and print a report short enough to paste.
///
/// Run it in WSL2 on the PC that runs PowerView, with RCL enabled
/// (`SETUP.API.RCL.Enable /PRotocol TCP`). The default run uses only the `safe` probes.
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

    /// Symbol for `run-control`, and the existence check after `elf-unzip`.
    #[arg(long)]
    pub symbol: Option<String>,

    /// Enable `elf-unzip` with this ELF. Loads its symbols (not its code) into PowerView.
    #[arg(long, value_name = "PATH")]
    pub elf: Option<PathBuf>,
}

/// The ELF, read once at start so a missing file is a usage error, not a probe failure.
#[derive(Clone, Debug)]
pub struct ElfInput {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub symbol: Option<Symbol>,
}

/// What the opt-in flags unlocked, with the inputs those probes need. A gate is open iff its
/// field is set, so "unlocked but missing its input" is not representable.
#[derive(Clone, Debug, Default)]
pub struct Gates {
    pub dialog: bool,
    pub multi_client: bool,
    pub run_control: Option<Symbol>,
    pub elf: Option<ElfInput>,
}

impl Gates {
    pub fn is_open(&self, o: OptIn) -> bool {
        match o {
            OptIn::Dialog => self.dialog,
            OptIn::MultiClient => self.multi_client,
            OptIn::RunControl => self.run_control.is_some(),
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
        let gates = Gates {
            dialog: self.allow_dialog,
            multi_client: self.allow_multi_client,
            run_control: if self.allow_run_control { symbol } else { None },
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
}
