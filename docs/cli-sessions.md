# t32ctl sessions

Literal agent sessions against the Phase 1 contract in [cli.md](cli.md). Each block is a command,
its full output, and its exit code. Lines that start with `A$` and `B$` are two agents sharing one
debugger.

Each session has a status line. An `MVP` session runs on the binary as it is, and a test renders
its blocks (marked `transcript: mvp`) from report values and compares them verbatim, so they are
golden tests. A `planned` session is the contract of commands that wait on a target opt-in probe
or the state file ([cli.md](cli.md#planned-commands)); its command lines parse or name a planned
command.

Every session uses one example board, `acme_evb` (4 x Cortex-A53, Zephyr), whose `worker_loop`
thread writes through a NULL `item`. Its profile is the one in
[profiles.md](profiles.md#target-profile). The sessions stand alone.

## 1. Crash on a halted target

Status: planned: needs `read stack`, `read var`, and `read sym` (`target-read`, `elf-unzip`), and
the stop record of the state file.

Earlier, the agent set `b1` on `k_sys_fatal_error_handler` and left the target running with
`run go --detach`. It now comes back.

```console
$ t32ctl read status
e8 stopped core=2 pc=0x40081f10 sym=k_sys_fatal_error_handler src=kernel/fatal.c:44
! e7->e8 since the last call: the target stopped (breakpoint b1 on core 2)
target  acme_evb cpu=CortexA53 (matches profile) cores=4: 0 1 2 3 stopped
profile boards/acme/acme_evb/support/t32ctl/
elf     build/zephyr/zephyr.elf sha=3f2a91c0 code+symbols, loaded at e3, matches the build
stop    breakpoint b1 on core 2 (known: pc is the address of b1)
fault   esr_el1=0x96000045 data abort, write, translation fault level 1
        far_el1=0x8 elr_el1=0x40080b1c sym=worker_loop+0x84 src=src/worker.c:61
        spsr_el1=0x60000005 EL1h
bp      b1 exec k_sys_fatal_error_handler
lease   none
pv      build=172803 clients=1 practice=idle
next: t32ctl read stack
[exit 0]
```

```console
$ t32ctl read stack
e8 stopped core=2 pc=0x40081f10 sym=k_sys_fatal_error_handler src=kernel/fatal.c:44
frame  pc          sym                        src
0      0x40081f10  k_sys_fatal_error_handler  kernel/fatal.c:44
1      0x40081fa4  z_fatal_error+0x7c         kernel/fatal.c:124
2      0x4008f2d8  z_arm64_fatal_error+0x48   arch/arm64/core/fatal.c:221
3      0x4008e5c0  z_arm64_sync_exc+0x60      arch/arm64/core/vector_table.S:88
4      0x40080b1c  worker_loop+0x84           src/worker.c:61  fault (elr_el1)
5      0x40083a10  z_thread_entry+0x30        lib/os/thread_entry.c:48
frame 0 args
  reason = 0 (unsigned int)
  esf = 0x40107e60 (const struct arch_esf *)
frame 0 locals: none
next: t32ctl read stack --frame 4
[exit 0]
```

```console
$ t32ctl read stack --frame 4
e8 stopped core=2 pc=0x40081f10 sym=k_sys_fatal_error_handler src=kernel/fatal.c:44
frame 4  0x40080b1c  worker_loop+0x84  src/worker.c:61  fault (elr_el1)
args
  q = 0x40104000 (struct k_queue *)
locals
  item = 0x0 (struct work_item *)
  n = 17 (int)
[exit 0]
```

```console
$ t32ctl read sym work_item
e8 stopped core=2 pc=0x40081f10 sym=k_sys_fatal_error_handler src=kernel/fatal.c:44
type work_item  struct, size 24, src=src/worker.h:14
offset  member  type             size
0x0     node    sys_snode_t      8
0x8     state   enum work_state  4
0xc     flags   uint32_t         4
0x10    cb      work_cb_t        8
[exit 0]
```

```console
$ t32ctl read var 'q->head' --frame 4
e8 stopped core=2 pc=0x40081f10 sym=k_sys_fatal_error_handler src=kernel/fatal.c:44
q->head = 0x0 (sys_snode_t *, size 8 at 0x40104000)
[exit 0]
```

```console
$ t32ctl read mem @sp --len 64
e8 stopped core=2 pc=0x40081f10 sym=k_sys_fatal_error_handler src=kernel/fatal.c:44
mem 0x40107e40 len=64 width=8 class=D (region ram "ddr")
0x40107e40: 0x0000000040107e60 0x0000000040081fa4
0x40107e50: 0x0000000000000000 0x000000004008f2d8
0x40107e60: 0x0000000040080b1c 0x0000000060000005
0x40107e70: 0x0000000040104000 0x0000000000000008
[exit 0]
```

The agent has the whole story without a conversion step: `far_el1=0x8` is the offset of `state`,
`item` is NULL in frame 4 at `src/worker.c:61`, and the queue was empty. The frame number, the
type name, and the variable names came from the output as printed; nothing was converted. Every
cell of the dump is a valid argument as printed. The agent stops here and reports.

## 2. Break and inspect: step and a conditional breakpoint

Status: planned: needs run control and breakpoints (`run-control`).

```console
$ t32ctl run bp set worker_loop
e12 stopped core=0 pc=0x40000000 sym=__start src=arch/arm64/core/reset.S:29
bp b2 set: exec worker_loop = 0x40080a98 src=src/worker.c:52
next: t32ctl run go
[exit 0]
```

```console
$ t32ctl run go
e13 stopped core=1 pc=0x40080a98 sym=worker_loop src=src/worker.c:52
stop: breakpoint b2 on core 1 (known), after 840ms
next: t32ctl read stack
[exit 0]
```

```console
$ t32ctl read stack
e13 stopped core=1 pc=0x40080a98 sym=worker_loop src=src/worker.c:52
frame  pc          sym                  src
0      0x40080a98  worker_loop          src/worker.c:52
1      0x40083a10  z_thread_entry+0x30  lib/os/thread_entry.c:48
frame 0 args
  q = 0x40104000 (struct k_queue *)
frame 0 locals
  item = 0x0 (struct work_item *)
  n = 0 (int)
[exit 0]
```

```console
$ t32ctl run step over
e14 stopped core=1 pc=0x40080ab4 sym=worker_loop+0x1c src=src/worker.c:56
stop: step over done (known)
[exit 0]
```

```console
$ t32ctl run step over
e15 stopped core=1 pc=0x40080ad0 sym=worker_loop+0x38 src=src/worker.c:57
stop: step over done (known)
[exit 0]
```

```console
$ t32ctl read var 'item->state'
e15 stopped core=1 pc=0x40080ad0 sym=worker_loop+0x38 src=src/worker.c:57
item->state = WORK_PENDING (1) (enum work_state, size 4 at 0x40102348)
[exit 0]
```

```console
$ t32ctl run step
e16 stopped core=1 pc=0x40080c00 sym=process_item src=src/worker.c:30
stop: step into done (known)
[exit 0]
```

```console
$ t32ctl run step out
e17 stopped core=1 pc=0x40080ad8 sym=worker_loop+0x40 src=src/worker.c:57
stop: step out done (known), process_item returned -11 (int)
[exit 0]
```

The return value came from `x0`, typed by the declared return type of `process_item`, and printed
signed because `int` is signed. The function breakpoint is no longer useful. The agent swaps it
for a conditional one on the line that crashed in session 1.

```console
$ t32ctl run bp clear b2
e17 stopped core=1 pc=0x40080ad8 sym=worker_loop+0x40 src=src/worker.c:57
bp b2 cleared (exec worker_loop)
[exit 0]
```

```console
$ t32ctl run bp set src/worker.c:61 --if 'item == 0'
e17 stopped core=1 pc=0x40080ad8 sym=worker_loop+0x40 src=src/worker.c:57
bp b3 set: exec src/worker.c:61 = 0x40080b14 worker_loop+0x7c, if item == 0
next: t32ctl run go
[exit 0]
```

```console
$ t32ctl run go --wait 90s
e18 stopped core=1 pc=0x40080b14 sym=worker_loop+0x7c src=src/worker.c:61
stop: breakpoint b3 on core 1 (known), after 41.3s
next: t32ctl read stack
[exit 0]
```

The agent chose `--wait 90s` because the condition is rare, and kept the call under two minutes.

```console
$ t32ctl read var n
e18 stopped core=1 pc=0x40080b14 sym=worker_loop+0x7c src=src/worker.c:61
n = 17 (int, size 4 at 0x40107f8c)
[exit 0]
```

```console
$ t32ctl run bp set src/worker.c:61 --if 'item == 0'
e18 stopped core=1 pc=0x40080b14 sym=worker_loop+0x7c src=src/worker.c:61
bp b3 already set: exec src/worker.c:61 = 0x40080b14 worker_loop+0x7c, if item == 0
next: t32ctl run go
[exit 0]
```

The repeated `bp set`, copied from earlier in the context, changed nothing: its address, kind,
length, condition, and count are those of `b3`. It returned `b3` and the same hint as the first
set.

```console
$ t32ctl run go --detach
e19 running cores=0-3
go: started all cores; not waiting. The next call reports any stop.
[exit 0]
```

## 3. Stale epoch: the target changed under the agent

Status: planned: needs the epoch (the state file) and `write var`.

The agent read `g_mode` and planned to write it. Meanwhile the operator pressed Go and Break in
the PowerView GUI.

```console
$ t32ctl read var g_mode
e20 stopped core=0 pc=0x40081234 sym=k_cpu_idle+0x8 src=arch/arm64/core/cpu_idle.S:30
g_mode = MODE_SCAN (3) (enum mode, size 4 at 0x40100010)
[exit 0]
```

```console
$ t32ctl write var g_mode MODE_IDLE
e21 stopped core=0 pc=0x40085c40 sym=scan_step+0x2c src=src/scan.c:118
! e20->e21 changed outside t32ctl: the target ran and stopped at pc=0x40085c40 (cause unknown)
error[changed-externally]: the target changed since your last observation; nothing was written
  a refused write does not acknowledge the change: re-read what this write was based on, then
  run it again
next: t32ctl read var g_mode
[exit 3]
```

A blind retry of the same write would be refused the same way, because a refusal does not
acknowledge the change. The agent follows the hint.

```console
$ t32ctl read var g_mode
e21 stopped core=0 pc=0x40085c40 sym=scan_step+0x2c src=src/scan.c:118
! e20->e21 changed outside t32ctl: the target ran and stopped at pc=0x40085c40 (cause unknown)
g_mode = MODE_REPORT (2) (enum mode, size 4 at 0x40100010)
[exit 0]
```

```console
$ t32ctl write var g_mode MODE_IDLE
e22 stopped core=0 pc=0x40085c40 sym=scan_step+0x2c src=src/scan.c:118
g_mode: MODE_REPORT (2) -> MODE_IDLE (0), read back MODE_IDLE (0)
[exit 0]
```

The refused write did nothing. The `next:` hint was the read the write depended on, built from
the write's own arguments. The read was the first call allowed to acknowledge the change: it
printed the notice again and acknowledged it, and then the same write went through. The guard
protects one caller: without a lease, another caller's read would have acknowledged the change
for this agent too. Agents that share the debugger take a lease before a write sequence.

## 4. A failing repository script

Status: MVP.

The agent runs a team script set from the repository. `raw` asks the human every time, because a
repository script is text an agent can write. Before the approval, the agent says in one
sentence what the script does and what it changes: it writes test patterns into the `0x100000`
bytes of DDR at `0x40000000` and logs the result into `stress.log`.

```console
$ t32ctl raw script tools/t32/ddr_stress.cmm -- 0x40000000 0x100000
e23 stopped core=0 pc=0x40085c40 sym=scan_step+0x2c src=src/scan.c:118
script tools/t32/ddr_stress.cmm (bundle tools/t32, 6 files, cached)
result: halted by a PRACTICE error after 3.2s
  message: memory access failed at A:0x40000400
  line:    Data.Test A:0x40000000++0xfffff /Prime
  the script was unwound; staged files were removed
outputs: ~/.local/state/t32ctl/out/s-0b91/ (1 file)
  stress.log  2150
[exit 1]
```

The script ran and failed, so the class is outcome (1), not fix (2): the agent reads
`stress.log`, not the command line. The epoch moved (e22 to e23) although the PC did not: a
`write`, `sys`, `burn` or `raw` command always bumps it, because it may have changed memory. A
script that opens a dialog ends with exit 5 (`error[dialog]`) instead, because only the operator
can close a PowerView window.

## 5. A procedure under the wrong grade

Status: MVP.

The agent wants to reset the board. It copies the shape of `t32ctl run proc attach` from earlier
in its context.

```console
$ t32ctl run proc reset
e23 stopped core=0 pc=0x40085c40 sym=scan_step+0x2c src=src/scan.c:118
error[grade-mismatch]: procedure reset is declared write in the board profile; the grade word
  must be write
next: t32ctl write proc reset
[exit 2]
```

```console
$ t32ctl write proc reset
e24 stopped core=0 pc=0x00000000
proc reset (profile=1c07e9b2, 6 files, cached)
result: finished rc=0 after 1.9s
[exit 0]
```

The grade word must equal the grade the profile declares, so a procedure cannot be run under a
lower grade by typing one. The hint was the same call under the declared grade, and the approval
prompt showed `t32ctl write proc reset`. The declared grade lives in the board profile, which the
permission snippet keeps agents from editing and the team reviews in git; Phase 1 has no
operator trust beyond that ([profiles.md](profiles.md#profile-changes)).

## 6. Two agents and the lease

Status: planned: needs the lease (the state file).

Agent A is stepping through `worker_loop` and must not have the target resumed under it.

```console
A$ t32ctl run lease take --purpose "step through worker_loop crash"
e30 stopped core=1 pc=0x40080a98 sym=worker_loop src=src/worker.c:52 lease=L7f3e:you
lease L7f3e taken: idle timeout 10m, ends by 10:44:12 at the latest
  token tk-4b1d9c2e07a5f316: pass --lease tk-4b1d9c2e07a5f316 on every call while you hold it
[exit 0]
```

Agent B, in another Claude Code session, wants to run the target.

```console
B$ t32ctl run go
e30 stopped core=1 pc=0x40080a98 sym=worker_loop src=src/worker.c:52 lease=L7f3e:other
error[lease-held]: lease L7f3e "step through worker_loop crash" holds the debugger
  until 10:44:12 at the latest; it ends earlier after 10m without a call from its holder
  (last call 41s ago). Short reads are allowed. If you took this lease, pass its --lease token.
  To wait for it, take the lease with your own --purpose and --wait up to 1m30s.
[exit 4]
```

The refusal has no `next:` line: taking the lease needs a purpose, and only B knows it. Short
reads still work.

```console
B$ t32ctl read var g_mode
e30 stopped core=1 pc=0x40080a98 sym=worker_loop src=src/worker.c:52 lease=L7f3e:other
g_mode = MODE_SCAN (3) (enum mode, size 4 at 0x40100010)
[exit 0]
```

At 10:15:40 B starts `t32ctl run lease take --purpose "check UART init after reset" --wait 90s`.
The call polls outside `call.lock` and marks the lease contended until its own deadline,
10:17:10. Meanwhile A's header shows that someone is waiting.

```console
A$ t32ctl run step over --lease tk-4b1d9c2e07a5f316
e31 stopped core=1 pc=0x40080ab4 sym=worker_loop+0x1c src=src/worker.c:56 lease=L7f3e:you,contended
! another caller has been waiting for the lease since 10:15:40, until 10:17:10 at the latest
stop: step over done (known)
[exit 0]
```

```console
A$ t32ctl run lease release --lease tk-4b1d9c2e07a5f316
e31 stopped core=1 pc=0x40080ab4 sym=worker_loop+0x1c src=src/worker.c:56
lease L7f3e released after 2m40s; target left stopped (core 1), breakpoints b2
[exit 0]
```

B's waiting call returns as soon as A releases:

```console
B$ t32ctl run lease take --purpose "check UART init after reset" --wait 90s
e31 stopped core=1 pc=0x40080ab4 sym=worker_loop+0x1c src=src/worker.c:56 lease=La21c:you
lease La21c taken after waiting 1m12s: idle timeout 10m, ends by 10:46:52 at the latest
  token tk-90e1a7d36b2c4f08: pass --lease tk-90e1a7d36b2c4f08 on every call while you hold it
previous lease L7f3e "step through worker_loop crash": released at 10:16:52
  target left stopped at pc=0x40080ab4 (core 1); t32ctl breakpoints: b2 (set under L7f3e)
[exit 0]
```

B resets the board and leaves it running. Its last call is at 10:19:05. Then B's human
interrupts B and leaves, and nobody calls for ten minutes. B's lease ends at 10:29:05 without any
call from B. Later A wants the debugger again.

```console
A$ t32ctl run lease take --purpose "verify fix for NULL item"
e33 running cores=0-3 lease=Lc4d0:you
lease Lc4d0 taken: idle timeout 10m, ends by 11:01:01 at the latest
  token tk-e2b04f7193d8a6c5: pass --lease tk-e2b04f7193d8a6c5 on every call while you hold it
previous lease La21c "check UART init after reset": expired at 10:29:05 (no call for 10m)
  target left running since e33
[exit 0]
```

B comes back and uses its old token.

```console
B$ t32ctl run halt --lease tk-90e1a7d36b2c4f08
e33 running cores=0-3 lease=Lc4d0:other
error[lease-ended]: your lease La21c expired at 10:29:05 (no call for 10m); nothing was done
  lease Lc4d0 "verify fix for NULL item" holds the debugger until 11:01:01 at the latest.
  An ended lease never comes back. Drop --lease to read; to act, take a new lease with your
  own --purpose and --wait up to 1m30s.
[exit 3]
```

An ended lease never comes back: its token gives `lease-ended` while it is the previous lease,
whether or not someone took the debugger since. B takes a new lease if it still needs one, and `run
lease take` reports how the previous lease ended. A token that matches neither the current nor the
previous lease is exit 2 `unknown-lease`. A token that is dead or missing never acts.

## 7. eMMC flashing

Status: MVP, without the lease, `run symbols`, and `run go`, which are planned.

The agent rebuilt the application. The board boots from eMMC, and PowerView still holds the
symbols of the previous build (`elf=stale`). The flash procedure needs the lease, so the agent
finds that first, then takes the lease for the whole sequence and tells its human that the flash
and the reset will each ask for an approval.

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

```console
$ t32ctl run lease take --purpose "flash user partition and boot it"
e40 running cores=0-3 lease=L2b9d:you elf=stale
lease L2b9d taken: idle timeout 10m, ends by 11:32:05 at the latest
  token tk-5c0e8a17d2f94b60: pass --lease tk-5c0e8a17d2f94b60 on every call while you hold it
[exit 0]
```

Right before the approval-gated flash, the agent checks the state once more. A change made in
the GUI after the lease was taken would otherwise refuse the flash with exit 3 after the human had
already approved it.

```console
$ t32ctl read status --lease tk-5c0e8a17d2f94b60
e40 running cores=0-3 lease=L2b9d:you elf=stale
target  acme_evb cpu=CortexA53 (matches profile) cores=4: 0 1 2 3 running
profile boards/acme/acme_evb/support/t32ctl/
elf     build/zephyr/zephyr.elf sha=3f2a91c0 code+symbols, loaded at e3, stale: the build changed
stop    none since e3
bp      none
lease   L2b9d you "flash user partition and boot it", idle timeout 10m, ends by 11:32:05
pv      build=172803 clients=1 practice=idle
[exit 0]
```

The flash may take up to its 45-minute
timeout, longer than the agent's command timeout, so the agent runs it with the shell tool's
background mode. The t32ctl process blocks until the flash is done, and its exit code is the
result. The call keeps the lease while it runs. The human approves the `burn` prompt.

```console
$ t32ctl burn proc flash --part user --lease tk-5c0e8a17d2f94b60
e41 stopped core=0 pc=0x00100840 lease=L2b9d:you elf=stale
proc flash (profile=1c07e9b2, 6 files, cached)
params: image=build/zephyr/zephyr.bin (2411724, 1 slice) part=user offset=0
result: finished rc=0 after 38.4s: "wrote 2411724 bytes to user at 0x0, verified"
outputs: ~/.local/state/t32ctl/out/p-3e07/ (1 file)
  verify.log  1229
[exit 0]
```

The target is halted inside the flash algorithm in SRAM. A reset boots the new image.

```console
$ t32ctl write proc reset --lease tk-5c0e8a17d2f94b60
e42 stopped core=0 pc=0x00000000 lease=L2b9d:you elf=stale
proc reset (profile=1c07e9b2, 6 files, cached)
result: finished rc=0 after 1.9s
[exit 0]
```

The header still says `elf=stale`, and the agent loads symbols only: the code in the target is
the image it just flashed.

```console
$ t32ctl run symbols --lease tk-5c0e8a17d2f94b60
e42 stopped core=0 pc=0x00000000 lease=L2b9d:you
symbols build/zephyr/zephyr.elf sha=8d04e6b1 symbols only, uploaded 6081740 in 2.7s
[exit 0]
```

```console
$ t32ctl run go main --lease tk-5c0e8a17d2f94b60
e43 stopped core=0 pc=0x40083e10 sym=main src=src/main.c:31 lease=L2b9d:you
stop: reached main on core 0 (known), after 1.5s
next: t32ctl read stack --lease tk-5c0e8a17d2f94b60
[exit 0]
```

```console
$ t32ctl run lease release --lease tk-5c0e8a17d2f94b60
e43 stopped core=0 pc=0x40083e10 sym=main src=src/main.c:31
lease L2b9d released after 2m31s; target left stopped (core 0), breakpoints none
[exit 0]
```

Had the flash died halfway, every later call would print this notice until a flash completes:

```text
! burn proc flash --part user did not complete (11:03:12, e41): the storage may hold a partial image
```

## Workflow coverage

Each sequence is what the agent types, without the leading `t32ctl`. "Friction" lists every
conversion, re-typed value, bookkeeping call, or inconsistent notation in it; "none" means every
argument was copied from the previous output or is a name the agent chose. "Status" says which
steps the MVP binary has.

| # | Workflow | Sequence | Friction | Status |
|---|---|---|---|---|
| 1 | Bring-up, code load | `read status` -> `read procs` -> `run proc attach` -> `write proc reset` -> `write load` -> `run go main` | None. `attach` and `reset` are well-known names, and `read procs` shows which ones the board has; without `reset`, the agent asks its human. The CPU check is skipped for `attach`, which declares `debugger-config`. | partly: `write load`, `run go` planned |
| 1b | Bring-up, boots from flash | `read status` -> `read procs` -> `run proc attach` -> `write proc reset` -> `run symbols` -> `run go main` | None. | partly: `run symbols`, `run go` planned |
| 2 | Crash on a halted target | `read status` (stop, decoded fault registers, faulting location) -> `read stack` -> `read stack --frame N` -> `read sym TYPE` or `read var EXPR --frame N` -> `read mem @sp --len 64` | None. `N` is the frame marked `fault`, copied as printed. Memory cells print `0x`-prefixed, so a pointer in a dump pastes as an argument. | planned: `read stack`, `read sym`, `read var` |
| 3 | Break and inspect | `run bp set worker_loop` -> `run go` -> `read stack` -> `run step over`, `run step`, `run step out` (reports the return value) -> `read var 'EXPR'` -> `run go` (next hit) -> `run bp clear b2` | Expressions with `->`, `*`, `&`, or spaces need single quotes (shell). | planned: `run-control` |
| 3b | Conditional or counted breakpoint | `run bp set src/worker.c:61 --if 'item == 0'` or `--count 5` -> `run go --wait 90s` | Same quoting. `--count` counts the hits where the condition is true. | planned: `run-control` |
| 4 | Hang on a running target | `read status` (running) -> `run halt` (per-core report) -> `read stack --core N` -> `read var X` -> `run go --detach` | None. `N` is copied from the halt report. | planned: `run halt`, `read stack`, `read var` |
| 5 | Data corruption | `run bp set g_cfg --on write` (or `0x40100000 --on write --len 64`) -> `run go --wait 90s` -> the stop names the writer's pc, sym, and src -> `read stack` | The stop is reported as `watchpoint b4 (inferred)`; the PC is the instruction after the store, and the report says so. Watchpoints are on-chip and few (`no-hw-resource` when none is free), and a DMA writer does not trigger them. Forgetting `--on write` is exit 2 `not-code`. | planned: `run-control` |
| 6 | Hypothesis test | `read status` -> `write var g_mode MODE_IDLE` (or `write reg x0 0x1`, `write mem 0x40100010 0x5`) -> `run go` -> `read var g_status` | None. Each write reports old, new, and read-back values. `read status` first keeps an exit 3 from wasting the approval; with other agents on the debugger, the agent takes a lease first. | planned: `write var`, `write reg`, `write mem`, `run go` |
| 7 | Reset and rerun after a rebuild | (agent rebuilds) -> the header shows `elf=stale` -> `write proc reset` -> `write load` -> `run go src/main.c:88` -> `read var x` | None. The `elf=stale` hint is `run symbols`; loading new code with `write load` is the agent's decision, and it asks each time by design. | planned: `write load`, `run go`, `elf=stale` |
| 8 | Peripheral and system registers | `read reg sctlr_el1 mpidr_el1` -> `read mem 0x08000000 --len 0x40` (the region's width, 4) -> `read window --list` -> `read window per.gicd` | None for listed views. Any other window is `raw window`, which asks. | partly: `read reg` and `read mem` MVP; `read window` planned |
| 9 | SMP | `read status` (all cores) -> `read stack --core 3` -> `run halt` -> `run go --detach` | None. Run control acts on all cores (TRACE32 SMP mode). | planned: `read stack --core`, `run halt`, `run go` |
| 10 | Team scripts | `raw script tools/t32/ddr_stress.cmm -- 0x40000000 0x100000` -> read the printed output files | Asks every time: a repository script is text an agent can write. The agent may read the script to describe it, and says what it does and changes before the approval. Arguments are positional and may not contain spaces or quotes (runner rule). A script the team runs often belongs in the profile as a procedure. | MVP |
| 11 | Bulk data | `read mem 0x40000000 --len 64M --to ddr.bin`; `write mem 0x40200000 --from blob.bin` | None. `--to` takes a plain file name, and the report prints the path under `~/.local/state/t32ctl/out/`. A transfer is bounded (10 minutes by default), and a large one runs in the background. | partly: `read mem --to` MVP; `write mem --from` planned |
| 12 | Symbol questions | `read sym 0x40080b1c`; `read sym worker_loop`; `read sym src/worker.c:61`; `read sym work_item` | None. Works without running the target. | planned: `elf-unzip` |
| 13 | eMMC flashing | `read procs flash` -> `run lease take --purpose "flash user partition and boot it"` -> `burn proc flash --part user --lease T` -> `write proc reset --lease T` -> `run symbols --lease T` -> `run go main --lease T` -> `run lease release --lease T` | The token is repeated on each call: the cost of identity across fresh processes. The flash can outlast the agent's command timeout, so the agent runs it in the background. The human approves the flash and the reset. | MVP without the lease: `read procs flash` -> `read status` -> `burn proc flash --part user` -> `write proc reset`; `run symbols`, `run go` planned |
| 14 | Attach to a live target without stopping it | `run proc attach` -> `run symbols` -> `read status` (running) -> `read var` on regions with `running = true` | Reads elsewhere refuse with exit 3 `not-halted` until `run halt`. | partly: `run proc attach`, `read mem` on `running = true` regions MVP; `run symbols`, `read var`, `run halt` planned |
| 15 | Recover after an interrupted call | `read status`: notices for replayed journals, an interrupted call's intent, an incomplete flash, a missing instance marker, and residue with the PowerView command to type | None. Every call repairs first, so no recovery command exists. | partly: replayed journals and residue MVP; intents, an incomplete flash, the instance marker planned |
| 16 | Hand the target back | `run bp clear --all` -> `run go --detach` -> `run lease release` | None. `--all` clears every t32ctl breakpoint, other agents' too. | planned: `run bp clear`, `run go`, the lease |
| 17 | Procedure under the wrong grade | `run proc reset` -> exit 2 `grade-mismatch` -> `write proc reset` (asks) | None. The hint is the call under the declared grade. | MVP |
| 18 | Debugger held by another agent | `read status` (`lease=<id>:other`) -> `run lease take --purpose "..." --wait 90s`, or work that does not need the target | One wait, then other work; never a loop. Short reads work meanwhile. | planned: the lease |

Not served in Phase 1, deliberately: live variable watching and stop notifications (`watch`,
Phase 2), trace and FDX, OS-aware thread lists (Phase 3), independent per-core run control (AMP),
one call spanning two PowerViews, and starting PowerView.
