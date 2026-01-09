# Third-party notices

TITAN Bash (`titanbash`) is licensed under the MIT License (see `LICENSE`).

Windows releases of this project bundle the following third-party software:

## busybox-w32 (BusyBox for Windows)

- Project: busybox-w32 (BusyBox ported to the Win32 API)
- Homepage: https://frippery.org/busybox/
- Source code: https://github.com/rmyorston/busybox-w32 (mirror) and https://gitlab.com/rmyorston/busybox-w32
- License: GNU General Public License v2.0 (GPL-2.0-only)
- Binary used by our releases: `busybox64u.exe` (x64 + Unicode) renamed to `tools\\busybox.exe` (and `busybox64a.exe` on Windows on ARM when applicable)

BusyBox is distributed as a standalone executable in `tools\\busybox.exe` and is not linked into `titanbash`.

For the GPLv2 license text, see `GPL-2.0.txt` (or https://www.gnu.org/licenses/old-licenses/gpl-2.0.txt).
