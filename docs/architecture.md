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
- `t32probe` measures. `t32ctl` is the Phase 1 CLI; in this PR it has `status` only.

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
The ledger runs newest first, never stops at a failure, and revives a dead connection once.
Undos that name PowerView files, directories, macros or the PRACTICE stack are written to a
locked journal on the WSL side before they are registered; the next run replays journals left
by dead processes. `VM:` restores stay in memory: they are captured per 4 KiB unit, written back,
read back and compared, and a unit that was never initialized is cleared alone.

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

## Testing without hardware

No PowerView stand-in exists. Tests use canned frames at the transport boundary
(`t32rcl::testkit`: a strict script of expected requests and raw replies, and a manual clock so
nothing sleeps), golden byte vectors from pyrcl, sans-IO units for framing, matching, waiting and
script watching, and loopback sockets as raw byte sources for connect classification. CI runs the
socket smoke tests against the static musl binary.

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

Questions that only the operator can answer:

- Which DVD is installed? DVD 02/2024 has no build tag in the manuals, so `min=uncertain` is
  expected for builds between 160740 and 166507.
- Can a second client attach while the first one runs a script or blocks in a command?
  `multi-client` measures it, but it needs `--allow-multi-client`.
