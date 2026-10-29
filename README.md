# t32ctl

t32ctl lets a coding agent in WSL2 drive a TRACE32 debugger whose PowerView runs on the same
Windows PC. The only channel is the TRACE32 Remote API (RCL) over TCP. Files and scripts travel
through TRACE32 virtual memory (`VM:`), `Data.SAVE.Binary` and `Data.LOAD.Binary`; PowerView runs
them; results come back the same way.

This repository holds the foundation, the Phase 0 measurement tool, and the Phase 1 CLI:

| Crate | What it is |
|---|---|
| `t32rcl` | RCL client over NETTCP: framing, attach, keepalive, commands, functions, PRACTICE state, memory, global macros. Reusable outside the CLI. |
| `t32stage` | Staging over RCL: `VM:` transfer, self-extracting bundles, the script runner, cleanup with a write-ahead journal. |
| `t32probe` | Phase 0 binary. Measures how a live PowerView behaves and prints a report short enough to paste. |
| `t32ctl` | Phase 1 CLI for coding agents. The MVP reads status, memory, and registers, runs board procedures and repository scripts, and sends single PRACTICE commands; the rest of the contract in [docs/cli.md](docs/cli.md) is planned. |

Phase 0 exists because the build machine has no TRACE32: every behavior the design depends on
and the manuals do not settle is measured by `t32probe` on the operator's PC. The design is in
[docs/architecture.md](docs/architecture.md); running the probe is in
[docs/operator.md](docs/operator.md). What the probe runs taught about PowerView is in
[docs/trace32-pitfalls.md](docs/trace32-pitfalls.md).

## Phase 1: t32ctl

`t32ctl` is the command a coding agent calls once per action. The first word after `t32ctl` is a
permission grade (`read`, `run`, `write`, `sys`, `burn`, `raw`), so a Claude Code rule on the
first two words covers exactly one grade. The binary implements the MVP: the commands whose
PowerView behavior the Phase 0 probe proved on build 168999, plus memory and register reads.

- `read status`: whether PowerView answers, the connected CPU against the board profile, the run
  state, and PowerView's PRACTICE state; `--full` adds the profile hash, the binding, the config,
  and the state directory.
- `read mem`, `read reg`: target memory, checked against the profile's memory map, and registers
  of PowerView's selected core.
- `read procs`, `<grade> proc NAME`: the board's procedures and their run at the grade the profile
  declares, with typed parameters and collected output files. An eMMC flash is a `burn`
  procedure.
- `raw script`, `raw cmd`, `raw eval`: a `.cmm` set from the repository, or one PRACTICE command
  or function. They always ask.
- `read describe`: the command schema, the generated Markdown blocks, and the permission rules.

Run control, breakpoints, symbols, variables and the stack, memory and register writes, code
loading, PowerView windows, and the lease are planned. They wait for the target opt-in probes
(`run-control`, `target-read`, `load-code`, `elf-unzip`) and for the state file;
[docs/cli.md](docs/cli.md#planned-commands) lists them.

### Set up

1. Put the static `t32ctl` binary ([Build](#build)) on the `PATH` in WSL, and enable RCL in
   PowerView as for the probe ([Run the probe](#run-the-probe)).
2. Optionally write `~/.config/t32ctl/config.toml` with `schema = 1` and the keys you change: the
   endpoint (`host = "auto"` and port 20000 by default), the `VM:` staging window, the transfer
   bounds ([profiles.md](docs/profiles.md#connection-config)).
3. Give the board a profile: `<board dir>/support/t32ctl/profile.toml` with `[target]` (`name`,
   `cpu`, `cores`, `word`), the memory map, and the procedures, beside the `.cmm` scripts they
   run ([profiles.md](docs/profiles.md#target-profile)). t32ctl finds it through the build's
   `CMakeCache.txt` (`BOARD_DIR`), `T32CTL_PROFILE`, or `<repo>/.t32ctl.toml`. Review changes to
   that directory like code: Phase 1 runs a procedure at the grade its profile declares and has no
   operator trust yet. Without a profile, `read status`, `read reg` and the `raw` commands work
   from the connection alone, with the CPU unchecked; `read mem` and procedures need one.
4. Merge the output of `t32ctl read describe --format permissions` into `.claude/settings.json`,
   and give the agent the skill in [skills/t32ctl/](skills/t32ctl/SKILL.md).
5. Check with `t32ctl read status`.

The contract and its decisions:

- [docs/cli.md](docs/cli.md): grades and permission rules, commands, notation, output, exit
  codes, state between calls, and the lease.
- [docs/profiles.md](docs/profiles.md): the connection config, board profiles, procedures, and
  the rules for writing their scripts.
- [docs/cli-sessions.md](docs/cli-sessions.md): literal agent sessions and workflow coverage.
- [skills/t32ctl/SKILL.md](skills/t32ctl/SKILL.md): the agent skill.
- [ADR 0002](docs/adr/0002-phase1-cli-contract.md): the decisions behind the contract.

## Build

Rust stable (1.89 or later) with the musl target:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

The static binaries are `target/x86_64-unknown-linux-musl/release/t32probe` and `.../t32ctl`.
CI builds the same binaries on every push and keeps them as an artifact. Publishing a GitHub
release builds them from the release tag and attaches them, each with a `.sha256` file.

## Run the probe

1. Start PowerView and enable RCL over TCP on port 20000: `SETUP.API.RCL.Enable /PRotocol TCP`
   (DVD 09/2024 and later), or `RCL=NETTCP` with `PORT=20000` in `config.t32` on older DVDs.
2. Make sure no PRACTICE script is running in PowerView.
3. In WSL2: `./t32probe --json probe.json`.
4. Paste the text report from stdout back into the chat. Keep `probe.json`; it holds every
   measurement and error chain.

`--host auto` (the default) tries `127.0.0.1`, the WSL default gateway, then the `resolv.conf`
nameserver. Opt-in probes need their flags; each one is described in
[docs/operator.md](docs/operator.md) together with what the probe does to the PC.

## Probes

The table is generated by `t32probe --list --markdown`; CI fails when it drifts.

<!-- probe-table:start -->
| Probe | Tier | Requires | Deadline | Measures |
|---|---|---|---|---|
| `connect` | safe | - | 30 s | TCP connect, attach, first response; classifies refused/timeout/non-RCL/no-answer |
| `version` | safe | `connect` | 10 s | build numbers, feature table, minimum-version hint |
| `latency` | safe | `connect` | 60 s | N round trips of GetState and of a trivial function: p50, p95, max |
| `clients` | safe | `connect` | 5 s | RCL.TCP.NrUsedCons(), else api.RCL.TCP.NrUsedCons() |
| `debugger-info` | safe | `connect` | 30 s | debugger, probe and debug-port state, read only: PowerView version and instance, licence, probe, cable, host link, SYStem.Mode, JTAG clock setting, port type, power sense; run once per debugger mode |
| `keepalive` | safe | `connect` | 30 s | blocking WAIT 10.s: do keepalive frames arrive, at what interval |
| `reconnect` | safe | `connect` | 20 s | read timeout shorter than a blocking command; the executor reconnects once |
| `tempdir` | safe | `connect` | 30 s | OS.PTD(), the stage root, MKDIR/RM/RMDIR, exists function, path with spaces |
| `vm-rw` | safe | `connect` | 180 s | RCL memory write/read on VM: 1 B to 1 MiB, chunk ladder, throughput, verified restore |
| `vm-file` | safe | `vm-rw` `tempdir` | 60 s | VM: to file and back with Data.SAVE/LOAD.Binary, ++ slicing |
| `gzip` | safe | `vm-file` | 180 s | gzip upload, /UNZIP decode, corrupt gzip must fail, size ceiling |
| `macro` | safe | `tempdir` | 45 s | EXP 2 macros: wire spelling, set/get, redeclaration, size, characters, cross spelling, empty and undeclared names, command-line assignment |
| `fn-table` | safe | `connect` `tempdir` `vm-rw` | 60 s | availability of commands and functions, & in functions, command length, EVAL slot, status codes and ERROR.ID per failure, epoch and CPU reads, path functions |
| `fs-semantics` | safe | `vm-file` | 120 s | .bak naming on overwrite, pre-RM, /Append, RMDIR of a full directory, idempotent cleanup, path punctuation, OS.FIRSTFILE listing, ABSPATH identity |
| `vm-large` | safe | `vm-file` | 300 s | VM: window at full size: RCL write, Data.SAVE/LOAD.Binary time and keepalives at 4 MiB, 8 MiB and the whole window (blocks the GUI for seconds) |
| `tar-download` | safe | `vm-file` | 60 s | TAR + ZIP of a work directory in PowerView, download through VM:, untar and gunzip in Rust |
| `script-lab` | safe | `vm-file` | 180 s | file-existence oracle for DO'd scripts; Data.Set VM: and WRITE channels; DO timing, PRACTICE state while scripts run, RCL during WAIT, loops and an 8 MiB SAVE |
| `script-halt` | safe | `script-lab` | 90 s | STOP, uncaught errors, T32_Stop status, END, ERROR.* reads, files and globals after END (sends END, which closes every custom dialog and OPEN file) |
| `script-error` | safe | `script-lab` | 120 s | failing scripts surface as failures, never as success: ON ERROR JUMPTO across DO, STOP, ENDDO FALSE(), a failed SAVE, IF and GOTO (sends END, which closes every custom dialog and OPEN file) |
| `channel-in` | safe | `script-lab` | 180 s | client to script without macros: VM: strings, DO arguments, EVAL, READ, OS.PPD, ~~~~ reads |
| `channel-out` | safe | `script-lab` | 150 s | script to client without macros: WRITE/WRITEB files, EVAL, AREA, ENDDO values, ENTRY after DO, Data.Set limits, loop expansion |
| `macro-visibility` | safe | `script-lab` | 180 s | client macros seen by DO'd scripts: EXP bare/amp, command-line GLOBAL and assignment, declared-only, undeclared, EXPLICIT, implicit visibility |
| `macro-return` | safe | `script-lab` | 180 s | script macros seen by the client and later scripts: script GLOBAL over EXP, writes into client globals, frames during WAIT, EXP while halted with CONTinue, markers across END, storage form, generated wrappers (sends END, which closes every custom dialog and OPEN file) |
| `unpack-literal` | safe | `script-lab` | 300 s | the unpack shape on literal paths: time per file and per MiB, .bak on rerun, pre-RM, the empty-or-relative path guard, ABSPATH identity |
| `bundle` | safe | `gzip` `macro` `macro-visibility` `unpack-literal` | 150 s | self-extracting bundle, nested DO chain, marker, runner rc, state during the chain; only where scripts see the snapshot path, with a canary outside it |
| `dialog` | opt-in `--allow-dialog` | `script-lab` | 40 s | Lab script opens a dialog: state 2, T32_Stop, only our script ended (shows a window) |
| `multi-client` | opt-in `--allow-multi-client` | `script-lab` | 60 s | second connection: attach and latency while A's script waits, NrUsedCons, attach while A blocks in WAIT |
| `run-control` | opt-in `--allow-run-control --symbol <s>` | `vm-file` | 180 s | Go/Break and every core's PC, Go to --symbol, steps and step-out, breakpoint kind, write and /COUNT breakpoints, a reverted RAM and register write, breakpoint list unchanged or restored from its STOre script (changes target state) |
| `target-read` | opt-in `--allow-target-read --symbol <s>` | `vm-file` | 180 s | registers, symbol memory and chunk reads, address translation, Var functions, a memory range over RCL and through a file, epoch reads, windows printed to a file (reads only; leave no printer file open) |
| `load-code` | opt-in `--allow-load-code --elf <path>` | `vm-file` | 300 s | Data.LOAD.Elf of --elf into the halted target with /NoReg and without, time and keepalives, PC, /DIFF with FOUND(); the PC restored (changes target memory) |
| `elf-unzip` | opt-in `--elf <path>` | `gzip` `tempdir` | 240 s | Data.LOAD.Elf x.elf.gz /UNZIP and the VM: fallback, then with --symbol the sYmbol functions on it; symbols only |
<!-- probe-table:end -->

## Check the firewall by hand

`t32probe` runs on the PowerView PC and cannot see the Windows firewall from another host. To
check that another PC can reach PowerView, run this in PowerShell on that PC, then on the
PowerView PC, and compare:

```powershell
Test-NetConnection <powerview-pc> -Port 20000
```

`TcpTestSucceeded : True` on both means the port is open; `False` only on the remote PC means a
firewall rule blocks it.

## License

Apache-2.0, see [LICENSE](LICENSE). Code ported from t32mcp `t32api.rs` (Apache-2.0) and pyrcl
1.1.6 (MIT) keeps its attribution in [NOTICE](NOTICE) and in the ported file headers.
