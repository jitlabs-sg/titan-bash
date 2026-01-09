//! Python virtual environment helpers.
//!
//! In Windows shells, "activating" a venv is fundamentally a parent-process concern:
//! it must mutate the current process environment so subsequent commands inherit it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use super::path;
use super::Shell;

const VAR_OLD_PATH: &str = "_TITANBASH_VENV_OLD_PATH";
const VAR_OLD_VENV: &str = "_TITANBASH_VENV_OLD_VIRTUAL_ENV";

pub fn try_activate_from_command(shell: &mut Shell, cmd0: &str) -> Result<Option<i32>> {
    let Some(venv_dir) = try_extract_venv_dir(&shell.cwd, cmd0) else {
        return Ok(None);
    };
    activate(shell, &venv_dir)?;
    Ok(Some(0))
}

pub fn activate(shell: &mut Shell, venv_dir: &Path) -> Result<()> {
    if !venv_dir.is_dir() {
        bail!("activate: not a directory: {}", venv_dir.display());
    }

    let scripts_dir = venv_dir.join("Scripts");
    if !scripts_dir.is_dir() {
        bail!("activate: not a venv (missing Scripts/): {}", venv_dir.display());
    }

    let pyvenv_cfg = venv_dir.join("pyvenv.cfg");
    let python_exe = scripts_dir.join("python.exe");
    if !pyvenv_cfg.is_file() && !python_exe.is_file() {
        bail!("activate: not a venv (missing pyvenv.cfg/python.exe): {}", venv_dir.display());
    }

    // Save original state once (so switching venvs is possible without stacking PATH prefixes).
    if !shell.vars.contains_key(VAR_OLD_PATH) {
        let cur_path = std::env::var("PATH").unwrap_or_default();
        shell.vars.insert(VAR_OLD_PATH.to_string(), cur_path);
        let cur_venv = std::env::var("VIRTUAL_ENV").unwrap_or_default();
        shell.vars.insert(VAR_OLD_VENV.to_string(), cur_venv);
    }

    let base_path = shell
        .vars
        .get(VAR_OLD_PATH)
        .cloned()
        .unwrap_or_default();
    let scripts_str = scripts_dir.to_string_lossy().to_string();
    let new_path = if base_path.is_empty() {
        scripts_str.clone()
    } else {
        format!("{};{}", scripts_str, base_path)
    };

    unsafe {
        std::env::set_var("PATH", new_path);
        std::env::set_var("VIRTUAL_ENV", venv_dir.to_string_lossy().to_string());
    }

    Ok(())
}

pub fn deactivate(shell: &mut Shell) -> Result<()> {
    let old_path = shell.vars.remove(VAR_OLD_PATH);
    let old_venv = shell.vars.remove(VAR_OLD_VENV);

    if let Some(p) = old_path {
        unsafe {
            std::env::set_var("PATH", p);
        }
    }

    match old_venv.as_deref() {
        Some(v) if !v.is_empty() => unsafe {
            std::env::set_var("VIRTUAL_ENV", v);
        },
        _ => unsafe {
            std::env::remove_var("VIRTUAL_ENV");
        },
    }

    Ok(())
}

pub fn find_default_venv_dir(cwd: &Path) -> Option<PathBuf> {
    for name in [".venv", "venv", "env"] {
        let p = cwd.join(name);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// If `cmd0` looks like a Windows venv activation script path (`.../Scripts/activate*`),
/// return the venv directory.
pub fn try_extract_venv_dir(cwd: &Path, cmd0: &str) -> Option<PathBuf> {
    let expanded = path::expand_env(cmd0);
    let resolved = path::resolve(cwd, &expanded);

    let file = resolved.file_name()?.to_string_lossy().to_string();
    let file_lower = file.to_ascii_lowercase();
    let is_activate = matches!(
        file_lower.as_str(),
        "activate" | "activate.bat" | "activate.cmd" | "activate.ps1"
    );
    if !is_activate {
        return None;
    }

    let scripts_dir = resolved.parent()?;
    let scripts_name = scripts_dir.file_name()?.to_string_lossy().to_string();
    if scripts_name.to_ascii_lowercase() != "scripts" {
        return None;
    }

    Some(scripts_dir.parent()?.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_venv_dir_from_activate_path() {
        let cwd = Path::new(r"D:\proj");
        let v = try_extract_venv_dir(cwd, r"venv\Scripts\activate").unwrap();
        assert_eq!(v, PathBuf::from(r"D:\proj\venv"));
    }

    #[test]
    fn test_extract_venv_dir_requires_scripts_parent() {
        let cwd = Path::new(r"D:\proj");
        assert!(try_extract_venv_dir(cwd, r"venv\activate").is_none());
        assert!(try_extract_venv_dir(cwd, r"activate").is_none());
    }
}

