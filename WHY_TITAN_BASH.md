# Why TITAN Bash?

TITAN Bash exists for one reason: Windows needs a stable, low-friction interactive terminal host for modern Windows CLI workflows.

> Scope (important)
>
> TITAN Bash is **bash-inspired**, but it is **not GNU bash** and it is **not a POSIX shell**.
> - It supports a small set of interactive operators (pipes, redirects, `&&`, `||`, `;`, background jobs).
> - It does not interpret bash scripts (`#!/bin/bash`, `if/for`, functions, `[[ ... ]]`, etc.).
> - If you need real bash scripting and GNU/Linux semantics, use **WSL** or a real bash distribution (MSYS2/Git Bash).

## The problem

Launching long-running, output-heavy processes on Windows (dev servers, watchers, Node CLIs, AI CLIs) requires a stable terminal host:
- stdout/stderr can be huge and continuous
- stdin must stay responsive (spinners/TUI)
- Ctrl+C must reliably stop the child and return you to a prompt

## Why not just use Git Bash / MSYS2?

Git Bash is great for bash scripting and GNU tooling, but for Windows-native interactive CLIs the extra PTY/path-translation layers can add friction and flakiness.

TITAN Bash is intentionally “Windows reality-first”: native process execution, predictable I/O, Windows script dispatch, and Windows path edge cases handled explicitly.

## Bundled BusyBox

To reduce “missing command” friction on Windows, releases bundle **busybox-w32** as `tools\busybox.exe` (providing `grep`, `sed`, `awk`, `find`, `tar`, ...).

BusyBox is not TITAN Bash; it is a separate executable used as a tool provider. See `THIRD_PARTY_NOTICES.md` for license/source links.

