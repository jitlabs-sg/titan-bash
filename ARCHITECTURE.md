# TITAN Bash Architecture

> Scope: TITAN Bash is **bash-inspired**, but it is **not GNU bash** and it is **not a POSIX shell**.

TITAN Bash focuses on being a stable Windows-native interactive host for running Windows tools, plus a small set of bash-like interactive operators.

## Goals

- Be a reliable interactive host for Windows-native CLI tools.
- Support common interactive operators: pipelines, redirects, `&&`, `||`, `;`, background jobs.
- Preserve native stdin/stdout/stderr behavior (colors, spinners, streaming output) without MSYS2/Cygwin PTY layers.
- Handle Windows path reality: long paths, UNC paths, reserved device names.

## Non-goals

- Full bash/POSIX scripting semantics (`#!/bin/bash`, `if/for`, functions, `[[ ... ]]`, etc.).
- Full job control like bash (process groups / TTY control).
- Shipping a full GNU userland.

If you need real bash scripting and GNU/Linux semantics, use **WSL** or a real bash distribution (MSYS2/Git Bash).

## Bundled tools (BusyBox)

Windows releases bundle **busybox-w32** as `tools\busybox.exe` to provide a wide set of Unix-style commands (`grep`, `sed`, `awk`, `find`, `tar`, ...).

- BusyBox is a separate executable and is not linked into `titanbash`.
- TITAN Bash exposes BusyBox applets in completion and can dispatch unknown commands to BusyBox when they are not found on `PATH`.

See `THIRD_PARTY_NOTICES.md` for BusyBox license/source links.

## Repository layout

```
src/
  main.rs            REPL entrypoint
  lib.rs             Library exports
  shell/
    mod.rs           Shell state, prompt rendering, dispatch
    input.rs         Crossterm-based line editor (Ctrl+C, history, paste)
    parser.rs        Bash-like parsing for interactive operators
    executor.rs      Builtins + native process spawning + streaming pipes/redirects
    completer.rs     Tab completion (builtins + PATH + BusyBox applets + filesystem)
    busybox.rs       BusyBox detection + applet list + PATH prepend
    path.rs          Windows path normalization helpers
    builtin.rs       Built-in commands
  task/
    mod.rs           Background job management
```

## Execution model (high-level)

- Builtins run in-process (so `cd` changes the shell’s current directory).
- External commands spawn as native Windows processes.
- Pipes and redirects are implemented with OS pipes and streaming I/O.
- Windows script dispatch is explicit:
  - `.cmd` / `.bat` via `cmd.exe`
  - `.ps1` via PowerShell

