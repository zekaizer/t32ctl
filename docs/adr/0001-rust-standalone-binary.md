# 1. Rust standalone binary with a native RCL client

Date: 2026-10-27

## Status

Accepted

## Context

t32ctl lets a coding agent drive a TRACE32 debugger. The agent and the tool run in WSL2. TRACE32
PowerView runs on the Windows host. The only channel between them is the TRACE32 Remote API (RCL)
over TCP. The tool runs once per agent call, so start-up cost and installation friction matter on
every call.

Two client stacks exist:

- pyrcl, the official Python package from Lauterbach (MIT). It covers memory, registers,
  breakpoints, PRACTICE macros, window content, and notifications. It needs a Python runtime and a
  managed environment on every machine that runs the tool.
- t32mcp `t32api.rs`, the official Rust port inside Lauterbach's MCP server (Apache-2.0). It covers
  attach, commands, functions, PRACTICE state, and register reads. Memory, macros, stop, and
  notifications are missing.

The design moves most work into PRACTICE scripts that PowerView runs, and moves files through
TRACE32 virtual memory. The client therefore needs a small, fixed set of RCL requests.

## Decision

Build t32ctl as a statically linked Rust binary (`x86_64-unknown-linux-musl`). Implement the RCL
client natively in a separate crate in the same workspace. Base it on t32mcp `t32api.rs` and port
the missing requests from pyrcl. Keep the attribution of both sources in `NOTICE` and in the
headers of ported files.

## Consequences

- Installation is one file. Start-up takes milliseconds, which fits one process per call.
- One clap definition can generate `--help`, a machine-readable command schema, and permission
  rule snippets.
- The project owns protocol correctness. Tests use golden byte vectors derived from the reference
  sources and canned frames at the transport boundary. No fake PowerView server is built.
- pyrcl stays useful as a reference and as an oracle during development. It is not a runtime
  dependency.
- The repository is licensed Apache-2.0 so that code ported from t32mcp keeps its license.
