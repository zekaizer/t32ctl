// SPDX-License-Identifier: Apache-2.0
//! The run environment: validated configuration plus facts learned along the way.

use std::{net::Ipv4Addr, path::PathBuf, process::Command, time::Duration};

use serde::Serialize;
use t32rcl::{CallBudget, Cancel, ConnectOpts, DevicePrompt, Endpoint, MacroSpelling, PowerViewBuild};
use t32stage::{ExistsFn, PvPath, VmWindow};

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
        let default_gateway = std::fs::read_to_string("/proc/net/route")
            .ok()
            .and_then(|r| gateway_from_proc_route(&r))
            .map(|g| g.to_string());
        let nameserver = std::fs::read_to_string("/etc/resolv.conf").ok().and_then(|t| nameserver_from_resolv_conf(&t));
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
        let mut v = vec!["127.0.0.1".to_string()];
        for h in [&self.default_gateway, &self.nameserver].into_iter().flatten() {
            if !v.contains(h) {
                v.push(h.clone());
            }
        }
        v
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

/// Default route of `/proc/net/route`: destination `00000000`, gateway as little-endian hex.
pub fn gateway_from_proc_route(route: &str) -> Option<Ipv4Addr> {
    route.lines().skip(1).find_map(|l| {
        let cols: Vec<&str> = l.split_whitespace().collect();
        if cols.get(1) != Some(&"00000000") {
            return None;
        }
        let g = u32::from_str_radix(cols.get(2)?, 16).ok()?;
        (g != 0).then(|| Ipv4Addr::from(g.to_le_bytes()))
    })
}

pub fn nameserver_from_resolv_conf(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("nameserver ")).map(|s| s.trim().to_string())
}

/// Tunables measured by earlier probes and used by later ones. The executor copies them into
/// each probe context and back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Learned {
    pub macro_spelling: Option<MacroSpelling>,
    pub exists: Option<ExistsFn>,
    pub build: Option<PowerViewBuild>,
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
}

impl Env {
    pub fn new(cfg: EnvConfig) -> Self {
        Self { cfg, cancel: Cancel::new(), endpoint: None, learned: Learned::default() }
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

    pub fn exists(&self) -> ExistsFn {
        self.learned.exists.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gateway_is_decoded_from_little_endian_hex() {
        let t = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t0010A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
                 eth0\t00000000\t01201DAC\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";
        assert_eq!(gateway_from_proc_route(t), Some(Ipv4Addr::new(172, 29, 32, 1)));
        assert_eq!(gateway_from_proc_route("Iface\tDestination\n"), None);
    }

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
        assert_eq!(nameserver_from_resolv_conf("# x\nnameserver 10.0.0.1\n"), Some("10.0.0.1".into()));
    }
}
