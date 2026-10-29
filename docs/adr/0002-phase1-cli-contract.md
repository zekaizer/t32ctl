# 2. Phase 1 command-line contract

Date: 2026-10-28

## Status

Accepted

## Context

The Phase 1 `t32ctl` CLI is called by a coding agent once per action. The agent sees only
`--help`, `describe`, a skill, and the command output. It copies the nearest example, retries what
failed, and appends any flag that made an error go away. A human operator approves the commands
the agent may not run alone, through Claude Code permission rules that match a prefix of the
command string. Those rules are a convenience, not a security boundary: they guard against an
agent's mistakes and shortcuts, and the firewall and the operator's approval are the boundary.

Several constraints pull against each other:

- Every call is a fresh process. State that must survive between calls lives in files, and the
  caller has no stable identity.
- Several agents and the operator's PowerView GUI share one target. A per-call lock serializes
  single calls, not an agent's sequence of calls.
- Board procedures are team-owned `.cmm` scripts declared in a profile that sits in the
  repository, where an agent can edit it.
- Every wait must be a bounded client poll. A PRACTICE `WAIT` or a cancel deadline poisons the
  RCL link.
- Five Phase 0 answers are still open and must not reach the contract.
- `t32rcl`'s `ErrorClass` says which layer failed, not what the caller should do next.

The contract is specified in [cli.md](../cli.md) and [profiles.md](../profiles.md), with literal
sessions in [cli-sessions.md](../cli-sessions.md).

## Decision

1. **Grade-first command tree.** The first word after `t32ctl` is a permission grade. The grades
   form a total order, `read < run < write < sys < burn < raw`, and `admin` belongs to the
   operator. `burn` writes persistent storage; `raw` runs arbitrary PRACTICE and can do anything,
   so it is the top. Every leaf lives under exactly one grade, and flags are accepted only after
   the grade word, so `Bash(t32ctl <grade>:*)` covers exactly that grade. The generated
   permission snippet allows `read` and `run`, asks for `write`, `sys`, `burn` and `raw`, denies
   `admin` and agent edits to profiles, the binding file, the config, and the state, and marks
   `burn` and `raw` as never-allow. A leaf under grade `G` receives a `Ctx<G>` whose operations
   live in impl blocks bounded by grade, so a leaf cannot do more than its first word says. No
   flag bypasses a check, and profile validation keeps procedure parameters out of the same
   bypass names.

2. **Procedures declared in the profile.** Each board has a profile directory that holds the
   profile and the team's scripts and is the bundle root. The profile declares board procedures
   with typed parameters, timeout, grade, effects, lease rule, and outputs. It is the single
   source of the parameters, which reach scripts as `&t32ctl_p_<name>` global macros. A
   procedure is invoked as `t32ctl <grade> proc NAME`, and the grade word must equal the
   procedure's effective grade. One function decides a procedure's effective grade and one
   decides the effective memory policy (memory map, access classes, peripheral views). In Phase 1
   both return what the profile declares, and a hash of the profile directory is printed so a
   change is visible. A repository script outside a profile runs as `raw script`. Flashing is a
   `burn` procedure, not a built-in command. A profile is required only where board knowledge
   is: memory reads, which need the map, and procedures. Status, register reads and `raw` run
   from the connection alone, without a CPU check, so a board without a profile can still be
   brought up and inspected. `read status` shows the missing profile; no notice repeats it on
   every call.

3. **A memory map guards every place.** Regions describe physical addresses, and a place in a
   translated access class is checked after translation. Every byte of an access must be allowed
   by its own smallest containing region, the accepted access classes are a closed list, and an
   exec breakpoint or a go target must be code. Procedures and `raw` are outside the map, which
   is one reason they ask.

4. **An opt-in, always-bounded lease.** An agent that must keep the target still across calls
   takes a lease and proves it on later calls with a `--lease` token; the state file stores only
   the token's hash. Ordinary calls take no lease and carry no token. While a lease is held,
   others may run short reads and wait for the lease, and everything else gets exit 4 with who
   holds it, why, and until when. A lease ends at release, at an operator break, after an idle
   timeout, or at an absolute cap; [cli.md](../cli.md#the-lease) has the defaults and limits. A
   holder call pins the lease only while it is alive, which a per-process lock proves, so a killed
   holder releases it at once. An ended lease never comes back; its holder takes a new one. Expiry
   is evaluated lazily from one WSL state file, so a stopped holder loses the lease without making
   a call. Every bound is a type with a maximum, and no setting removes one. The operator breaks a
   lease with `t32ctl admin lease break`.

5. **Exit codes by next action.** 0 ok, 1 outcome, 2 fix (the command text is wrong), 3 state
   (the target or debugger is not in the state the command needs; do what the report names, then
   rerun), 4 wait, 5 human, 130 interrupted. Errors are classified once, by RCL status code,
   `StageError` variant, and runner outcome, never by message text. A target-centric epoch
   detects changes made outside t32ctl. A `read` or `run` call prints the notice and acknowledges
   the change; `write` and above refuse with exit 3 until a `read` or `run` call has acknowledged
   it. A refusal acknowledges nothing, so a blind retry is refused again.

6. **One definition generates the self-description.** The clap derive is the command list, and
   every leaf carries its spec through a macro, so a leaf without one does not compile. The
   definition generates `--help`, `t32ctl read describe`, the `next:` hints, and every generated
   block: in the reference, the command, exit-code, and error-tag tables and the permission
   snippet; in the skill, the command and exit-code tables. A hint is built from the target
   leaf's argument type and rendered by the code that parses it, and it is always a complete
   command. Every value the tool prints is in argument notation, so a printed token pastes back
   as an argument. Tests parse every command line in the documentation, reject any flag or
   procedure parameter named like a bypass, and fail CI when a generated block drifts.

Profiles, the binding file, and the connection config are TOML. `toml` is the one new workspace
dependency.

## Consequences

- Every command carries one extra word, the grade. In exchange, the operator audits the
  permission rules by reading a handful of lines, and no argument order or hand-written rule
  grants more than one grade.
- Operator trust is deferred. An agent that edits a profile can lower a procedure's grade or
  widen the memory map, and Phase 1 runs what the profile says. Until trust exists, four things
  cover that gap: the generated deny rules on the profile directories and the binding file, the
  skill's rule never to edit profiles, the profile hash in `read procs` and `read status --full`,
  and git review of profile changes. A later ADR adds operator trust by replacing the two policy
  functions; the command line does not change.
- Holder calls repeat `--lease <token>`. Identity survives fresh shells, subagents, and
  sandboxes, and a forgotten token fails closed.
- The lease, the locks, and the epoch live in one WSL instance. Callers in another WSL instance or
  on another host share PowerView without being excluded.
- Without a lease, the epoch guard protects one caller: another caller's read acknowledges a
  change for everyone. Agents that share a debugger take a lease before a write sequence.
- The epoch misses a change that leaves the run state and the PC as they were, such as a memory
  poke from the GUI. Commands at `write` and above bump it unconditionally to cover their own
  effects.
- `read window` shows only views from a compiled table and peripheral sections the profile opts
  into. Any other window text is `raw`.
- A breakpoint condition is evaluated by TRACE32 at each hit, so a pointer it dereferences is not
  checked against the memory map.
- A long call, such as a flash, can outlast the agent's command. The skill runs it in the
  background, and the call keeps its lease while it is alive.
- `t32stage` gains debugger builders, parameter macros, and a sliced append upload, each assumed
  until a probe confirms the PowerView behavior it relies on. `t32probe` moves to the same
  per-PowerView state directory as `t32ctl`, so both share the `VM:` lock and the journals.

## Alternatives rejected

- **A verb-first tree with a grade per leaf** (`t32ctl bp set`, `t32ctl mem write`). The snippet
  becomes one rule per leaf, a hand-written `Bash(t32ctl mem:*)` silently allows writes, and the
  approval prompt hides the risk.
- **The grade as a flag or a session mode.** A prefix rule cannot see a flag reliably, and a mode
  is invisible to the rule.
- **One permission rule per procedure** (`Bash(t32ctl proc selftest:*)`). The operator edits the
  settings whenever the team adds a procedure, and the rule shows a name, not what the procedure
  may do.
- **Grades as a lattice where `burn` and `raw` are incomparable.** `raw` can do what `burn` does,
  so the lattice would understate it.
- **A built-in flash command.** The controller setup is board-specific and belongs to the team's
  scripts.
- **Parameter declarations in t32mcp-style `.cmm` headers.** They hold no grade, lease, timeout,
  or description, so the profile would need them anyway.
- **A Windows-resident `.cmm` library.** Its scripts would sit outside the bundle and its hash.
  Scripts may still use TRACE32 installation files under `~~/`, such as the eMMC algorithm.
- **Unmapped memory allowed with a notice.** An unmapped address is often a device whose read
  has side effects.
- **JSON output by default.** Text carries no keys, quotes, or braces, and `--json` renders the
  same report value.
- **The lease in a PowerView global macro.** RCL has no atomic compare-and-set, every call pays
  round trips, and the GUI user ignores it anyway.
- **Lease identity from an environment variable or the process tree.** A variable does not
  survive fresh shells and breaks prefix matching; process ancestry breaks in sandboxes.
- **A waiting queue for the lease.** It needs liveness for waiters that crash, for two or three
  agents per board.
- **Reviving an expired lease for its returning holder.** It needs a book of ended leases, and it
  can lock out a caller that started work after the expiry.
- **An explicit epoch argument** (`--seen e17`). It is a bookkeeping value the agent must carry
  and will copy wrong.
- **Exit codes from `ErrorClass`.** It puts input errors, contention, and operator problems in one
  class.
