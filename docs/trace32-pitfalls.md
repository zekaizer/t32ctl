# TRACE32 pitfalls

PowerView behaviors that broke an assumption in t32ctl or would break the next one. Each entry
gives the symptom, the evidence, and the rule the code follows. The evidence is from `t32probe`
runs against PowerView build 168999 (base 166414, about DVD 09/2024) on Windows 11 with t32ctl in
WSL2, unless a manual is cited. A newer build may differ; rerun the probe before relying on an
entry there.

## Macros

### Commands sent over RCL never expand macros

`T32_Cmd` (ExecuteCommand) behaves like the PowerView command line: `MKDIR "&t32ctl_root"` creates
a directory literally named `&t32ctl_root`, and `&b="[&a]"` stores `[&a]`. The PRACTICE Script
Language User's Guide says so ("Macro expansion does NOT take place inside the TRACE32 command
line!"). Declaring (`GLOBAL &x`) and assigning a literal (`&x="text"`) do work on the command
line. Function arguments are not expanded either (`fn-table`: `fn_amp=literal`).

Rule: a client-issued command or function names everything literally. Only `.cmm` scripts expand
macros.

### EXP 2 macro names: `&name` and `name` are different macros

The Remote API's EXP 2 call takes a macro name. Spelled with the ampersand (`&t32ctl_x`) it reads
and writes the PRACTICE macro that scripts use. Spelled without it (`t32ctl_x`) it reads and
writes a separate store that no script sees, and a set followed by a get still round-trips, so a
round-trip test cannot tell the two apart (`macro-visibility`: `v1` set bare is empty in a script,
`v3`/`v4` set with `&` are seen; after a script writes, only the `&` spelling returns the new
value).

This caused the alpha.3 failure: Stage declared `GLOBAL &t32ctl_snap` on the command line, which
created an empty PRACTICE global, then set `t32ctl_snap` bare. The unpack script saw an empty
`&t32ctl_snap` and unpacked the bundle into PowerView's working directory.

Rule: every macro shared with a script is set and read over EXP 2 with the `&` spelling. Decide
the spelling by script visibility, never by an EXP round trip. A `GLOBAL` declaration is not
needed before an EXP set with `&`.

### An unset macro expands differently depending on whether it was declared

Inside a script, a macro that was never declared stays as its literal text (`[&t32ctl_x]`). A
macro declared with `GLOBAL` but never assigned expands to the empty string (`macro-visibility`:
`u1`, `e1`). A path built from either lands somewhere unintended: `"&t32ctl_snap\main.cmm"`
becomes `\main.cmm`, resolved against `OS.PWD()`, or a directory literally named `&t32ctl_snap`.
With `PMACRO.EXPLICIT`, assigning an undeclared macro is an error.

Rule: a script that writes through a macro path first checks that the value is non-empty and
absolute (`render_unpack` guards `&t32ctl_snap` with `OS.FILE.ABSPATH`).

### What a macro stores

A string assignment stores the text without its quotes (`&x="0"` gives `0`). Any other expression
is stored in PRACTICE notation: `&x=0` stores `0x0` (the default radix is hex), `&x=0.` stores
`0.`, `&x=1.+1.` stores `2.`, `&x=TRUE()` stores `TRUE()` (`macro-return`: `num`, `quote`). A
value read back from `ENTRY %LINE` or a script's `ENDDO` value keeps its quotes
(`channel-out`: `entryline=quoted`). EXP 2 values and PRACTICE macros hold at most 4 KiB.

Rule: scripts assign result codes as strings (`&t32ctl_rc="0"`). Parsers accept `0`, `0.` and
`0x0`, and strip one pair of quotes only where a quoted form is documented.

## Paths and files

### `OS.PWD()` can be anywhere

PowerView's working directory is wherever it was started. Started from a WSL shell, it is a
`\\wsl.localhost\<distro>\...` UNC path inside the operator's WSL home (`fn-table`:
`path.OS.PWD()`). Every relative path, and every path that starts with `\`, resolves against it
(`OS.FILE.ABSPATH("\x.cmm")` returns `<pwd>\x.cmm`). `PRINTER.FILENAME()` defaults there too.

Rule: t32ctl never uses a relative path. Staged files live under `<OS.PTD()>\t32ctl` or
`--stage-root`.

### `SETUP.BAKfile` is on: overwriting a file leaves a `.bak`

`Data.SAVE.Binary` onto an existing file keeps the old one as a backup. The backup name replaces
the last extension with `.bak`: `x.bin` gives `x.bak`, `complete` gives `complete.bak`, `a.b.c`
gives `a.b.bak`, and `k.cmm` and `k.bin` share `k.bak` (`fs-semantics`: `bak_name`,
`bak_collide`). The backup holds the old content. `RM` before the write leaves no backup, and
`/Append` makes none. A leftover `.bak` makes the directory's `RMDIR` fail.

Rule: probes write each file once under a unique name. Where Stage rewrites a snapshot (a
repair), it registers the predicted `.bak` names for cleanup. `RM` before the write is the
alternative where a rewrite is routine.

### File commands are not idempotent

`MKDIR` of an existing directory, `RM` of a missing file and `RMDIR` of a missing directory are
errors, and `RMDIR` of a non-empty directory fails with "critical error accessing file"
(`fs-semantics`: `idem`, `rmdir_full`).

Rule: check with `OS.DIR()` or `OS.FILE.readable()` before creating or removing, and remove
contents before the directory.

### Directory listings include `.` and `..`

`OS.FIRSTFILE("<dir>\*")` and `OS.NEXTFILE()` return `.` and `..` along with the entries. The
alpha.4 `unpack-literal` probe turned them into the path `u1/.` and failed.

Rule: listings drop `.` and `..`.

### Other file facts

- `OS.FILE.readable()` answers; use it as the existence test (`tempdir`).
- `~~~` resolves to `OS.PTD()`; `~~~~` to the running script's directory (`channel-in`).
- `WRITE` writes CRLF line ends (`channel-out`, `script-lab`).
- `TAR` stores ustar names with the full path minus the drive (`Users/.../a.txt`), and `ZIP`
  produces gzip (`tar-download`).

## Memory

### Reading never-written `VM:` fails with status 113

A read of `VM:` memory that was never written returns RCL status 113 (`STD_FAILED`) with a
full-size reply, not a bus error or `FN1` (alpha.1 `vm-rw`). `Data.CLEARVM` exists and returns a
range to that state.

Rule: t32rcl classifies 113 on a memory read or write as a memory access refusal; VM snapshots
treat such a unit as uninitialized.

### Transfer sizes

Memory reads and writes of 15360 bytes per request work in both directions. `Data.SAVE.Binary`
and `Data.LOAD.Binary` of 32 MiB through `VM:` take well under a second with no keepalive needed
(`vm-large`).

## Scripts and the PRACTICE stack

### `DO` returns at once; RCL keeps working while a script runs

`DO` over ExecuteCommand returns before the script ends. Functions, memory access, EXP 2 and other
commands answer in under a millisecond while a script runs or waits (`script-lab`: `blocks=no`,
`rcl_wait`). Completion is detected by polling `T32_GetPracticeState` and
`PRACTICE.StackDepth()`, not by the `DO` reply.

### A blocking command blocks new connections

While a command such as `WAIT 5.s` runs on the command line, PowerView sends a keepalive frame
about every 2 s and accepts no new RCL connection until it ends (`keepalive`, `reconnect`:
`revive` about 4.7 s after a 0.3 s timeout). A read timeout during such a command poisons the
link.

Rule: never wait inside PowerView on behalf of the client; every wait is a bounded client poll.

### `END` is global

`END` ends every PRACTICE script, closes every custom dialog, and closes every file opened with
`OPEN`, not only the script t32ctl started (practice_ref `END`). `T32_Stop` answers with status 0
or 1, both success (api_remote_c).

Rule: t32ctl sends `END` only after checking the stack held nothing but its own script, and warns
that it closes the operator's dialogs and files.

### `ENDDO` hands back text, not a value

A script's `ENDDO <value>` reaches the caller's `ENTRY %LINE` as the text of the line, unevaluated
and with its quotes: `ENDDO "a="+OS.PPD()` arrives as `"a="+OS.PPD()` (alpha.6 `bundle`). Only
macros in it are expanded, because that happens before the line runs.

Rule: compute a return value into a macro first (`&m=...`), then `ENDDO "&m"`.

### Errors

`ON ERROR JUMPTO` in a script catches errors raised in nested `DO` files (`script-error`:
`caught=6/6`). After a script halts on an error, `EVAL ERROR.MESSAGE()` followed by
`T32_EvalGetString` reads the message (`msg=slot`); `ERROR.ID("PLAIN")` is refused.

### Script input and output without macros

These also work, if the macro path cannot be used: a NUL-terminated string the client writes into
`VM:` and the script reads with `Data.String(VM:<addr>)` (up to 4095 bytes), `DO <file> <args>`
with `ENTRY` or `PRACTICE.ARG()`, `Data.Set VM:<addr> "<text>" 0` read back over RCL, a `WRITE`
file, and the `EVAL` slot (`channel-in`, `channel-out`).

## Functions and versions

- The `api.` function group (`api.RCL.TCP.NrUsedCons()`) starts at build 171257. Build 168999
  knows only the deprecated `RCL.TCP.NrUsedCons()`.
- Not available on build 168999: `Step.Out` (use `Go.Return` or `Go.Up`), `PRACTICE.GLOBAL.VALUE()`,
  `PRACTICE.GLOBAL.EXIST()`, `PRACTICE.MACRO.VALUE()`, `Eval.ENDDO` (`fn-table`).
- A command sent over RCL is at most 2037 characters (the client's limit, accepted in full).
- With the target down, `Register(PC)` fails with "target system down"; `SYStem.Up()`,
  `CORE.NUMBER()` and `SYStem.CPU()` still answer.
