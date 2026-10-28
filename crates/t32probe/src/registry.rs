// SPDX-License-Identifier: Apache-2.0
//! The probe catalogue as data. One declaration generates the id enum, the names, the ordered
//! table, `--list` and the README table. `ALL` is the execution order; `requires` names only
//! earlier probes (unit test).

use std::time::Duration;

use serde::{Serialize, Serializer};

use crate::{
    cli::Gates,
    ctx::{ProbeCtx, ProbeResult},
    probes::{optin, scriptlab, scripts, storage, transport},
};

/// Why a probe is not in the default run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptIn {
    Dialog,
    MultiClient,
    RunControl,
    Elf,
}

impl OptIn {
    pub const fn flag(self) -> &'static str {
        match self {
            OptIn::Dialog => "--allow-dialog",
            OptIn::MultiClient => "--allow-multi-client",
            OptIn::RunControl => "--allow-run-control --symbol <s>",
            OptIn::Elf => "--elf <path>",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Safe,
    OptIn(OptIn),
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Safe => "safe",
            Tier::OptIn(o) => o.flag(),
        }
    }
}

pub struct ProbeDef {
    pub id: ProbeId,
    pub tier: Tier,
    pub requires: &'static [ProbeId],
    /// False only for `connect`, which creates the connection it measures.
    pub needs_connection: bool,
    /// Wall-clock cap before `--scale`, enforced through `Cancel`.
    pub deadline: Duration,
    pub about: &'static str,
    pub run: fn(&mut ProbeCtx<'_>) -> ProbeResult,
}

impl std::fmt::Debug for ProbeDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id.name())
    }
}

macro_rules! probes {
    ($( $variant:ident = $name:literal, $tier:expr, [$($req:ident),*], $conn:literal, $secs:literal, $run:path,
        $about:literal; )+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ProbeId { $($variant),+ }

        impl ProbeId {
            pub const fn name(self) -> &'static str {
                match self { $(ProbeId::$variant => $name),+ }
            }

            pub fn parse(text: &str) -> Option<ProbeId> {
                match text { $($name => Some(ProbeId::$variant),)+ _ => None }
            }

            pub fn def(self) -> &'static ProbeDef {
                ALL.iter().find(|d| d.id == self).expect("every ProbeId has a table row")
            }
        }

        pub static ALL: &[ProbeDef] = &[
            $(ProbeDef {
                id: ProbeId::$variant,
                tier: $tier,
                requires: &[$(ProbeId::$req),*],
                needs_connection: $conn,
                deadline: Duration::from_secs($secs),
                about: $about,
                run: $run,
            }),+
        ];
    };
}

use OptIn::*;
use Tier::Safe;

probes! {
    Connect = "connect", Safe, [], false, 30, transport::connect,
        "TCP connect, attach, first response; classifies refused/timeout/non-RCL/no-answer";
    Version = "version", Safe, [Connect], true, 10, transport::version,
        "build numbers, feature table, minimum-version hint";
    Latency = "latency", Safe, [Connect], true, 60, transport::latency,
        "N round trips of GetState and of a trivial function: p50, p95, max";
    Clients = "clients", Safe, [Connect], true, 5, transport::clients,
        "RCL.TCP.NrUsedCons(), else api.RCL.TCP.NrUsedCons()";
    Keepalive = "keepalive", Safe, [Connect], true, 20, transport::keepalive,
        "blocking WAIT 3.s: do keepalive frames arrive, at what interval";
    Reconnect = "reconnect", Safe, [Connect], true, 20, transport::reconnect,
        "read timeout shorter than a blocking command; the executor reconnects once";
    Tempdir = "tempdir", Safe, [Connect], true, 30, storage::tempdir,
        "OS.PTD(), the stage root, MKDIR/RM/RMDIR, exists function, path with spaces";
    VmRw = "vm-rw", Safe, [Connect], true, 180, storage::vm_rw,
        "RCL memory write/read on VM: 1 B to 1 MiB, chunk ladder, throughput, verified restore";
    VmFile = "vm-file", Safe, [VmRw, Tempdir], true, 60, storage::vm_file,
        "VM: to file and back with Data.SAVE/LOAD.Binary, ++ slicing";
    Gzip = "gzip", Safe, [VmFile], true, 180, storage::gzip,
        "gzip upload, /UNZIP decode, corrupt gzip must fail, size ceiling";
    Macro = "macro", Safe, [Tempdir], true, 30, scripts::macro_probe,
        "EXP 2 macros: wire spelling, set/get, redeclaration, size, characters";
    Bundle = "bundle", Safe, [Gzip, Macro], true, 90, scripts::bundle,
        "self-extracting bundle, nested DO chain, marker, runner rc, state during the chain";
    ScriptLab = "script-lab", Safe, [VmFile], true, 180, scriptlab::script_lab,
        "file-existence oracle for DO'd scripts; Data.Set VM: and WRITE channels; DO timing, PRACTICE state while scripts run, RCL during WAIT, loops and an 8 MiB SAVE";
    ScriptError = "script-error", Safe, [Bundle], true, 90, scripts::script_error,
        "failing scripts surface as failures, never as success";
    TarDownload = "tar-download", Safe, [Bundle], true, 60, storage::tar_download,
        "TAR + ZIP in PowerView, download through VM:, untar in Rust";
    Dialog = "dialog", Tier::OptIn(Dialog), [Bundle], true, 40, optin::dialog,
        "script opens a dialog: state 2, T32_Stop, script ends (shows a window)";
    MultiClient = "multi-client", Tier::OptIn(MultiClient), [Bundle], true, 60, optin::multi_client,
        "second connection: NrUsedCons, latency of B while A runs a long DO";
    RunControl = "run-control", Tier::OptIn(RunControl), [Tempdir], true, 60, optin::run_control,
        "Go to --symbol with a timeout, Break, breakpoint list unchanged (changes target state)";
    ElfUnzip = "elf-unzip", Tier::OptIn(Elf), [Gzip, Tempdir], true, 240, optin::elf_unzip,
        "Data.LOAD.Elf x.elf.gz /UNZIP and the VM: fallback; symbols only";
}

impl Serialize for ProbeId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

/// Selected probes plus their prerequisites, in table order. An opt-in probe needs its gate; an
/// explicitly requested probe without its gate is a usage error.
pub fn select(only: &[ProbeId], skip: &[ProbeId], gates: &Gates) -> Result<Vec<&'static ProbeDef>, String> {
    let open = |d: &ProbeDef| match d.tier {
        Tier::Safe => true,
        Tier::OptIn(o) => gates.is_open(o),
    };
    let mut wanted: Vec<ProbeId> = if only.is_empty() {
        ALL.iter().filter(|d| open(d) && !skip.contains(&d.id)).map(|d| d.id).collect()
    } else {
        for id in only {
            if !open(id.def()) {
                return Err(format!("{} needs {}", id.name(), id.def().tier.label()));
            }
        }
        only.to_vec()
    };
    let mut i = 0;
    while i < wanted.len() {
        for r in wanted[i].def().requires {
            if !wanted.contains(r) {
                wanted.push(*r);
            }
        }
        i += 1;
    }
    Ok(ALL.iter().filter(|d| wanted.contains(&d.id)).collect())
}

/// Probes that are in the catalogue but not selected because their gate is closed.
pub fn not_run(gates: &Gates) -> Vec<ProbeId> {
    ALL.iter().filter(|d| matches!(d.tier, Tier::OptIn(o) if !gates.is_open(o))).map(|d| d.id).collect()
}

/// `--list` text.
pub fn list_text() -> String {
    let mut s = String::new();
    for d in ALL {
        let req: Vec<_> = d.requires.iter().map(|r| r.name()).collect();
        s += &format!(
            "{:<13}{:<34}{:>5}s  {}\n    requires: {}\n",
            d.id.name(),
            d.tier.label(),
            d.deadline.as_secs(),
            d.about,
            if req.is_empty() { "-".to_string() } else { req.join(", ") }
        );
    }
    s
}

/// `--list --markdown`: the README table, checked in CI.
pub fn list_markdown() -> String {
    let mut s = String::from("| Probe | Tier | Requires | Deadline | Measures |\n|---|---|---|---|---|\n");
    for d in ALL {
        let req: Vec<_> = d.requires.iter().map(|r| format!("`{}`", r.name())).collect();
        let tier = match d.tier {
            Tier::Safe => "safe".to_string(),
            Tier::OptIn(o) => format!("opt-in `{}`", o.flag()),
        };
        s += &format!(
            "| `{}` | {tier} | {} | {} s | {} |\n",
            d.id.name(),
            if req.is_empty() { "-".to_string() } else { req.join(" ") },
            d.deadline.as_secs(),
            d.about
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_ordered_unique_and_acyclic() {
        let mut seen: Vec<ProbeId> = Vec::new();
        for d in ALL {
            assert!(!seen.contains(&d.id), "duplicate {}", d.id.name());
            assert_eq!(ProbeId::parse(d.id.name()), Some(d.id));
            for r in d.requires {
                assert!(seen.contains(r), "{} requires {} which is not earlier", d.id.name(), r.name());
            }
            seen.push(d.id);
        }
        let offline: Vec<_> = ALL.iter().filter(|d| !d.needs_connection).map(|d| d.id.name()).collect();
        assert_eq!(offline, ["connect"]);
    }

    #[test]
    fn the_catalogue_matches_the_brief() {
        let safe = [
            "connect",
            "version",
            "latency",
            "clients",
            "keepalive",
            "reconnect",
            "tempdir",
            "vm-rw",
            "vm-file",
            "gzip",
            "macro",
            "bundle",
            "script-lab",
            "script-error",
            "tar-download",
        ];
        let opt = [("dialog", Dialog), ("multi-client", MultiClient), ("run-control", RunControl), ("elf-unzip", Elf)];
        for n in safe {
            assert_eq!(ProbeId::parse(n).map(|p| p.def().tier), Some(Tier::Safe), "{n}");
        }
        for (n, o) in opt {
            assert_eq!(ProbeId::parse(n).map(|p| p.def().tier), Some(Tier::OptIn(o)), "{n}");
        }
        assert_eq!(ALL.len(), safe.len() + opt.len());
    }

    #[test]
    fn select_adds_prerequisites_and_refuses_closed_gates() {
        let none = Gates::default();
        let names = |v: Vec<&ProbeDef>| v.iter().map(|d| d.id.name()).collect::<Vec<_>>();
        assert_eq!(names(select(&[ProbeId::VmFile], &[], &none).unwrap()), ["connect", "tempdir", "vm-rw", "vm-file"]);
        let default = select(&[], &[ProbeId::Latency], &none).unwrap();
        assert_eq!(default.len(), 14);
        assert!(default.iter().all(|d| d.tier == Tier::Safe));
        assert_eq!(select(&[ProbeId::Dialog], &[], &none).unwrap_err(), "dialog needs --allow-dialog");
        let gates = Gates { dialog: true, ..Gates::default() };
        assert!(names(select(&[], &[], &gates).unwrap()).contains(&"dialog"));
        assert_eq!(
            not_run(&gates).iter().map(|p| p.name()).collect::<Vec<_>>(),
            ["multi-client", "run-control", "elf-unzip"]
        );
    }
}
