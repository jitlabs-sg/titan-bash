# TITAN Bash

Modern interactive shell for Windows. No more Git Bash pain.

> Scope: TITAN Bash is **bash-inspired**, but it is **not GNU bash** and it is **not a POSIX shell**.

TITAN Bash is a Windows-native terminal host for running Windows tools reliably (`git`, `node`, `cargo`, `npm`, AI CLIs, dev servers), with a small set of bash-like interactive operators (`|`, redirects, `&&`, `||`, `;`, background jobs).

To make common “Unixy” commands available out-of-the-box, Windows releases bundle **busybox-w32** as `tools\busybox.exe` (providing applets like `grep`, `sed`, `awk`, `find`, `tar`, `gzip`, ...).

## What this is (and isn’t)

**TITAN Bash is:**
- A Windows-native interactive host for Windows-native CLI tools
- A lightweight bash-like operator layer (pipelines/redirects/conditionals/jobs)
- Windows reality-first: long paths, UNC paths, reserved device names

**TITAN Bash is not:**
- A bash interpreter (no `#!/bin/bash`, `if/for`, functions, `[[ ... ]]`, etc.)
- A Linux environment (use WSL when you need Linux semantics or real bash scripting)

## Features

- Reliable Ctrl+C: interrupt child processes without killing `titanbash`
- Tab completion: builtins + PATH executables + bundled BusyBox applets
- History search: `Ctrl+R` reverse search
- Streaming pipes & redirects: `|`, `>`, `>>`, `2>`, `2>>`, `|&`, `2>&1`
- Windows script dispatch: `.cmd/.bat` via `cmd.exe`, `.ps1` via PowerShell
- Background jobs: `command &` + `jobs` + `fg`/`wait`/`kill`
- Python venv: `venv\Scripts\activate` / `activate` / `deactivate` (updates `PATH` + shows `(venv)` in prompt)
- Path normalization: supports `C:\...`, `C:/...`, `/c/...`, `~`, `~user` where appropriate

## Installation

Releases ship two formats:
- Installer: `titan-bash-setup-<version>.exe` (installs and can add to `PATH`)
- Portable (full): `titanbash-<version>-portable.zip` (recommended; includes `tools\busybox.exe`)

Or build from source:

```powershell
cargo build --release
.\target\release\titanbash.exe
```

Packaging maintainers: see `PACKAGING.md`.

## Usage

```bash
# Interactive shell
titanbash

# Single command
titanbash -c "ls -la"

# Run Windows scripts directly
titanbash deploy.ps1
titanbash build.cmd
```

## Built-in commands

TITAN Bash includes a small set of built-ins (so `cd` works like a real shell and path handling is consistent):

- `cd`, `pwd`, `ls`, `cat`, `echo`, `clear`, `help`, `history`
- `activate`, `deactivate` (Python venv)
- `mkdir`, `rm`, `cp`, `mv`, `touch`
- `alias`, `unalias`, `export`, `env`/`printenv`, `which`
- `jobs`, `fg`, `wait`, `kill`
- `md5sum`, `sha1sum`, `sha256sum`, `sha512sum`

## Bundled BusyBox tools

Windows releases bundle busybox-w32 as `tools\busybox.exe`.

- Run any applet directly (TITAN Bash will dispatch unknown commands to BusyBox when appropriate): `grep`, `sed`, `awk`, `find`, `tar`, ...
- List available applets: `busybox --list`

See `THIRD_PARTY_NOTICES.md` for BusyBox license/source links.

## Multi-line paste

TITAN Bash enables “bracketed paste mode” in compatible terminals (Windows Terminal / VS Code), so multi-line pastes are treated as input and do not require a separate confirmation prompt.

If your terminal host still prompts on multi-line paste, you can disable it in the host settings:
- Windows Terminal: `"multiLinePasteWarning": false`
- VS Code: disable “Terminal: Enable Multi Line Paste Warning”

## License

MIT (see `LICENSE`). Third-party components: see `THIRD_PARTY_NOTICES.md`.
