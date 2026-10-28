// SPDX-License-Identifier: Apache-2.0
//! The run environment: validated configuration plus facts learned along the way.

use std::{net::Ipv4Addr, path::PathBuf, process::Command, time::Duration};

use serde::Serialize;
use t32rcl::{CallBudget, Cancel, ConnectOpts, DevicePrompt, Endpoint, MacroSpelling, PowerViewBuild, Tuning};
use t32stage::{ExistsFn, PvPath, VmWindow, host};

use crate::cli::Gates;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WslMode {
    Mirrored,
    Nat,
    #[default]
    Unknown,
}

impl WslMode {
    pub fn tag(self) -> &'static str {
        match self {
            WslMode::Mirrored => "mirrored",
            WslMode::Nat => "nat",
            WslMode::Unknown => "unknown",
        }
    }
}

/// How WSL reaches Windows. Read from Linux-side files and `wslinfo`; parsing is pure.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WslFacts {
    pub kernel: Option<String>,
    pub is_wsl: bool,
    pub mode: WslMode,
    pub default_gateway: Option<String>,
    pub nameserver: Option<String>,
}

impl WslFacts {
    /// Never fails: anything unreadable stays unknown.
    pub fn read() -> Self {
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|s| s.trim().to_string());
        let is_wsl = kernel.as_deref().is_some_and(|k| k.to_ascii_lowercase().contains("microsoft"));
        let default_gateway = host::read_default_gateway().map(|g| g.to_string());
        let nameserver = host::read_nameserver();
        let reported = Command::new("wslinfo")
            .arg("--networking-mode")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let mode = mode_from(reported.as_deref(), is_wsl, default_gateway.as_deref());
        Self { kernel, is_wsl, mode, default_gateway, nameserver }
    }

    /// Hosts for `--host auto`, in order, without duplicates.
    pub fn candidates(&self) -> Vec<String> {
        host::auto_candidates(self.default_gateway.as_deref(), self.nameserver.as_deref())
    }
}

/// `wslinfo` output when present; otherwise a NAT guess from a 172.16/12 gateway.
fn mode_from(reported: Option<&str>, is_wsl: bool, gateway: Option<&str>) -> WslMode {
    match reported {
        Some("mirrored") => return WslMode::Mirrored,
        Some("nat") => return WslMode::Nat,
        _ => {}
    }
    let private_nat = gateway.and_then(|g| g.parse::<Ipv4Addr>().ok()).is_some_and(|ip| {
        let [a, b, ..] = ip.octets();
        a == 172 && (16..32).contains(&b)
    });
    if is_wsl && private_nat { WslMode::Nat } else { WslMode::Unknown }
}

/// Fixed part of the cleanup budget after each probe, independent of the probe's own deadline.
pub const CLEANUP_BASE: Duration = Duration::from_secs(30);
/// Slowest RCL memory throughput the cleanup budget plans for, in bytes per second.
const RESTORE_FLOOR: f64 = 1e6;

/// How a Lab script hands back a value besides the file oracle. script-lab decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptOut {
    /// `Data.Set VM:<slot> "<text>" 0`, read over RCL.
    VmSet,
    /// `OPEN #8`, `WRITE #8`, `CLOSE #8` into a file the client loads.
    Write,
    Off,
}

impl ScriptOut {
    pub fn tag(self) -> &'static str {
        match self {
            ScriptOut::VmSet => "vmset",
            ScriptOut::Write => "write",
            ScriptOut::Off => "none",
        }
    }
}

/// Tunables measured by earlier probes and used by later ones. The executor copies them into
/// each probe context and back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Learned {
    pub macro_spelling: Option<MacroSpelling>,
    pub exists: Option<ExistsFn>,
    pub build: Option<PowerViewBuild>,
    pub script_out: Option<ScriptOut>,
    /// A value set over EXP 2 reads back inside a `DO`'d script. macro-visibility decides.
    pub exp_to_script: Option<bool>,
    /// `OS.FILE.ABSPATH` returns an absolute stage path unchanged. unpack-literal decides.
    pub abspath_same: Option<bool>,
}

impl Learned {
    /// The session tuning these facts call for: reference chunks, the measured macro spelling.
    pub fn tuning(&self) -> Tuning {
        Tuning::default().with_macro_spelling(self.macro_spelling.unwrap_or_default())
    }
}

#[derive(Debug)]
pub struct EnvConfig {
    pub run_id: String,
    pub hosts: Vec<String>,
    pub port: u16,
    pub prompt: Option<DevicePrompt>,
    pub idle: Duration,
    pub scale: f64,
    pub vm_window: VmWindow,
    pub state_dir: PathBuf,
    pub stage_root: Option<PvPath>,
    pub gzip_max: u32,
    pub latency_n: u32,
    pub gates: Gates,
    pub wsl: WslFacts,
}

#[derive(Debug)]
pub struct Env {
    pub cfg: EnvConfig,
    /// Shares the Ctrl-C flag.
    pub cancel: Cancel,
    /// Host and port that attached, set after `connect`.
    pub endpoint: Option<Endpoint>,
    pub learned: Learned,
    /// Lab polling without real waits.
    #[cfg(test)]
    pub lab_timing: crate::probes::lab::LabTiming,
}

impl Env {
    pub fn new(cfg: EnvConfig) -> Self {
        Self {
            cfg,
            cancel: Cancel::new(),
            endpoint: None,
            learned: Learned::default(),
            #[cfg(test)]
            lab_timing: crate::probes::lab::LabTiming::INSTANT,
        }
    }

    pub fn connect_opts(&self) -> ConnectOpts {
        ConnectOpts {
            attach_idle: self.cfg.idle.min(Duration::from_secs(5)),
            budget: CallBudget::idle(self.cfg.idle).with_total(Duration::from_secs(300)),
            prompt: self.cfg.prompt.clone(),
            cancel: self.cancel.clone(),
            ..ConnectOpts::default()
        }
    }

    /// First half of the window: raw `VM:` work of the probes, snapshotted by them.
    pub fn scratch_window(&self) -> VmWindow {
        let w = self.cfg.vm_window;
        VmWindow::new(w.base(), w.len() / 2).expect("half of a valid window is valid")
    }

    /// Second half: the `Stage` the probes open.
    pub fn stage_window(&self) -> VmWindow {
        let w = self.cfg.vm_window;
        VmWindow::new(w.base() + w.len() / 2, w.len() / 2).expect("half of a valid window is valid")
    }

    pub fn deadline(&self, base: Duration) -> Duration {
        base.mul_f64(self.cfg.scale)
    }

    /// The cleanup budget after a probe: a fixed part plus time to write back and read back
    /// every pending `VM:` restore at `RESTORE_FLOOR`, times `--scale`.
    pub fn cleanup_deadline(&self, restore_bytes: u64) -> Duration {
        self.deadline(CLEANUP_BASE + Duration::from_secs_f64(2.0 * restore_bytes as f64 / RESTORE_FLOOR))
    }

    pub fn exists(&self) -> ExistsFn {
        self.learned.exists.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networking_mode_prefers_wslinfo_and_never_fails() {
        assert_eq!(mode_from(Some("mirrored"), true, Some("172.29.32.1")), WslMode::Mirrored);
        assert_eq!(mode_from(None, true, Some("172.29.32.1")), WslMode::Nat);
        assert_eq!(mode_from(None, true, Some("192.168.0.1")), WslMode::Unknown);
        assert_eq!(mode_from(Some("garbage"), false, None), WslMode::Unknown);
    }

    #[test]
    fn auto_hosts_are_loopback_then_gateway_then_nameserver_without_duplicates() {
        let w = WslFacts {
            default_gateway: Some("172.29.32.1".into()),
            nameserver: Some("172.29.32.1".into()),
            ..WslFacts::default()
        };
        assert_eq!(w.candidates(), ["127.0.0.1", "172.29.32.1"]);
    }
}
