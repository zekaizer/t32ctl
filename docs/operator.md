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
t32probe --scale 2                        slow PC: double every probe deadline
t32probe --timeout 20                     idle timeout per RCL call in seconds (default 10)
t32probe --prompt B::                     the PowerView command-line prompt was changed
t32probe --stage-root 'C:\t32stage'       stage files here instead of OS.PTD()\t32ctl
```

Opt-in probes, each behind its own flag:

| Flag | Probe | What it does to your PC |
|---|---|---|
| `--allow-dialog` | `dialog` | Opens a PowerView dialog for about a second and stops it. |
| `--allow-multi-client` | `multi-client` | Opens a second RCL connection. |
| `--allow-run-control --symbol main` | `run-control` | Runs the target to the symbol and halts it. Changes target state. |
| `--elf build/zephyr/zephyr.elf` | `elf-unzip` | Loads the ELF's symbols into PowerView (`/NoCODE /NoReg`, no target writes). The symbols stay loaded. |

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
  the affected probes `SKIP`; the probe never ends a script it did not start.
- On the WSL side: a lock file and a cleanup journal in `$XDG_STATE_HOME/t32ctl/` (default
  `~/.local/state/t32ctl/`). A run that dies leaves its journal; the next run replays it once
  connected and reports the result on a `residue` line.

## Reading the report

```text
t32probe 0.1.0 PowerView build=172803/base 172138 ~DVD 02/2025+ 127.0.0.1:20000 run=a41c wsl=nat
connect      PASS  tcp=0.40ms attach=3.10ms first=0.35ms host=127.0.0.1
version      PASS  build=172803 base=172138 dvd=02/2025+ min=ok nrusedcons=y elfunzip=y clearvm=?
reconnect    PASS  timeout=300.12ms kind=idle poisoned=ok revive=4.20ms first_call=2.98s
vm-rw        PASS  max=1MiB w=8.1MB/s r=9.4MB/s chunk=r15360/w15360(client-limit) restore=ok
gzip         FAIL  up=6.0MB/s unzip_max=8MiB corrupt=ACCEPTED(crc)
  why: /UNZIP accepted corrupt gzip: crc
skipped(3) prereq gzip FAIL: bundle script-error tar-download
not run (opt-in): dialog multi-client run-control elf-unzip
config      read_chunk=15360 write_chunk=15360 macro=bare keepalive=1.00s exists=OS.FILE.readable …
summary: pass=10 fail=1 unsup=0 skip=3 intr=0 | cleanup=clean | 38.2s | exit=1
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
- `RESIDUE <probe>: <what> by hand: <command>` lines name anything cleanup could not remove and
  the exact PowerView command to remove it.
- The `config` line holds the values Phase 1 pins from this run: memory chunk sizes, the EXP 2
  macro name spelling, the keepalive interval, the file-exists function and the stage-root mode.
  `?` means not measured.

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
30-second budget, marks the remaining probes `INTR` and prints the partial report. A second
Ctrl-C exits at once with 130; the JSON written after the previous probe stays valid, and the
journal lets the next run finish the cleanup.
