# Packaging

This repo ships two distribution formats:

1. **Portable (full)**: a zip containing `titanbash.exe` + `tools\busybox.exe`
2. **Installer**: a Windows setup EXE that installs to a directory and (optionally) adds it to `PATH`

> TITAN Bash is **not** GNU bash / POSIX; it is a Windows-native interactive host. See `README.md` for scope.

## Build portable

```powershell
cd D:\ocr\titan-bash
cargo build --release
.\target\release\titanbash.exe
```

The portable artifact is `target\release\titanbash.exe`.

## Build installer (Inno Setup)

Prerequisite: **Inno Setup 6+** (for `ISCC.exe`).

From repo root:

```powershell
.\scripts\package.ps1
```

Outputs go to `dist\`:
- `dist\titanbash-<version>-portable.exe`
- `dist\titanbash.exe` (optional convenience copy)
- `dist\titanbash-<version>-portable.zip` (recommended; includes `tools\busybox.exe`)
- `dist\titan-bash-setup-<version>.exe` (if Inno Setup is installed)

The packaging script downloads busybox-w32 into `dist\tools\busybox.exe` and bundles it into the portable zip + installer.

If `ISCC.exe` is not in `PATH`, install Inno Setup and re-run, or pass the path explicitly:

```powershell
.\scripts\package.ps1 -IsccPath "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
```

## CI

GitHub Actions builds the portable EXE + installer on Windows for tags `v*`:
- `.github/workflows/windows-release.yml`
