//! BusyBox integration (bundled Unixy tools).
//!
//! We treat BusyBox as an optional sidecar executable (e.g. `tools\\busybox.exe`).
//! If present, titanbash can:
//! - expose BusyBox applets in tab completion
//! - fallback-dispatch unknown commands to `busybox <applet> ...`
//! - prepend the BusyBox directory to the process PATH (opt-in behavior at startup)

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

#[derive(Debug)]
pub struct Busybox {
    pub path: PathBuf,
    applets_lower: HashSet<String>,
}

static BUSYBOX: OnceLock<Option<Busybox>> = OnceLock::new();

pub fn get() -> Option<&'static Busybox> {
    BUSYBOX.get_or_init(detect).as_ref()
}

pub fn has_applet(name: &str) -> bool {
    let Some(bb) = get() else { return false };
    bb.applets_lower.contains(&name.to_ascii_lowercase())
}

pub fn applets() -> Vec<String> {
    let Some(bb) = get() else { return Vec::new() };
    let mut list: Vec<String> = bb.applets_lower.iter().cloned().collect();
    list.sort();
    list
}

pub fn prepend_busybox_dir_to_path() {
    let Some(bb) = get() else { return };
    let Some(dir) = bb.path.parent() else { return };
    let Some(dir_str) = dir.to_str() else { return };
    if dir_str.is_empty() {
        return;
    }

    let current = std::env::var("PATH").unwrap_or_default();
    if path_contains_entry(&current, dir_str) {
        return;
    }
    if current.is_empty() {
        // SAFETY: modifying PATH for child process resolution is intended behavior for a shell.
        unsafe { std::env::set_var("PATH", dir_str); }
        return;
    }

    let new_path = format!("{};{}", dir_str, current);
    unsafe { std::env::set_var("PATH", new_path); }
}

fn detect() -> Option<Busybox> {
    if let Ok(explicit) = std::env::var("TITANBASH_BUSYBOX") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return load(p);
        }
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    if let Some(dir) = exe_dir.as_ref() {
        let candidates = [
            dir.join("busybox.exe"),
            dir.join("tools").join("busybox.exe"),
        ];
        for c in candidates {
            if c.is_file() {
                return load(c);
            }
        }

        // Common layout: {root}\bin\titanbash.exe and {root}\tools\busybox.exe
        if let Some(parent) = dir.parent() {
            let candidates = [
                parent.join("busybox.exe"),
                parent.join("tools").join("busybox.exe"),
            ];
            for c in candidates {
                if c.is_file() {
                    return load(c);
                }
            }
        }
    }

    // Best-effort: allow a BusyBox already on PATH.
    if let Ok(p) = which::which("busybox") {
        if p.is_file() {
            return load(p);
        }
    }

    None
}

fn load(path: PathBuf) -> Option<Busybox> {
    let mut cmd = Command::new(&path);
    cmd.arg("--list");
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut applets_lower = HashSet::new();
    for token in stdout.split_whitespace() {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        applets_lower.insert(t.to_ascii_lowercase());
    }
    if applets_lower.is_empty() {
        return None;
    }
    Some(Busybox { path, applets_lower })
}

fn normalize_path_entry(s: &str) -> String {
    let trimmed = s.trim().trim_end_matches(['\\', '/']);
    trimmed.to_ascii_lowercase()
}

fn path_contains_entry(paths: &str, entry: &str) -> bool {
    let norm = normalize_path_entry(entry);
    for seg in paths.split(';') {
        if seg.is_empty() {
            continue;
        }
        if normalize_path_entry(seg) == norm {
            return true;
        }
    }
    false
}

pub fn resolve_busybox_argv(applet: &str, argv: &[String]) -> Option<Vec<String>> {
    let bb = get()?;
    if !has_applet(applet) {
        return None;
    }
    let mut out = Vec::with_capacity(argv.len() + 1);
    out.push(bb.path.to_string_lossy().to_string());
    out.extend_from_slice(argv);
    Some(out)
}

pub fn normalize_applet_name(cmd: &str) -> String {
    // BusyBox applets are invoked by name (no extension), so normalize
    // "grep.exe" -> "grep" and "grep" -> "grep".
    let lower = cmd.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if lower.ends_with(ext) {
            return lower.trim_end_matches(ext).to_string();
        }
    }
    lower
}

pub fn looks_like_path(cmd: &str) -> bool {
    cmd.contains('\\') || cmd.contains('/') || cmd.contains(':')
}
