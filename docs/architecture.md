# Architecture

This document explains how the four crates fit together, why they are shaped this way, and which
PowerView behaviors are still assumptions waiting for a probe report. The decision to build a
standalone Rust binary with a native RCL client is [ADR 0001](adr/0001-rust-standalone-binary.md).

## Crates and dependency direction

```text
t32ctl, t32probe  ->  t32stage  ->  t32rcl  ->  std
```

- `t32rcl` speaks RCL and nothing else. It knows no PRACTICE script text, no files, no policy.
  It depends on `std` only, so any tool can reuse it.
- `t32stage` is the only place that formats PRACTICE command text (`practice`), defines the
  bundle format, the runner script, and the cleanup ledger.
- `t32probe` measures. `t32ctl` is the Phase 1 CLI: a library with a thin binary, so tests run
  whole calls in-process. It implements the MVP subset of the contract.

## t32rcl: one request in flight

```text
Session ── Link ── Transport (TcpStream, or a scripted transport in tests)
              ├── Framer   (sans-IO: bytes in, NETTCP frames out)
              ├── IdGen + classify (sans-IO: keepalive, stale, match, desync)
              └── Waiter   (sans-IO: idle and total clocks driven by an injected Clock)
```

The wire format is ported from t32mcp `t32api.rs` and pyrcl `_library.py`; the request encodings
are pinned by golden vectors that `tools/golden/gen.py` regenerates from pyrcl's own serializers.
Three deviations from the references are deliberate:

- A frame is returned only after its 8-byte pad has arrived; `t32api.rs` drains a partial pad and
  then misreads the rest as the next header.
- A response id is matched modulo the 255-id cycle, so a late answer from before the wrap to 0 is
  stale instead of a desync.
- A frame longer than any reply the client can provoke (2 bytes plus a u16 length) is a protocol
  violation as soon as the header is in. Neither reference bounds the length.

Every timeout, loss, protocol violation or cancellation poisons the link: a late response would
otherwise be matched to the next request. `Session::try_revive` reconnects at most once per
poisoning, and the library never retries a request; only the caller knows whether it was
idempotent. Errors are one enum whose `class()` tells the caller what to do next.

## t32stage: files and scripts through VM:

Upload of a file: RCL memory write into a `VM:` region, `Data.SAVE.Binary <file> VM:<range>`,
then `OS.FILE.SIZE` must match. Download: PowerView `ZIP`s the file, the client loads the `.gz`
into `VM:`, reads it and gunzips it in Rust, so no decompressed size is needed in advance.

A bundle is many files in one gzip blob whose first member is a generated `unpack.cmm`:

```text
client                                        PowerView
VM:A <- gzip(blob)            (RCL writes)
Data.SAVE.Binary <key>.gz VM:A                (blob on disk)
Data.LOAD.Binary <key>.gz VM:B /UNZIP         (blob inflated in VM:)
Data.SAVE.Binary <snap>\.t32ctl\unpack.cmm VM:B++<len-1>
DO unpack.cmm             --------------->    per file: Data.SAVE.Binary from VM:B, size check
                                              last: the completion marker
```

The offsets in the script and in the blob come from one layout with fixed-width numbers, so the
script length does not depend on the offsets it contains. The snapshot directory is named by a
content hash; a snapshot whose marker exists costs one function call to reuse.

Running a script: the runner `.cmm` reads its inputs from global macros set over EXP 2, runs the
entry script under `ON ERROR JUMPTO`, and as its last statement on both the success and the error
path copies a fresh token into `&t32ctl_done`. The client polls `GetPracticeState`; when it reads
idle it compares the token. `GetPracticeState` reads 0 for a script halted at an error although
the script is still on the stack, so stack depth alone cannot prove completion; the token can.
State 2 (dialog) is answered with `T32_Stop` and an unwind. Runs start only on an empty PRACTICE
stack, and the unwind stays pending only until the stack is seen empty again, so the unwinding
`END` can only end scripts t32stage started.

The stage root is resolved once per `Stage`: `--stage-root` when given, otherwise
`OS.PTD()\t32ctl` when that text is ASCII-safe. A temp directory that is not ASCII-safe needs
`--stage-root`. PowerView does not expand macros in commands sent over RCL (PRACTICE Script
Language User's Guide), so no root that PowerView computes itself can be named in a client
command. For the same reason the client sets every macro a script reads over EXP 2.

## Cleanup

Every side effect registers its undo before it happens. Undos are a closed enum, not closures,
because a cleanup that cannot run must still print the exact command to type into PowerView.
The ledger runs newest first, never stops at a failure, and revives a dead connection once. Once its
own unwind fails, the journaled undos older than it stay in the journal unapplied: they name what
the script it could not end may still use. Undos that name PowerView files, directories, macros or
the PRACTICE stack are written to a locked journal on the WSL side before they are registered; the
next run replays journals left by dead processes. A journal that names the PRACTICE stack replays
only while PowerView is idle with an empty stack; otherwise the dead process's script may still run,
and the journal waits for a later run. A halted script, or one in a dialog, never ends by itself, so
that report also names `END`, for a human to type only if the script is t32ctl's. `VM:` restores
stay in memory: they are captured per 4 KiB unit, written back, read back and compared, and a unit
that was never initialized is cleared alone.

A run's output directory (`out-<token>`) is the one undo that removes what it holds. Its script
writes files no undo names in advance, and a run that fails or is interrupted fetches none, so
cleanup lists the directory (`OS.FIRSTFILE`), removes each entry, contents before their
directory, then the directory. A listing stops at 1000 names; a full one that removed something
is listed again. Removals past a failure go on; the manual commands name each entry left, then the
`RMDIR`. When an entry may stay that no command can name (a name outside the stage's path
characters, or a listing still full), cleanup prints no command: what is left is unknown.

## t32probe

`registry` declares the catalogue once (ids, tiers, prerequisites, deadlines, `--list`, the README
table). `exec` owns every guarantee: prerequisite skips, one reconnect, deadlines through
`Cancel`, `catch_unwind`, a fresh cleanup `Cancel` so an expired deadline or Ctrl-C never cancels
the cleanup, and the JSON rewrite after each probe. `outcome` is built once and rendered twice,
as the paste-back text and as JSON, so the two cannot disagree. Probes run in table order and
learn as they go: `tempdir` picks the file-exists function and `macro` the EXP 2 name spelling
that later probes use, `script-lab` the value channel Lab scripts answer through, and
`macro-visibility` and `unpack-literal` whether scripts see EXP-set macros and whether ABSPATH
keeps an absolute path; `bundle` runs only where both hold.

## The Lab: probe scripts on literal paths

Script probes run their PRACTICE through `probes::lab`. A third live run showed a macro set over
EXP 2 expanding to an empty string inside a `DO`'d script, so a Lab script never takes a path from
a macro: every path is a literal absolute path below its work directory
`<root>\probe-<run>-<tag>`, rendered by the client. Writing through a macro is measured only
behind an `IF` that compares it with the expected literal first.

- Oracle (FX): the script answers by saving the one-byte marker region to `<id>-<name>.bin` with
  `Data.SAVE.Binary`; the client asks the file-exists function. `@ask k L == R` writes
  `<id>-k-yes.bin` or `<id>-k-no.bin`. It uses only what the third run proved inside a script.
  `Data.Set VM:` and `WRITE` are measured as value channels on top of it, never as the oracle.
- Markers: `a` at the start, `z` after the default answers, `h`, `w` or any other name where a
  case may halt or wait. Every output name is unique per run and case and is checked absent and
  registered for removal before the `DO`; the directory is listed at the end and files nobody
  registered (`.bak` siblings, names PowerView chose) are reported as `stray` and removed.
- Ending: `GetPracticeState` alone never ends a script, because a halted script and a `WAIT`
  both read idle with a non-empty stack. A Lab sends `END` only to a stack it filled itself, and
  only after a marker allowed the halt and it held for 300 ms, after the case timeout and a
  `T32_Stop`, or after the probe stopped the script on purpose. Before each `DO` the stack must be
  empty: our own leftover is unwound once, anybody else's script makes the probe `SKIP`. The
  stack check does not limit what `END` does beyond the stack: it also closes every custom dialog
  and every `OPEN` file in PowerView (practice_ref `END`), which no RCL call can check
  beforehand. After an `END` the Lab sends `CLOSE` for the buffers its own script opened.
- Revert: settings a probe changes (an AREA, a breakpoint, a selected core, a register, a printer
  file) are undone by `Undo::Revert`, a list of commands that lives only in memory. It is never
  journaled: replaying it after a crash would act on a state the operator may have changed since,
  so a dead run leaves it to a human. A probe applies it with `Ledger::revert_now` right after the
  step that measured it; cleanup applies whatever a failure left pending.
- The opt-in target probes ask `PRACTICE.CoMmanD.AVAILable` or `PRACTICE.FUNCtion.AVAILable`
  before each target command or function and record `unverified:<name>` instead of sending one the
  build does not offer.

## t32ctl: the Phase 1 contract

The command-line contract is in [cli.md](cli.md), board profiles in [profiles.md](profiles.md),
literal sessions in [cli-sessions.md](cli-sessions.md), and the decisions in
[ADR 0002](adr/0002-phase1-cli-contract.md). `t32ctl` stays a layer on top of `t32stage` and
`t32rcl`: a command body receives a `Ctx<G>` typed by its grade and never sees a `Session` or a
`Stage`. The per-PowerView state directory, `pv/<host>-<port>/` keyed by the endpoint as
configured, is passed to `t32stage` as its state directory, so journal replay and the `VM:` lease
are per PowerView. `t32probe` keys its default `--state-dir` by the same rule
(`t32stage::pv_state_dir`), so the two tools lock and journal the same directory.

`t32ctl` walks a board's profile directory itself: it skips hidden files and editor leftovers,
refuses symlinks and special files, hashes the rest into the profile hash, and builds the bundle
from the same files with `Bundle::insert`. `Bundle::key` still keys the staging cache.

The contract needs these additions below `t32ctl`. Where an addition relies on PowerView
behavior, its code is marked `ASSUMED(<probe>)` until a probe report confirms it, like the
assumptions under [Open questions](#open-questions).

| Addition | Crate | Status |
|---|---|---|
| Read builders in `practice`: `Register(<name>)` with a validated `RegName`, `CORE()`, `CORE.NUMBER()`, `SYStem.CPU()`, and `MMU.PHYSICAL(<class>:0x...)` for address translation | `t32stage` | done; `MMU.PHYSICAL` is `ASSUMED(target-read)` |
| An access width in the memory address object (`WI` and a u16 after the access class, as pyrcl serializes it), so an `mmio` read uses its region's width in one bulk read | `t32rcl` | done; PowerView honoring it is `ASSUMED(target-read)` |
| Parameter macros `t32ctl_p_<name>`: validated dynamic names, set over EXP 2 with the `&` spelling before the run (no `GLOBAL`), cleared through `Undo::ClearMacros`, carried by `RunRequest` | `t32stage` | done; `macro-visibility` holds on build 168999 |
| Runner v2: the entry script owns `&t32ctl_rc`; the runner writes nothing on the success path and flags a caught error in `&t32ctl_fail`, with the message in `&t32ctl_msg`; the token is the last write on both paths. `Stage` empties `Done`, `Fail`, `Rc`, `Msg`, and `Ret` over EXP 2 before the `DO`. `RUNNER_VERSION = 2` changes every bundle key once | `t32stage` | done; rerun `t32probe --only bundle` once on build 168999 to confirm v2 |
| Result macros `t32ctl_rc` and `t32ctl_msg`, read after a run: `0`, `0.` and `0x0` are zero (`ScriptRc`), and an empty rc is success except for a `burn` procedure | `t32stage` | done; `macro-visibility` and `macro-return` hold on build 168999 |
| Sliced append upload (`Stage::upload_file`): a local file goes to `in-<token>/` slice by slice through the `VM:` window, `Data.SAVE.Binary` for the first slice and `/Append` for the rest, then `OS.FILE.SIZE` must match | `t32stage` | done; `ASSUMED(fs-semantics)` until the append result is in the record |
| Output listing before `TAR`: `OS.FIRSTFILE` and `OS.NEXTFILE`, dropping `.` and `..`; an empty output directory returns no files without `TAR`, which was never measured on an empty directory | `t32stage` | done |
| Script arguments capped at 4 KiB, the PRACTICE macro limit | `t32stage` | done |
| One per-PowerView state directory rule, `pv/<host>-<port>/` with the host as configured, beside `default_state_dir()`; the auto host candidates move from `t32probe` to `t32stage::host`; `t32probe` uses the rule for its default `--state-dir` | `t32stage`, `t32probe` | done; no PowerView behavior |
| `testkit` feature: scripted stage conversations for `t32ctl`'s tests | `t32stage` | done; tests only |
| Debugger builders in `practice`: run, run to a location, step, halt, the return register after a step out, breakpoints with condition and count, on-chip watchpoints, register and variable writes, core select, `Frame.view`, symbol and source-line queries, window printing with a printer restore undo, `Data.LOAD.Elf` with code | `t32stage` | planned; `ASSUMED(run-control)`, which gains cases for step, breakpoint options and counts, software breakpoints in RAM, source lines, and printer routing |
| An instance marker macro; a missing marker reads as unknown, because `PMACRO.RESet` deletes it as well as a restart | `t32stage` | planned with the epoch; `ASSUMED(macro)` |

`toml` is the one new workspace dependency. `t32ctl` also starts to depend on `t32stage`, `serde`,
`serde_json`, `sha2`, and `signal-hook`, which the workspace already has.

## Testing without hardware

No PowerView stand-in exists. Tests use canned frames at the transport boundary
(`t32rcl::testkit`: a strict script of expected requests and raw replies, and a manual clock so
nothing sleeps), golden byte vectors from pyrcl, sans-IO units for framing, matching, waiting and
script watching, and loopback sockets as raw byte sources for connect classification. CI runs the
socket smoke tests against the static musl binary.

`t32ctl` runs whole calls in-process through its library entry point against the same scripted
transport, with `t32stage::testkit` for scripted stage conversations (snapshot, runner, outputs),
so every exit code and report line of the MVP commands is tested without PowerView.
`crates/t32ctl/tests/docs.rs` keeps the documentation honest: the generated blocks equal the
binary's output, every documented command line parses or names a planned command, the planned
table is disjoint from the binary, the example config, binding file, and profile load, and the
`transcript: mvp` blocks equal rendered reports.

Probe bodies are tested where a script of reasonable size can drive their decisions: `connect`
(smoke tests), `version`, `clients`, `keepalive`, `reconnect`, `tempdir`, `macro`, the `vm-rw`
chunk ladder, the `gzip` acceptance decision, the Lab rendering and judgement, the full runs of
`script-lab`, `script-halt`, `channel-in`, `channel-out`, `macro-return` and `dialog`, the gates
and decision points of the other Lab probes, `bundle`, `fn-table`, `fs-semantics`, `vm-large`
and `tar-download`, the revert paths of `run-control`, `target-read` and `load-code`, and the
failure path and symbol queries of `elf-unzip`. The rest is live-only, because a script would
have to answer megabytes of `VM:` traffic or stand in for PRACTICE semantics: `latency` (timings
only), the `vm-rw` round trips from 1 B to 1 MiB, `vm-file`, the `gzip` payloads and ceiling
search, the success paths of the large Lab probes, `multi-client` (it opens a real second
connection), and the run paths of `run-control`, `target-read`, `load-code` and `elf-unzip`.

## Open questions

Each PowerView behavior below is assumed by code marked `ASSUMED(<probe>)` and is measured by
that probe. The result column is build 168999 (about DVD 09/2024), from the alpha.1 to alpha.8 runs
on #2; `docs/trace32-pitfalls.md` explains the refuted ones. A row without a result needs an opt-in
probe that has not run yet.

| Assumption | Probe | Where | Build 168999 |
|---|---|---|---|
| PowerView always sends the 8-byte frame pad. | `connect` | `t32rcl::frame` | holds |
| A long EXP reply (a 60000-byte macro value) arrives as one frame. | `macro` | `t32rcl::frame` | moot: EXP values over 4 KiB are refused |
| Request id 0 (after id 254) is accepted. | `latency` | `t32rcl::wait` | holds |
| A keepalive is `[0] == 0xFE`; status 254 shares the byte (raw frames are recorded). | `keepalive` | `t32rcl::wait` | holds; one about every 2 s |
| Memory chunks above 1024 bytes are accepted, and up to which size. | `vm-rw` | `t32rcl::Tuning` | holds up to the client limit, 15360 |
| Reading never-initialized `VM:` fails instead of returning zeros. | `vm-rw` | `t32stage::vm` | holds, as status 113 |
| `Data.CLEARVM <range>` exists on DVD 02/2024. | `vm-rw` | `practice::cmd::clear_vm` | holds |
| `OS.PTD()` answers; whether its text is ASCII-safe; trailing separator. | `tempdir` | `practice::fnc::PTD` | holds; ASCII, no separator |
| Which of `OS.FILE.readable`, `OS.FILE.EXIST`, `FILE.EXIST` answers. | `tempdir` | `practice::ExistsFn` | `OS.FILE.readable` |
| `PRACTICE.StackDepth()` answers over ExecuteFunction. | `tempdir` | `practice::fnc::STACK_DEPTH` | holds |
| `/UNZIP` refuses a corrupt gzip (the bundle's integrity check). | `gzip` | `Stage::ensure_snapshot` | holds, 4 of 4 |
| EXP 2 with `&name` reaches the macro a `DO`'d script uses; bare `name` is a separate store. | `macro-visibility` | `t32rcl::MacroSpelling` | holds for `&name`; bare is a separate store |
| Redeclaring an existing global keeps its value. | `macro` | `practice::cmd::global` | holds |
| `&m="0"` stores `0`, not `"0"` (quotes are stripped either way). | `macro-return` | `Stage` | holds |
| A macro set over EXP 2 reads back inside a `DO`'d script. | `macro-visibility` | `bundle::render_unpack` | holds with `&name` |
| One-line `IF` bodies, `GOTO` and `OPEN /Create /Binary` for empty files work in the unpack script, which writes nothing behind an empty or relative snapshot path. | `unpack-literal` | `bundle::render_unpack` | holds |
| `OS.FILE.ABSPATH` returns an absolute path unchanged. | `unpack-literal` | `bundle::render_unpack` | holds |
| `DO` over ExecuteCommand returns while the script runs. | `script-lab` | `Stage::execute_with` | holds |
| A script in `WAIT` reads as running, not as halted. | `script-lab` | `runner::ScriptWatch` | holds |
| `ENTRY %LINE` after `DO` yields the callee's `ENDDO` value. | `channel-out` | `runner::runner_cmm` | partly: the unevaluated text, quotes kept |
| `ON ERROR JUMPTO` in the runner catches errors in nested `DO` files and unwinds them. | `script-error` | `runner::runner_cmm` | holds, 6 of 6 |
| `EVAL ERROR.MESSAGE()` + EvalGetString reads the error of a halted script. | `script-error` | `Stage` | holds |
| `T32_Stop` answers status 0 or 1. | `script-halt` | `t32rcl::Session::stop_practice` | holds; 0 seen |
| Arguments with `, / : = + @ - . _` reach `ENTRY` intact through the unquoted `DO` line. | `channel-in` | `practice::render_args` | holds |
| An overwritten file leaves a `.bak` sibling named as `bak_candidates` predicts. | `fs-semantics` | `RelPath::bak_candidates` | holds; last extension becomes `.bak` |
| 10 s plus 1 s per MB covers the silence of a long `SAVE` or `LOAD` between keepalives. | `vm-large` | `transfer::data_budget` | holds; 32 MiB in under 0.2 s |
| `TAR <archive> <dir>\*` stores names relative to `<dir>`. | `tar-download` | `practice::cmd::tar` | refuted: full path minus the drive; `tar::below` strips it |
| `ZIP` writes plain gzip. | `tar-download` | `practice::cmd::zip` | holds |
| The whole bundle path: upload, unpack, cache hit, repair, runner, nested `DO` chain. | `bundle` | `Stage` | holds (alpha.8) |
| A modal dialog reads as state 2 and `T32_Stop` ends the wait (and whether the window closes). | `dialog` | `runner::ScriptWatch` | |
| Two `STOre <file> Break` lists differ only when breakpoints differ. | `run-control` | `practice::cmd::store_breakpoints` | |
| `Data.LOAD.Elf ... /UNZIP` exists. | `elf-unzip` | `practice::cmd::load_elf_symbols` | |
| `MMU.PHYSICAL(<class>:0x<addr>)` translates a virtual address, and its reply names the physical address after the last `:`. | `target-read` | `practice::fnc::physical`, `t32ctl` `read mem` | |
| PowerView honors the access width (`WI`) of a memory read object, so an `mmio` read uses the region's width. | `target-read` (case to add) | `t32rcl::MemAddr::with_width` | |
| A read with the `E` class prefix (`ED:`, `EA:`) works while the cores run. | `target-read` (case to add) | `t32ctl` `read mem` | |
| `Register(<name>)` answers for system registers (`esr_el1`, `sctlr_el1`) as it does for `x0`. | `target-read` | `practice::fnc::register` | |
| Memory read over RCL is little-endian per cell on this AArch64 target. | `target-read` (case to add) | `t32ctl::report::cells` | |

Questions that only the operator can answer:

- Which DVD is installed? DVD 02/2024 has no build tag in the manuals, so `min=uncertain` is
  expected for builds between 160740 and 166507.
- Can a second client attach while the first one runs a script or blocks in a command?
  `multi-client` measures it, but it needs `--allow-multi-client`.
