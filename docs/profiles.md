# t32ctl profiles

`t32ctl` resolves four layers before it acts: the operator's connection config, the team's
target profile, the binding that picks a profile and an ELF for a workspace, and the runtime
state it keeps between calls. This document covers the first three, board procedures, and
validation. The state is described in [cli.md](cli.md#state-kept-between-calls), and the rest of
the command line in [cli.md](cli.md).

## The four layers

| Layer | File | Owner | Holds |
|---|---|---|---|
| connection | `~/.config/t32ctl/config.toml` | operator, not in git | endpoint, `VM:` staging window, Phase 0 tunables, lease limits, call limits |
| target | `<board dir>/support/t32ctl/`: `profile.toml` and the `.cmm` scripts beside it | team, in git | CPU and cores, ELF path, run timing, stop rules, access classes, memory map, peripheral views, procedures, variants |
| binding | `<repo>/.t32ctl.toml` (optional), `--build`, `T32CTL_BUILD`, `T32CTL_PROFILE` | project, in git | which build directory, profile directory, and variant a workspace uses |
| state | `~/.local/state/t32ctl/pv/<host>-<port>/` | t32ctl | epoch, tags, loaded ELF, intents, flash progress, lease |

`$XDG_CONFIG_HOME` and `$XDG_STATE_HOME` replace `~/.config` and `~/.local/state` when set.

Every key has one home, so the layers do not override each other's keys. Lease limits are only in
the connection config, because they are the operator's. Run timing is only in the profile, and a
`--wait` flag overrides it for one call within its type's range. A variant overrides its base
profile.

All files are TOML. Every table rejects unknown keys, and nothing falls back to a default
silently.

## Binding: which profile and which ELF

The binding is automatic in the common case. `./build/CMakeCache.txt` names the board directory
(`BOARD_DIR`), whose `support/t32ctl/` is the profile directory, and the board target (`BOARD`),
which picks a variant. The ELF is the profile's `target.elf` below the build directory.
Precedence, highest first:

1. `--build DIR` on the command line.
2. `T32CTL_BUILD` and `T32CTL_PROFILE` in the environment. `T32CTL_PROFILE` names a board's
   `support/t32ctl/` directory.
3. `<repo>/.t32ctl.toml` at the repository root.
4. Auto-detection from `./build/CMakeCache.txt`.

```toml
# <repo>/.t32ctl.toml: only when auto-detection picks the wrong build or profile.

schema = 1
build = "build-evb"
profile = "boards/acme/acme_evb/support/t32ctl"   # a board's support/t32ctl/ directory
```

`t32ctl read status --full` prints every resolved value with the rule or file it came from, and
the profile hash:

```console
$ t32ctl read status --full
e5 stopped core=0 pc=0x40083e10 sym=main src=src/main.c:31
target  acme_evb cpu=CortexA53 (matches profile) cores=4: 0 1 2 3 stopped
profile boards/acme/acme_evb/support/t32ctl/ profile=1c07e9b2 (6 files)
binding build=build (auto-detection: ./build/CMakeCache.txt), profile from BOARD_DIR,
        variant base (BOARD=acme_evb/a53)
config  ~/.config/t32ctl/config.toml: host=auto (127.0.0.1) port=20000
state   ~/.local/state/t32ctl/pv/auto-20000/
elf     build/zephyr/zephyr.elf sha=3f2a91c0 code+symbols, loaded at e3, matches the build
stop    reached main on core 0 (known)
bp      none
lease   none
pv      build=172803 clients=1 practice=idle
[exit 0]
```

After connecting, every call checks two things against the profile:

- `SYStem.CPU()` against `target.cpu`. A mismatch refuses everything but `read status` with exit 5
  `cpu-mismatch`: the board or the profile is wrong. The check is skipped while the debug port is
  down and for procedures that declare the `debugger-config` effect, so `attach` can select the
  CPU.
- The build's ELF against the record of the last load (size and mtime, then the hash). A
  difference adds `elf=stale` to the header; it is not a refusal. PowerView's symbols still match
  the running image, and refusing would force a reload to debug the old build. The hint is
  `run symbols`; loading code is the caller's own decision.

## Connection config

Personal, outside every repository. The `[tuning]` keys are the t32probe `config` line of a
Phase 0 run, key for key, so a probe result changes a value here and never the contract.

```toml
# ~/.config/t32ctl/config.toml: personal, not in git. Every key is optional.

schema = 1

[connection]
host = "auto"                  # 127.0.0.1, then the WSL gateway, then the resolv.conf nameserver
port = 20000                   # the state directory is pv/<host>-<port>/ with host as written here

[staging]
vm_base = 0x01000000           # the VM: window t32ctl may use; keep it clear of your own scripts
vm_len = 0x04000000            # 64M; a larger file is uploaded in slices
# root = 'C:\T32\tmp\t32ctl'   # instead of OS.PTD()\t32ctl

[tuning]                       # the t32probe `config` line of a Phase 0 run, key for key
read_chunk = 15360
write_chunk = 15360
macro = "bare"
keepalive = "1.00s"
exists = "OS.FILE.readable"
stage_root = "literal"
do_blocks = false              # whether DO over ExecuteCommand blocks until the script ends
go_leaves_breakpoint = false   # whether `Go <loc>` leaves a temporary breakpoint behind

[lease]
idle = "10m"                   # 1m to 30m: a holder that makes no call for this long loses it
default = "30m"                # 5m to 2h: the cap when `--for` is not given
max = "2h"                     # 5m to 2h: the largest `--for` accepted

[calls]
lock_wait = "10s"              # 0s to 60s: how long a call waits for call.lock
transfer_timeout = "10m"       # 1s to 1h: bound of a staged transfer, a code load, or a symbol load
transfer_max = "1G"            # largest `read mem --to` or `write mem --from`
```

Each duration is a type with a range, so no value can make a lease, a wait, or a transfer
unbounded. A value outside the range is a load error with file and line, not a clamp.

## Target profile

One directory per board, `support/t32ctl/` in the Zephyr board directory, holding `profile.toml`
and the board's `.cmm` scripts. The directory is the bundle root: its files travel with a
procedure. Three names are well known: `attach`, `reset`, and `flash`. The skill refers to them
by name, and `read procs` shows which ones a board has, so a board uses these names for these
jobs.

The profile hash is SHA-256 over the relative paths and contents of the regular files in the
directory, excluding hidden files and editor leftovers (`.*`, `*~`, `#*#`, `*.swp`). The bundle
carries the same files. `read procs` and `read status --full` print the hash as `profile=<hash>`:
the first 8 hex digits in text, all 64 in `--json`. It is independent of `t32stage::Bundle::key`,
which still keys the staging cache. A symlink or a special file in the directory is exit 5
`profile-invalid`, naming the file.

```toml
# boards/acme/acme_evb/support/t32ctl/profile.toml
#
# Team-owned, in git, beside the board's .cmm scripts. This directory is the bundle root: its
# files travel with a procedure, and `read procs` prints their hash as profile=<hash>. Phase 1
# runs every procedure at the grade declared here and uses this memory map as written, so review
# a change to this directory like code.

schema = 1

[target]
name = "acme_evb"
cpu = "CortexA53"            # compared with SYStem.CPU() on every call
cores = 4
word = 8                     # default cell width of `read mem` in ram and rom, in bytes
elf = "zephyr/zephyr.elf"    # relative to the build directory
classes = ["A", "D", "P"]    # access classes a place may name; any other is raw
default_class = "D"          # class of a bare address; type `A:0x...` for a physical address

[timing]
go_wait = "30s"              # default bound of `run go`; 100ms to 10m
step_wait = "30s"            # default bound of `run step`; 100ms to 10m

[stop]
# A stop whose PC is in one of these functions carries the fault registers, decoded.
fatal = ["k_sys_fatal_error_handler", "z_fatal_error", "z_arm64_fatal_error"]
fault_regs = ["esr_el1", "far_el1", "elr_el1", "spsr_el1"]

# Memory map of physical addresses. Every byte of an access must be allowed by the smallest
# region that contains it; a byte in no region is refused (exit 2, memory-policy).
[[memory]]
name = "ddr"
start = 0x40000000
size = 0x40000000
kind = "ram"
running = true               # readable while cores run

[[memory]]
name = "sram"
start = 0x00100000
size = 0x00040000
kind = "ram"

[[memory]]
name = "uart0"
start = 0x09000000
size = 0x00001000
kind = "mmio"
width = 4                    # every access to this block is 32-bit
write = true                 # mmio is read-only unless declared writable

[[memory]]
name = "uart0-fifo"          # reading pops the RX FIFO: never read by t32ctl
start = 0x09000000
size = 0x00000004
kind = "deny"

[[memory]]
name = "gic"
start = 0x08000000
size = 0x00020000
kind = "mmio"
width = 4

# PER.view sections `read window per.<name>` may show. List only sections whose registers have
# no read side effects; a FIFO or a clear-on-read status keeps its section out.
[per.clocks]
section = "Clock Controller"

[per.gicd]
section = "GIC Distributor"

# Board procedures. Parameters reach the script as global macros &t32ctl_p_<name>, already in
# PRACTICE form. The script writes declared outputs into the directory &t32ctl_out.

[proc.attach]
about = "Connect to the A53 cluster without resetting it"
script = "attach.cmm"
grade = "run"
timeout = "30s"
effects = ["debugger-config"]

[proc.reset]
about = "Reset the SoC and halt at the reset vector"
script = "reset.cmm"
grade = "write"
timeout = "20s"
effects = ["target-reset", "target-run"]

[proc.selftest]
about = "Read clock and PMIC status into selftest.txt"
script = "selftest.cmm"
grade = "read"
timeout = "30s"
effects = []
outputs = ["selftest.txt"]

[proc.flash]
about = "Write an image into eMMC and verify it by readback"
script = "flash_emmc.cmm"    # runs FLASHFILE with an algorithm binary from ~~/ (TRACE32)
grade = "burn"
lease = "required"
timeout = "45m"
effects = ["flash", "target-reset", "target-memory", "target-registers"]
outputs = ["verify.log"]

[proc.flash.params.image]
type = "file"
about = "image to write"
default = "{build}/zephyr/zephyr.bin"

[proc.flash.params.part]
type = "enum"
values = ["boot0", "boot1", "user"]
about = "eMMC hardware partition"
default = "user"

[proc.flash.params.offset]
type = "integer"
about = "byte offset inside the partition"
min = 0
default = 0

# A variant of the same board, selected by the build's BOARD (from CMakeCache.txt).
[variant."acme_evb/a53/up"]
target = { cores = 1 }
```

The directory holds `profile.toml`, `attach.cmm`, `reset.cmm`, `selftest.cmm`, `flash_emmc.cmm`,
and `emmc_setup.cmm`, which `flash_emmc.cmm` calls: six files.

`[target]`, `[timing]` and `[stop]` are what every call needs. `[timing]` gives the default
bounds of run waits; `--wait` overrides them per call, and both stay within 100 ms to 10 min.
`[stop]` names the fatal-error functions: a stop inside one of them reports the fault registers,
decoded, so crash triage starts from the stop report.

### Memory map

The memory map guards every place t32ctl reads or writes for the caller: `read mem`, `write mem`,
`read var` and `write var` by the lvalue's address and size, `read window list LOC`, `@reg`
places, and `--to` and `--from` transfers. Exec breakpoints and go targets must be code
([cli.md](cli.md#run-control-and-breakpoints)). Procedures and `raw` commands are outside the map,
which is one reason they ask. A breakpoint condition is evaluated by TRACE32 at each hit, so a
pointer it dereferences is not checked; that is a known limit.

| Kind | Read | Write |
|---|---|---|
| `ram` | yes | yes |
| `rom` | yes | no |
| `mmio` | yes | only with `write = true` |
| `deny` | no | no |

`read` and `write` override the kind per region. `running = true` allows access while cores run;
t32ctl adds the `E` access class itself. Elsewhere a running target refuses with exit 3
`not-halted`.

Regions describe physical addresses. A place in a translated class (`D`, `P`) is translated to a
physical address first, through TRACE32's translation function (`ASSUMED(run-control)`), and the
physical range is checked. `classes` lists the access classes a place may name, `A`, `D` and `P`
by default. A place with any other class is refused with exit 2 `memory-policy`; such an access
is `raw`.

An access is allowed only if every byte of it is allowed by the smallest region that contains
that byte. Regions nest or are disjoint, so a `deny` hole keeps a FIFO out of an `mmio` block even
when an access spans the hole. A byte in no region is refused. The refusal is exit 2
`memory-policy`, and it names the first refused byte and its region.

An `mmio` region declares its access `width`. `read mem` and `write mem` in it use that width by
default and refuse any other with exit 2 `memory-policy`. `word` is the default width elsewhere.

### Peripheral views

`[per.<name>]` opts one `PER.view` section into `read window per.<name>`. The `section` value is
the section name as PowerView's PER tree shows it. A peripheral view reads device registers, so
the team lists only sections without read side effects. Any other window is
`t32ctl raw window 'TEXT'`, which asks.

### Board variants

One profile serves every variant of a board. `[variant."<board target>"]` holds overrides of
`target`, `timing`, and `memory` (a `memory` override replaces the whole list), selected when the
build's `BOARD` equals the quoted board target exactly. Any other board target uses the base
profile. Procedures are shared, so a board has one bundle and one profile hash.

## Procedures

A procedure is a named, team-owned operation backed by `.cmm` scripts: attach, reset, a
self-test, a flash, anything the team adds. The team adds a name without a new t32ctl release.

### Declaration

| Key | Value | Required | Meaning |
|---|---|---|---|
| `about` | text | yes | One line for `read procs` and `--help`. |
| `script` | path | yes | Entry script, relative to the profile directory (the bundle root). |
| `grade` | `read`, `run`, `write`, `sys`, `burn` | yes | The grade the procedure runs at and the caller types. `raw` cannot be declared. |
| `lease` | `none`, `required` | no, `none` | `required`: the caller must hold the lease (exit 3 `lease-required`). |
| `timeout` | duration, 1s to 2h | yes | Bound of the run. A holder call keeps its lease while it runs. |
| `effects` | list | yes | What the procedure changes; the grade must be at least each effect's floor. |
| `outputs` | list of file names | no | Files the script writes into `&t32ctl_out`; a missing one makes the result exit 1. |
| `params.<name>` | table | no | Typed parameters. |

Effects and the lowest grade each one allows:

| Effect | Floor |
|---|---|
| `debugger-config` (symbols, breakpoints, debugger settings) | `run` |
| `target-run` | `run` |
| `target-reset`, `target-memory`, `target-registers` | `write` |
| `flash` (persistent storage) | `burn` |

The floor keeps a declaration consistent; it does not check what a script does. The team's
review of the scripts does.

### Parameters

The profile is the single source of truth for parameters. Scripts carry no parameter header, so
the grade, the lease rule, the timeout, the descriptions, and the types live in one place.

| `type` | Typed by the caller | The script reads `&t32ctl_p_<name>` as |
|---|---|---|
| `string` | text without `"`, `&` before a letter, or control characters; at most 256 bytes | the text |
| `integer` | `42`, `0x2a`, `-1`; within `min` and `max` | decimal with PRACTICE's dot: `42.` |
| `address` | any `LOC` notation | the resolved address with its class: `D:0x40100010` |
| `enum` | one of `values` | the value |
| `bool` | `true` or `false` | `TRUE()` or `FALSE()` |
| `file` | a WSL path; only on `write`, `sys`, and `burn` procedures | the PowerView path of the uploaded copy; quote it: `"&t32ctl_p_image"` |

A parameter with `default` is optional; without one it is required. A `file` default may contain
`{build}`, the binding's build directory, and nothing else. A `file` parameter moves a local file
into PowerView, so it is a validation error on a `read` or `run` procedure, which runs without an
approval. A parameter `foo_bar` is typed `--foo-bar VALUE`. `proc` flags are generated from the
profile at run time, so validation keeps parameter names out of the bypass pattern
([below](#validation)). A check the procedure performs, such as a readback verification, is part
of its script, not an option.

Before the run, t32ctl declares each macro `GLOBAL`, sets it over RCL, and clears it after the run
through the cleanup ledger. A `file` parameter is uploaded through the `VM:` window, in slices
appended to one file when it is larger than the window, and removed after the run. The script
writes its declared outputs into the directory `&t32ctl_out` and reports its result through two
global macros that t32ctl declares and empties before the run. `&t32ctl_rc` is `0.` for success;
any other value is a failure (exit 1, with the value in the report). `&t32ctl_msg` is one line of
text for the report. An empty `&t32ctl_rc` counts as success for `read`, `run`, `write`, and `sys`
procedures and for `raw script`, so a team script that knows nothing of t32ctl still works. It
counts as failure for a `burn` procedure, which must confirm success explicitly after its own
verification, so a flash that ends without saying so never clears the flash record. A PRACTICE
error is a failure whatever the macros hold. An `ENDDO` value is not read.

### Discovery

The agent finds procedures and their parameters through the tool, not by reading scripts.
`read procs` reads only the profile directory, so it works while PowerView is down.

```console
$ t32ctl read procs
proc      grade  lease     timeout  about
attach    run    -         30s      Connect to the A53 cluster without resetting it
reset     write  -         20s      Reset the SoC and halt at the reset vector
selftest  read   -         30s      Read clock and PMIC status into selftest.txt
flash     burn   required  45m      Write an image into eMMC and verify it by readback
boards/acme/acme_evb/support/t32ctl/ profile=1c07e9b2; parameters: t32ctl read procs NAME
[exit 0]
```

```console
$ t32ctl read procs flash
proc     flash  grade burn  lease required  timeout 45m
about    Write an image into eMMC and verify it by readback
params   --image FILE      image to write (default: {build}/zephyr/zephyr.bin)
         --part ENUM       eMMC hardware partition: boot0, boot1, user (default: user)
         --offset INTEGER  byte offset inside the partition, at least 0 (default: 0)
outputs  verify.log
profile  boards/acme/acme_evb/support/t32ctl/ profile=1c07e9b2
usage    t32ctl burn proc flash [--image FILE] [--part boot0|boot1|user] [--offset N]
[exit 0]
```

`read procs --json` and `t32ctl <grade> proc NAME --help` print the same declaration.

### Invocation and results

A procedure is invoked under the grade `read procs` prints. Any other grade word is exit 2
`grade-mismatch`, with the right command as `next:`.

```console
$ t32ctl read proc selftest
e5 stopped core=0 pc=0x40083e10 sym=main src=src/main.c:31
proc selftest (profile=1c07e9b2, 6 files, cached)
result: finished rc=0 after 2.4s: "clocks locked, pmic ok"
outputs: ~/.local/state/t32ctl/out/p-1f2c/ (1 file)
  selftest.txt  922
[exit 0]
```

The bundle is uploaded once per content and reused from PowerView's cache afterwards. The result
is exit 0 for rc 0; exit 1 for a non-zero rc, a PRACTICE error, a timeout, or a missing declared
output; exit 5 when the script opened a dialog or its run vanished. A `write`, `sys`, or `burn`
procedure bumps the epoch; a procedure whose effects touch the target records an intent
first, so a call that dies halfway is attributed correctly by the next one.

### Flashing an eMMC

Flashing is a board procedure, not a t32ctl command: the controller setup is board-specific. The
`flash` procedure above is the pattern.

- It is `burn`, so it always asks, even when the operator relaxed `write` or `sys`.
- It declares `lease = "required"`, so a flash, reset, load sequence cannot interleave with
  another agent.
- Its `image` parameter defaults to `{build}/zephyr/zephyr.bin`. The image is uploaded through
  `VM:` staging, in slices appended to one file when it is larger than the window; an image of
  hundreds of MB is several slices.
- `flash_emmc.cmm` resets and halts the SoC, calls `emmc_setup.cmm` to set up clocks and pinmux
  for the DesignWare MMC controller, loads the DesignWare MMC algorithm binary for AArch64 from
  the TRACE32 installation (`~~/`) into SRAM, selects the hardware partition from
  `&t32ctl_p_part`, writes `&t32ctl_p_image` at `&t32ctl_p_offset` with `FLASHFILE`, verifies by
  readback, and writes `verify.log` into `&t32ctl_out`. Verification is not optional. The
  algorithm binary ships with TRACE32 and is not in the repository.
- Its timeout may be up to 2 h. A holder call keeps the lease while it runs, so a long flash does
  not lose the lease mid-call. The agent runs such a call in the background, because it can
  outlast the agent's command timeout.
- t32ctl records the flash before it starts and clears the record only when the procedure
  completes with rc 0. Until a flash completes, every call reports it:

  ```text
  ! burn proc flash --part user did not complete (11:03:12, e41): the storage may hold a partial image
  ```

The literal sequence (take the lease, flash, reset, load symbols, run to `main`, release) is in
[cli-sessions.md](cli-sessions.md#7-emmc-flashing).

## Validation

A profile and the connection config are validated when they load. Every error has its file,
line, and column, and all errors are reported, not just the first:

- `schema` is `1`.
- An unknown key, a value of the wrong type, and a duplicate key or table are errors.
- The profile directory is a directory named `support/t32ctl`, the one path the deny rules cover.
  A binding that names any other directory is an error.
- The profile directory holds only regular files and directories. A symlink or a special file is
  an error naming the file. Hidden files and editor leftovers are ignored.
- Every `script` exists below the profile directory and is a relative path t32stage accepts.
- `grade` is one of `read`, `run`, `write`, `sys`, `burn`, and is at least the floor of each
  declared effect.
- Procedure and parameter names follow `[a-z][a-z0-9_]{0,23}`. Parameter names are unique after
  `_` becomes `-` and are not `json`, `lease`, `build`, `help`, `timeout`, or `out`.
- No parameter name matches the bypass pattern that the clap test applies to t32ctl's own flags:
  `force`, `yes`, `steal`, `unsafe`, `override`, a name that starts with `ignore`, `no_` or
  `skip`, or a `bool` that toggles verification (a name containing `verify`).
- A `file` parameter belongs to a `write`, `sys` or `burn` procedure.
- A default matches its type, an enum default is one of `values`, `min` is at most `max`, and
  `{build}` appears only in `file` defaults.
- Regions nest or are disjoint; a partial overlap is an error. An `mmio` region has a `width` of
  1, 2, 4, or 8.
- `classes` holds TRACE32 class names of 1 to 8 letters or digits, and `default_class` is one of
  them.
- `cores` is at least 1, and `word` is 1, 2, 4, or 8.
- A variant overrides only keys that exist in the base profile.
- A `section` is printable text without `"` or `&`.
- A duration outside its type's range is an error, never clamped.

A broken profile stops every call before it connects:

```console
$ t32ctl read status
error[profile-invalid]: boards/acme/acme_evb/support/t32ctl/profile.toml:83:1: unknown key `timout` in proc.reset
  expected one of: about, script, grade, lease, timeout, effects, outputs, params
[exit 5]
```

## Profile changes

Phase 1 has no operator trust. A procedure runs at the grade its profile declares, and the memory
map and the peripheral views are the profile as written. An agent that edits the profile can
therefore lower a procedure's grade or widen the memory map. Until a later ADR adds operator
trust, four things cover that gap:

- The generated permission snippet denies agent edits to `**/support/t32ctl/**` and
  `.t32ctl.toml`, so an agent's edit there is refused and the operator edits by hand
  ([cli.md](cli.md#grades-and-permission-rules)).
- The skill tells the agent never to edit profiles or their scripts.
- `read procs` and `read status --full` print the profile hash, so a changed profile is visible.
- The team reviews changes to the profile directory in git, like code.

Inside t32ctl, two functions decide a procedure's effective grade and the effective memory
policy, and both return what the profile declares. A trust layer replaces them without changing
the command line ([cli.md](cli.md#policy-hooks)).
