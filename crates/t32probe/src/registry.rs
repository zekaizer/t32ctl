// SPDX-License-Identifier: Apache-2.0
//! The probe catalogue as data. One declaration generates the id enum, the names, the ordered
//! table, `--list` and the README table. `ALL` is the execution order; `requires` names only
//! earlier probes (unit test).

use std::time::Duration;

use serde::{Serialize, Serializer};

use crate::{
    cli::Gates,
    ctx::{ProbeCtx, ProbeResult},
    probes::{channels, files, functions, macros, optin, scriptlab, scripts, storage, target, transport, unpack},
};

/// Why a probe is not in the default run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptIn {
    Dialog,
    MultiClient,
    RunControl,
    TargetRead,
    LoadCode,
    Elf,
}

impl OptIn {
    pub const fn flag(self) -> &'static str {
        match self {
            OptIn::Dialog => "--allow-dialog",
            OptIn::MultiClient => "--allow-multi-client",
            OptIn::RunControl => "--allow-run-control --symbol <s>",
            OptIn::TargetRead => "--allow-target-read --symbol <s>",
            OptIn::LoadCode => "--allow-load-code --elf <path>",
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
    Keepalive = "keepalive", Safe, [Connect], true, 30, transport::keepalive,
        "blocking WAIT 10.s: do keepalive frames arrive, at what interval";
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
    Macro = "macro", Safe, [Tempdir], true, 45, scripts::macro_probe,
        "EXP 2 macros: wire spelling, set/get, redeclaration, size, characters, cross spelling, empty and undeclared names, command-line assignment";
    FnTable = "fn-table", Safe, [Connect, Tempdir, VmRw], true, 60, functions::fn_table,
        "availability of commands and functions, & in functions, command length, EVAL slot, status codes and ERROR.ID per failure, epoch and CPU reads, path functions";
    FsSemantics = "fs-semantics", Safe, [VmFile], true, 120, files::fs_semantics,
        ".bak naming on overwrite, pre-RM, /Append, RMDIR of a full directory, idempotent cleanup, path punctuation, OS.FIRSTFILE listing, ABSPATH identity";
    VmLarge = "vm-large", Safe, [VmFile], true, 300, files::vm_large,
        "VM: window at full size: RCL write, Data.SAVE/LOAD.Binary time and keepalives at 4 MiB, 8 MiB and the whole window (blocks the GUI for seconds)";
    TarDownload = "tar-download", Safe, [VmFile], true, 60, storage::tar_download,
        "TAR + ZIP of a work directory in PowerView, download through VM:, untar and gunzip in Rust";
    ScriptLab = "script-lab", Safe, [VmFile], true, 180, scriptlab::script_lab,
        "file-existence oracle for DO'd scripts; Data.Set VM: and WRITE channels; DO timing, PRACTICE state while scripts run, RCL during WAIT, loops and an 8 MiB SAVE";
    ScriptHalt = "script-halt", Safe, [ScriptLab], true, 90, scriptlab::script_halt,
        "STOP, uncaught errors, T32_Stop status, END, ERROR.* reads, files and globals after END";
    ScriptError = "script-error", Safe, [ScriptLab], true, 120, scripts::script_error,
        "failing scripts surface as failures, never as success: ON ERROR JUMPTO across DO, STOP, ENDDO FALSE(), a failed SAVE, IF and GOTO";
    ChannelIn = "channel-in", Safe, [ScriptLab], true, 180, channels::channel_in,
        "client to script without macros: VM: strings, DO arguments, EVAL, READ, OS.PPD, ~~~~ reads";
    ChannelOut = "channel-out", Safe, [ScriptLab], true, 150, channels::channel_out,
        "script to client without macros: WRITE/WRITEB files, EVAL, AREA, ENDDO values, ENTRY after DO, Data.Set limits, loop expansion";
    MacroVisibility = "macro-visibility", Safe, [ScriptLab], true, 180, macros::macro_visibility,
        "client macros seen by DO'd scripts: EXP bare/amp, command-line GLOBAL and assignment, declared-only, undeclared, EXPLICIT, implicit visibility";
    MacroReturn = "macro-return", Safe, [ScriptLab], true, 180, macros::macro_return,
        "script macros seen by the client and later scripts: script GLOBAL over EXP, writes into client globals, frames during WAIT, EXP while halted with CONTinue, markers across END, storage form, generated wrappers";
    UnpackLiteral = "unpack-literal", Safe, [ScriptLab], true, 300, unpack::unpack_literal,
        "the unpack shape on literal paths: time per file and per MiB, .bak on rerun, pre-RM, the empty-or-relative path guard, ABSPATH identity";
    Bundle = "bundle", Safe, [Gzip, Macro, MacroVisibility, UnpackLiteral], true, 150, scripts::bundle,
        "self-extracting bundle, nested DO chain, marker, runner rc, state during the chain; only where scripts see the snapshot path, with a canary outside it";
    Dialog = "dialog", Tier::OptIn(Dialog), [ScriptLab], true, 40, optin::dialog,
        "Lab script opens a dialog: state 2, T32_Stop, only our script ended (shows a window)";
    MultiClient = "multi-client", Tier::OptIn(MultiClient), [ScriptLab], true, 60, optin::multi_client,
        "second connection: attach and latency while A's script waits, NrUsedCons, attach while A blocks in WAIT";
    RunControl = "run-control", Tier::OptIn(RunControl), [VmFile], true, 180, optin::run_control,
        "Go/Break and every core's PC, Go to --symbol, steps and step-out, breakpoint kind, write and /COUNT breakpoints, a reverted RAM and register write, breakpoint list unchanged (changes target state)";
    TargetRead = "target-read", Tier::OptIn(TargetRead), [VmFile], true, 180, target::target_read,
        "registers, symbol memory and chunk reads, address translation, Var functions, a memory range over RCL and through a file, epoch reads, windows printed to a file (reads only; leave no printer file open)";
    LoadCode = "load-code", Tier::OptIn(LoadCode), [VmFile], true, 300, target::load_code,
        "Data.LOAD.Elf of --elf into the halted target with /NoReg and without, time and keepalives, PC, /DIFF with FOUND(); the PC restored (changes target memory)";
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
            "fn-table",
            "fs-semantics",
            "vm-large",
            "tar-download",
            "script-lab",
            "script-halt",
            "script-error",
            "channel-in",
            "channel-out",
            "macro-visibility",
            "macro-return",
            "unpack-literal",
            "bundle",
        ];
        let opt = [
            ("dialog", Dialog),
            ("multi-client", MultiClient),
            ("run-control", RunControl),
            ("target-read", TargetRead),
            ("load-code", LoadCode),
            ("elf-unzip", Elf),
        ];
        for n in safe {
            assert_eq!(ProbeId::parse(n).map(|p| p.def().tier), Some(Tier::Safe), "{n}");
        }
        for (n, o) in opt {
            assert_eq!(ProbeId::parse(n).map(|p| p.def().tier), Some(Tier::OptIn(o)), "{n}");
        }
        assert_eq!(ALL.len(), safe.len() + opt.len());
        let order: Vec<&str> = ALL.iter().filter(|d| d.tier == Tier::Safe).map(|d| d.id.name()).collect();
        assert_eq!(order, safe);
        let rows: [(&str, &[&str], u64); 22] = [
            ("keepalive", &["connect"], 30),
            ("gzip", &["vm-file"], 180),
            ("macro", &["tempdir"], 45),
            ("fn-table", &["connect", "tempdir", "vm-rw"], 60),
            ("fs-semantics", &["vm-file"], 120),
            ("vm-large", &["vm-file"], 300),
            ("tar-download", &["vm-file"], 60),
            ("script-lab", &["vm-file"], 180),
            ("script-halt", &["script-lab"], 90),
            ("script-error", &["script-lab"], 120),
            ("channel-in", &["script-lab"], 180),
            ("channel-out", &["script-lab"], 150),
            ("macro-visibility", &["script-lab"], 180),
            ("macro-return", &["script-lab"], 180),
            ("unpack-literal", &["script-lab"], 300),
            ("bundle", &["gzip", "macro", "macro-visibility", "unpack-literal"], 150),
            ("dialog", &["script-lab"], 40),
            ("multi-client", &["script-lab"], 60),
            ("run-control", &["vm-file"], 180),
            ("target-read", &["vm-file"], 180),
            ("load-code", &["vm-file"], 300),
            ("elf-unzip", &["gzip", "tempdir"], 240),
        ];
        for (n, req, secs) in rows {
            let d = ProbeId::parse(n).expect("listed").def();
            assert_eq!(d.requires.iter().map(|r| r.name()).collect::<Vec<_>>(), req, "{n}");
            assert_eq!(d.deadline.as_secs(), secs, "{n}");
        }
    }

    #[test]
    fn select_adds_prerequisites_and_refuses_closed_gates() {
        let none = Gates::default();
        let names = |v: Vec<&ProbeDef>| v.iter().map(|d| d.id.name()).collect::<Vec<_>>();
        assert_eq!(names(select(&[ProbeId::VmFile], &[], &none).unwrap()), ["connect", "tempdir", "vm-rw", "vm-file"]);
        let default = select(&[], &[ProbeId::Latency], &none).unwrap();
        assert_eq!(default.len(), 23);
        assert!(default.iter().all(|d| d.tier == Tier::Safe));
        assert_eq!(select(&[ProbeId::Dialog], &[], &none).unwrap_err(), "dialog needs --allow-dialog");
        let gates = Gates { dialog: true, ..Gates::default() };
        assert!(names(select(&[], &[], &gates).unwrap()).contains(&"dialog"));
        assert_eq!(
            not_run(&gates).iter().map(|p| p.name()).collect::<Vec<_>>(),
            ["multi-client", "run-control", "target-read", "load-code", "elf-unzip"]
        );
    }
}
