# Running t32probe

`t32probe` measures what a live TRACE32 PowerView does over RCL and prints a report you paste
back into the chat. It runs inside WSL2 on the PC that runs PowerView. It never starts, quits or
reconfigures PowerView.

## Invocations

```text
t32probe                                  every safe probe, text report on stdout
t32probe --json probe.json                plus the full JSON report, rewritten after every probe
t32probe --only bundle                    one probe and the probes it requires
t32probe --skip gzip,tar-download         all safe probes except these, unless one is required
t32probe --list                           the catalogue: id, tier, deadline, prerequisites
t32probe --host 172.28.80.1 --port 20003  explicit endpoint instead of --host auto
t32probe --scale 2                        slow PC: double every probe and cleanup deadline
t32probe --timeout 20                     idle timeout per RCL call in seconds (default 10)
t32probe --prompt B::                     the PowerView command-line prompt was changed
t32probe --stage-root 'C:\t32stage'       stage files here instead of OS.PTD()\t32ctl
```

Opt-in probes, each behind its own flag:

| Flag | Probe | What it does to your PC |
|---|---|---|
| `--allow-dialog` | `dialog` | Opens a PowerView dialog for about a second and stops it. Check that the window closed. |
| `--allow-multi-client` | `multi-client` | Opens a second RCL connection, twice. |
| `--allow-run-control --symbol main` | `run-control` | Runs and halts the target, steps it, sets and deletes breakpoints. If the breakpoint list differs at the end, it is restored from the `STOre` script taken first. Changes target state. |
| `--loop-line main.c:42`, `--watch counter`, `--scratch-ram 0x80001000` | `run-control` | Optional: a `/COUNT 3` breakpoint on a line inside a loop, write breakpoints on a variable, and a RAM long and X9 written and restored. The address must be RAM nothing else uses. |
| `--allow-target-read --symbol main` | `target-read` | Reads registers, memory and symbols of the halted target and prints windows to files with `PRinTer.OPEN`. Leave no PowerView printer file open while it runs. |
| `--va 0xFFFF000000001000`, `--per-section GICD`, `--read-range 0x80000000++0x100000` | `target-read` | Optional: an address to translate, a peripheral section to print, and up to 16 MiB of memory to read over RCL and through a file. |
| `--elf build/zephyr/zephyr.elf` | `elf-unzip` | Loads the ELF's symbols into PowerView (`/NoCODE /NoReg`, no target writes). The symbols stay loaded. With `--symbol`, also asks the `sYmbol` functions. |
| `--allow-load-code --elf build/zephyr/zephyr.elf` | `load-code` | Loads the ELF's code into the halted target, then restores the PC. Target memory keeps the code. |

Every target command and function is asked with `PRACTICE.CoMmanD.AVAILable` or
`PRACTICE.FUNCtion.AVAILable` first; one this build does not offer is skipped and reported as
`unverified:<name>`. Every setting these probes change is restored right after the step that
measured it.

Asking for an opt-in probe without its flag is a usage error before anything is sent.

## What the default run does to your PC

- Files only below `OS.PTD()\t32ctl\`, in directories named `probe-<run id>`. Each probe removes
  what it created before the next one starts.
- `VM:` only inside `--vm-base`/`--vm-window` (default `0x01000000`, 64 MiB). Before the first
  write the probe reads the memory it will use; afterwards it writes it back and reads it again
  to verify. Memory that was never initialized is cleared again with `Data.CLEARVM`.
- PRACTICE global macros named `t32ctl_*` and `t32ctl_probe_*`. Macros cannot be deleted over
  RCL; they are set to the empty string.
- PRACTICE scripts it starts itself, only when no other script is running. A busy PowerView makes
  the affected probes `SKIP`.
- `END` to end a script it started. `END` is global: besides ending every PRACTICE script it
  closes every custom dialog and every file opened with `OPEN`, yours included (PRACTICE Script
  Language Reference Guide, `END`). `script-halt`, `script-error` and `macro-return` always send
  it; every script probe sends it when one of its scripts halts or times out. Close your own
  `DIALOG` windows and `OPEN` files before a run, or `--skip` those three probes.
- Script probes write their scripts and answer files below `OS.PTD()\t32ctl\probe-<run id>-<tag>`
  (`lab`, `halt`, `err`, `in`, `out`, `vis`, `ret`, `unp`, `bun`, ...). A script answers by saving
  a one-byte marker file such as `<case>-a.bin` or `<case>-k-yes.bin`; the probe checks whether it
  exists. At the end the directory is listed and files it did not expect (a `.bak` written by
  `SETUP.BAKfile`) are removed too and counted as `stray=`.
- `channel-out` creates an AREA window `T32PQ<RUN>` and deletes it right after reading it.
- Script probes create PRACTICE globals `t32ctl_q<run>_*` whose values carry the run id. Globals
  cannot be deleted over RCL; they are set to the empty string and their number is reported as
  `globals_left`.
- PowerView's GUI stalls for seconds while `vm-large` saves and loads the whole `VM:` window and
  while `keepalive` blocks in `WAIT 10.s`.
- On the WSL side: a lock file and a cleanup journal in `$XDG_STATE_HOME/t32ctl/pv/<host>-<port>/`
  (default `~/.local/state/t32ctl/pv/auto-20000/` for `--host auto`), keyed by `--host` as typed:
  the directory t32ctl uses for the same PowerView, so the two tools share the `VM:` lock and the
  journals. A run that dies leaves its journal; the next run replays it once connected and reports
  the result on a `residue` line.

## Reading the report

```text
t32probe 0.1.0 PowerView build=172803/base 172138 ~DVD 02/2025+ 127.0.0.1:20000 run=a41c wsl=nat
connect      PASS  tcp=0.40ms attach=3.10ms first=0.35ms host=127.0.0.1
version      PASS  build=172803 base=172138 dvd=02/2025+ min=ok nrusedcons=y elfunzip=y clearvm=?
reconnect    PASS  timeout=300.12ms kind=idle poisoned=ok revive=4.20ms first_call=2.98s
vm-rw        PASS  max=1MiB w=8.1MB/s r=9.4MB/s chunk=r15360/w15360(client-limit) restore=ok
gzip         FAIL  up=6.0MB/s unzip_max=8MiB corrupt=ACCEPTED(crc)
  why: /UNZIP accepted corrupt gzip: crc
skipped(1) prereq gzip FAIL: bundle
not run (opt-in): dialog multi-client run-control target-read load-code elf-unzip
config      read_chunk=15360 write_chunk=15360 macro=amp keepalive=1.00s exists=OS.FILE.readable …
summary: pass=10 fail=1 unsup=0 skip=1 intr=0 | cleanup=clean | 38.2s | exit=1
```

- Every line is at most 100 columns. A long line ends with `…`; the full data is in the JSON.
- The verdict column is `PASS`, `FAIL`, `UNSUP` (the PowerView build lacks the feature; not a
  failure), `SKIP` or `INTR` (Ctrl-C).
- Tokens are `key=value` with units: `ms`, `s`, `MB/s` (10^6 bytes per second), `KiB`, `MiB`.
- A `FAIL` adds at most two lines: `why:` and `hint:`. Skips with one cause collapse into one
  `skipped(n)` line that names the root cause.
- `revive=` and `first_call=` appear when a probe left the connection dead and the run reconnected
  once; `first_call` is the first request on the new connection.
- `restore=ok` means the `VM:` contents were written back and verified.
- `RESIDUE <probe>: <command>` lines give, one per line and never cut, the exact PowerView
  command that removes what cleanup could not. Where no command can undo it (a cleared macro, a
  `VM:` unit that would not take its old bytes back), the line reads
  `RESIDUE <probe>: <what> (<result>)`.
- The `config` line holds the values Phase 1 pins from this run: memory chunk sizes, the EXP 2
  macro name spelling, the keepalive interval, the file-exists function and the stage-root mode.
  `?` means not measured.

Tokens of the script probes:

- `oracle=fx` (`script-lab`): a script's answer files arrive; every other script probe relies on
  it. `FAIL` makes their results meaningless.
- `out=vmset|write|none` (`script-lab`): the channel scripts hand values back through besides the
  answer files; `none` means only yes/no answers are available.
- `wait_state=running|idle|mixed` (`script-lab`): how a script in `WAIT` reads. `idle` means a
  waiting script looks like a halted one.
- `dip=<ms>/<n>` (`script-lab`): the longest stretch a running script read as idle with a
  non-empty stack, and how many cases showed one; the Lab never ends a script on that alone.
- `t32stop=st0|st1|err<code>` (`script-halt`): the status `T32_Stop` answers while a script waits.
- `v1=<rd1>/<ex>/<rd2>/<exp>` (`macro-visibility`): whether a script reads a macro set over EXP 2
  (`val`), sees it empty, or halts, then what EXP reads after the script. `bundle` needs `val`.
- `guard=ok` (`channel-in`, `unpack-literal`): a script wrote nothing through an empty or relative
  path. `FAIL` means it would have written into PowerView's working directory.
- `gate=run|skip:a|skip:b|skip:c` (`bundle`): bundle ran, or which precondition kept it from
  running: (a) scripts do not see EXP-set macros, (b) ABSPATH changed an absolute path, (c) a
  script did not read the real snapshot path.
- `stray=<n>`: files the probe did not expect in its directory, removed during cleanup; their
  names are in the JSON (`stray_names`).
- `cut=<n>`: cases not started because the probe deadline was near; rerun with `--scale 2`.

`gzip` measures the `/UNZIP` ceiling only up to `--gzip-max-mib` (default 8). To find the real
ceiling, give it room: `--vm-window 0x08000000 --gzip-max-mib 32`.

`connect` failures carry a class: `refused` (PowerView not running, RCL off, wrong port),
`connect-timeout` and `unreachable` (wrong host for the WSL networking mode, or a firewall),
`not-rcl` (another service owns the port; its banner is shown), `no-answer` (TCP open but silent:
busy PowerView or a modal dialog), `closed-during-attach`, `attach-rejected` (demo mode blocks the
Remote API). The `hint:` line says what to check.

## Exit status

| Code | Meaning |
|---|---|
| 0 | Every selected probe passed, was unsupported, or was skipped for a reason other than a failure. |
| 1 | A probe failed, or the connection was lost and could not be restored for the rest of the run. |
| 2 | Command-line error; nothing was sent. |
| 130 | Interrupted. |

Cleanup residue does not change the exit status; it is reported on `RESIDUE` lines.

## Ctrl-C

The first Ctrl-C stops the current probe at its next RCL call, runs its cleanup with a fresh
budget (30 seconds plus time to write back and verify the `VM:` it touched at 1 MB/s, times
`--scale`), marks the remaining probes `INTR` and prints the partial report. A second Ctrl-C
exits at once with 130; the JSON written after the previous probe stays valid, and the journal
lets the next run finish the cleanup.
