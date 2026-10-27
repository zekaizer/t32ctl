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

The stage root is resolved once per `Stage`: the literal `OS.PTD()\t32ctl` path when that text is
ASCII-safe, otherwise the macro `&t32ctl_root`, which PowerView computes and expands itself so a
temp directory in an unknown code page never passes through the client's command bytes.
`--stage-root` overrides both.

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
that later probes use.

## Testing without hardware

No PowerView stand-in exists. Tests use canned frames at the transport boundary
(`t32rcl::testkit`: a strict script of expected requests and raw replies, and a manual clock so
nothing sleeps), golden byte vectors from pyrcl, sans-IO units for framing, matching, waiting and
script watching, and loopback sockets as raw byte sources for connect classification. CI runs the
socket smoke tests against the static musl binary.

## Open questions

Each PowerView behavior below is assumed by code marked `ASSUMED(<probe>)` and is measured by
that probe. The probe report answers it.

| Assumption | Probe | Where |
|---|---|---|
| PowerView always sends the 8-byte frame pad. | `connect` | `t32rcl::frame` |
| A long EXP reply (a 60000-byte macro value) arrives as one frame. | `macro` | `t32rcl::frame` |
| Request id 0 (after id 254) is accepted. | `latency` | `t32rcl::wait` |
| A keepalive is `[0] == 0xFE`; status 254 shares the byte (raw frames are recorded). | `keepalive` | `t32rcl::wait` |
| Memory chunks above 1024 bytes are accepted, and up to which size. | `vm-rw` | `t32rcl::Tuning` |
| Reading never-initialized `VM:` fails instead of returning zeros. | `vm-rw` | `t32stage::vm` |
| `Data.CLEARVM <range>` exists on DVD 02/2024. | `vm-rw` | `practice::cmd::clear_vm` |
| `OS.PTD()` answers; whether its text is ASCII-safe; trailing separator. | `tempdir` | `practice::fnc::PTD` |
| `GLOBAL` and `&m=OS.PTD()+"\t32ctl"` over ExecuteCommand create and set the root macro, and later commands expand it. | `tempdir` | `practice::cmd::bind_root` |
| Which of `OS.FILE.readable`, `OS.FILE.EXIST`, `FILE.EXIST` answers. | `tempdir` | `practice::ExistsFn` |
| `PRACTICE.StackDepth()` answers over ExecuteFunction. | `tempdir` | `practice::fnc::STACK_DEPTH` |
| `/UNZIP` refuses a corrupt gzip (the bundle's integrity check). | `gzip` | `Stage::ensure_snapshot` |
| EXP 2 names a macro without `&` (both spellings are tried). | `macro` | `t32rcl::MacroSpelling` |
| Redeclaring an existing global keeps its value. | `macro` | `practice::cmd::global` |
| Macros expand inside a quoted command-line assignment. | `macro` | `practice::cmd::bind_path` |
| `&m="0"` stores `0`, not `"0"` (quotes are stripped either way). | `macro` | `Stage` |
| One-line `IF` bodies, `GOTO`, `OPEN /Create /Binary` for empty files, and EXP-set macros visible to scripts. | `bundle` | `bundle::render_unpack` |
| `DO` over ExecuteCommand returns while the script runs. | `bundle` | `Stage::execute_with` |
| `ENTRY %LINE` after `DO` yields the callee's `ENDDO` value. | `bundle` | `runner::runner_cmm` |
| `ON ERROR JUMPTO` in the runner catches errors in nested `DO` files and unwinds them. | `script-error` | `runner::runner_cmm` |
| `EVAL ERROR.MESSAGE()` + EvalGetString reads the error of a halted script. | `script-error` | `Stage` |
| Arguments with `, / : = + @ - . _` reach `ENTRY` intact through the unquoted `DO` line. | `script-error` | `practice::render_args` |
| `TAR <archive> <dir>\*` stores names relative to `<dir>`. | `tar-download` | `practice::cmd::tar` |
| `ZIP` writes plain gzip. | `tar-download` | `practice::cmd::zip` |
| A modal dialog reads as state 2 and `T32_Stop` ends the wait (and whether the window closes). | `dialog` | `runner::ScriptWatch` |
| Two `STOre <file> Break` lists differ only when breakpoints differ. | `run-control` | `practice::cmd::store_breakpoints` |
| `Data.LOAD.Elf ... /UNZIP` exists. | `elf-unzip` | `practice::cmd::load_elf_symbols` |

Questions that only the operator can answer:

- Which DVD is installed? DVD 02/2024 has no build tag in the manuals, so `min=uncertain` is
  expected for builds between 160740 and 166507.
- Does a second client block while the first one runs a long `DO`? `multi-client` measures it,
  but it needs `--allow-multi-client`.
