//! Path normalization - the core feature of TITAN Bash
//!
//! Accepts all path formats:
//! - C:\Users\xxx     (Windows native)
//! - C:/Users/xxx     (Forward slashes)
//! - /c/Users/xxx     (Git Bash style)
//! - ~/Documents      (Home directory)
//! - ~username/docs   (Other user's home)
//! - Mixed: C:/Users\xxx (because why not)
//! - \\server\share   (UNC network paths)

use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::path::{Component, Prefix};

/// Fallback base directory for user home directories on Windows
#[cfg(windows)]
const FALLBACK_USER_HOME_BASE_DIR: &str = "C:\\Users";

/// Windows reserved device names that cannot be used as file names.
/// See: https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
///
/// These names (with or without extension) are reserved:
/// - CON, PRN, AUX, NUL
/// - COM1-COM9, COM superscripts
/// - LPT1-LPT9, LPT superscripts
#[cfg(windows)]
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL",
    "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    // Superscript variants (rare but valid on some Windows versions)
    "COM\u{00B9}", "COM\u{00B2}", "COM\u{00B3}",  // COM1, COM2, COM3
    "LPT\u{00B9}", "LPT\u{00B2}", "LPT\u{00B3}",  // LPT1, LPT2, LPT3
];

/// Check if a path refers to a Windows reserved device name.
///
/// These are special paths that can be read/written but don't appear as regular files.
/// Attempting to create a file with these names will either fail or have unexpected behavior.
#[cfg(windows)]
pub fn is_windows_reserved_name(path: &Path) -> bool {
    // Check for device namespace prefix (\\.\)
    if let Some(Component::Prefix(prefix)) = path.components().next() {
        if matches!(prefix.kind(), Prefix::DeviceNS(_)) {
            return true;
        }
    }

    // Get the file stem (name without extension)
    let name = path.file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().to_uppercase());

    match name {
        Some(n) => WINDOWS_RESERVED_NAMES.iter().any(|reserved| {
            n == reserved.to_uppercase()
        }),
        None => false,
    }
}

#[cfg(not(windows))]
pub fn is_windows_reserved_name(_path: &Path) -> bool {
    false
}

/// Check if a path is a Windows device path (\\.\device)
#[cfg(windows)]
pub fn is_windows_device_path(path: &Path) -> bool {
    if let Some(Component::Prefix(prefix)) = path.components().next() {
        matches!(prefix.kind(), Prefix::DeviceNS(_))
    } else {
        false
    }
}

#[cfg(not(windows))]
pub fn is_windows_device_path(_path: &Path) -> bool {
    false
}

/// Get the error message for a reserved name
pub fn reserved_name_error(name: &str) -> String {
    format!(
        "'{}' is a Windows reserved device name and cannot be used as a file/directory name. \
         Reserved names include: CON, PRN, AUX, NUL, COM1-9, LPT1-9",
        name
    )
}

/// Get home directory for a specific username (Windows implementation)
#[cfg(windows)]
fn user_home_dir(username: &str) -> PathBuf {
    // Try to get current user's home and derive other user's home from it
    match dirs::home_dir() {
        Some(current_home) => {
            // Check if the last component matches username
            if current_home
                .components()
                .next_back()
                .map(|last| {
                    if let std::path::Component::Normal(name) = last {
                        name.to_string_lossy() != username
                    } else {
                        true
                    }
                })
                .unwrap_or(true)
            {
                // Different user - construct path from base dir
                let mut path = current_home.clone();
                path.pop();  // Remove current username
                path.push(username);

                if path.is_dir() {
                    path
                } else {
                    // Fallback to C:\Users\username
                    PathBuf::from(format!("{}\\{}", FALLBACK_USER_HOME_BASE_DIR, username))
                }
            } else {
                current_home
            }
        }
        None => PathBuf::from(format!("{}\\{}", FALLBACK_USER_HOME_BASE_DIR, username)),
    }
}

#[cfg(not(windows))]
fn user_home_dir(username: &str) -> PathBuf {
    PathBuf::from(format!("/home/{}", username))
}

/// Expand tilde with another user's home directory (~username/path)
fn expand_tilde_with_another_user_home(path: &str) -> PathBuf {
    // Find the separator (/ or \)
    match path.find(['/', '\\']) {
        None => {
            // Just ~username, no trailing path
            let username = &path[1..];  // Skip the ~
            user_home_dir(username)
        }
        Some(i) => {
            // ~username/rest/of/path
            let username = &path[1..i];
            let rest = &path[i + 1..];
            let mut home = user_home_dir(username);
            if !rest.is_empty() {
                home.push(normalize_slashes(rest));
            }
            home
        }
    }
}

/// Normalize any path format to Windows native path
///
/// # Examples
/// ```
/// use titan_bash::shell::path::normalize;
/// use std::path::PathBuf;
///
/// assert_eq!(normalize("C:/Users/test"), PathBuf::from("C:\\Users\\test"));
/// assert_eq!(normalize("/c/Users/test"), PathBuf::from("C:\\Users\\test"));
/// ```
pub fn normalize(path: &str) -> PathBuf {
    let path = path.trim();

    // Handle empty path
    if path.is_empty() {
        return PathBuf::from(".");
    }

    // Handle ~ (home directory)
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }

    // Handle ~/ or ~\ (current user's home)
    if path.starts_with("~/") || path.starts_with("~\\") {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let rest = &path[2..];
        return home.join(normalize_slashes(rest));
    }

    // Handle ~username (another user's home) - must start with ~ but not ~/
    if path.starts_with('~') {
        return expand_tilde_with_another_user_home(path);
    }

    // Handle /c/ or /C/ style paths (Git Bash format)
    if path.len() >= 3 {
        let chars: Vec<char> = path.chars().collect();
        if chars[0] == '/' && chars[1].is_ascii_alphabetic() && (chars[2] == '/' || chars.len() == 2) {
            let drive = chars[1].to_ascii_uppercase();
            let rest = if chars.len() > 2 { &path[2..] } else { "" };
            let normalized_rest = normalize_slashes(rest);
            return PathBuf::from(format!("{}:{}", drive, normalized_rest));
        }
    }

    // Handle regular paths - normalize slashes
    PathBuf::from(normalize_slashes(path))
}

/// Convert forward slashes to backslashes for Windows
/// Preserves UNC paths (\\server\share) and long path prefix (\\?\)
fn normalize_slashes(path: &str) -> String {
    // Check for UNC path (\\server\share) or long path prefix (\\?\)
    let (prefix, rest) = if path.starts_with("\\\\") {
        // UNC path or long path prefix - preserve the leading \\
        ("\\\\", &path[2..])
    } else if path.starts_with("//") {
        // Forward slash UNC path - convert to backslash UNC
        ("\\\\", &path[2..])
    } else {
        ("", path)
    };

    // Replace forward slashes with backslashes in the rest
    let mut result = rest.replace('/', "\\");

    // Remove duplicate backslashes (but only in the non-prefix part)
    while result.contains("\\\\") {
        result = result.replace("\\\\", "\\");
    }

    format!("{}{}", prefix, result)
}

/// Add long path prefix for Windows paths exceeding MAX_PATH
/// Windows MAX_PATH is 260 characters, we use 250 as threshold to be safe
pub fn add_long_path_prefix(path: &str) -> String {
    const LONG_PATH_THRESHOLD: usize = 250;

    // Only add prefix for absolute Windows paths without existing prefix
    if path.len() > LONG_PATH_THRESHOLD
        && !path.starts_with("\\\\?\\")
        && !path.starts_with("\\\\")
        && path.chars().nth(1) == Some(':')
    {
        format!("\\\\?\\{}", path)
    } else {
        path.to_string()
    }
}

/// Resolve path for filesystem operations (applies long-path prefix when needed)
pub fn resolve_fs(base: &Path, path: &str) -> PathBuf {
    let resolved = resolve(base, path);
    let resolved_str = resolved.to_string_lossy().to_string();
    PathBuf::from(add_long_path_prefix(&resolved_str))
}

/// Expand environment variables in path
/// Supports both Windows and bash syntax:
/// - %USERPROFILE% -> C:\Users\xxx (Windows)
/// - $HOME -> C:\Users\xxx (bash)
/// - ${HOME} -> C:\Users\xxx (bash)
pub fn expand_env(path: &str) -> String {
    let mut result = path.to_string();

    // 1. Handle ${VAR} syntax (bash with braces)
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start + 2..].find('}') {
            let var_name = &result[start + 2..start + 2 + end];
            if let Ok(value) = std::env::var(var_name) {
                result = result.replacen(&format!("${{{}}}", var_name), &value, 1);
            } else {
                // Can't expand, replace with empty string (bash behavior)
                result = result.replacen(&format!("${{{}}}", var_name), "", 1);
            }
        } else {
            break;
        }
    }

    // 2. Handle $VAR syntax (bash without braces)
    // Must be careful not to match ${ which was already handled
    let mut i = 0;
    while i < result.len() {
        if let Some(pos) = result[i..].find('$') {
            let abs_pos = i + pos;
            // Skip if it's ${ (already handled) or at end
            if abs_pos + 1 >= result.len() || result.chars().nth(abs_pos + 1) == Some('{') {
                i = abs_pos + 1;
                continue;
            }

            // Extract variable name (alphanumeric + underscore)
            let rest = &result[abs_pos + 1..];
            let var_len = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .count();

            if var_len > 0 {
                let var_name: String = rest.chars().take(var_len).collect();
                if let Ok(value) = std::env::var(&var_name) {
                    let pattern = format!("${}", var_name);
                    result = result.replacen(&pattern, &value, 1);
                    // Don't increment i, re-scan from same position in case value contains $
                    continue;
                } else {
                    // Can't expand, replace with empty string (bash behavior)
                    let pattern = format!("${}", var_name);
                    result = result.replacen(&pattern, "", 1);
                    continue;
                }
            }
            i = abs_pos + 1;
        } else {
            break;
        }
    }

    // 3. Handle %VAR% syntax (Windows)
    while let Some(start) = result.find('%') {
        if let Some(end) = result[start + 1..].find('%') {
            let var_name = &result[start + 1..start + 1 + end];
            if var_name.is_empty() {
                break;  // %% escape, skip
            }
            if let Ok(value) = std::env::var(var_name) {
                result = result.replacen(&format!("%{}%", var_name), &value, 1);
            } else {
                // Can't expand, skip this one
                break;
            }
        } else {
            break;
        }
    }

    result
}

/// Join path with current directory, handling relative paths
pub fn resolve(base: &Path, path: &str) -> PathBuf {
    let normalized = normalize(path);

    // If it's an absolute path, return it directly
    if normalized.is_absolute() {
        return normalized;
    }

    // Handle . and ..
    let mut result = base.to_path_buf();
    for component in normalized.components() {
        match component {
            std::path::Component::CurDir => {} // . - do nothing
            std::path::Component::ParentDir => {
                result.pop(); // .. - go up
            }
            std::path::Component::Normal(name) => {
                result.push(name);
            }
            _ => {}
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_forward_slashes() {
        assert_eq!(
            normalize("C:/Users/test"),
            PathBuf::from("C:\\Users\\test")
        );
    }

    #[test]
    fn test_git_bash_style() {
        assert_eq!(
            normalize("/c/Users/test"),
            PathBuf::from("C:\\Users\\test")
        );
        assert_eq!(
            normalize("/d/ocr/project"),
            PathBuf::from("D:\\ocr\\project")
        );
    }

    #[test]
    fn test_mixed_slashes() {
        assert_eq!(
            normalize("C:/Users\\test/folder"),
            PathBuf::from("C:\\Users\\test\\folder")
        );
    }

    #[test]
    fn test_home_directory() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(normalize("~"), home);
        assert_eq!(normalize("~/Documents"), home.join("Documents"));
    }

    #[test]
    fn test_other_user_home() {
        // Test ~username expansion
        let result = normalize("~testuser/Documents");
        // Should contain testuser in path
        assert!(result.to_string_lossy().contains("testuser"));
    }

    #[test]
    fn test_unc_path() {
        assert_eq!(
            normalize("\\\\server\\share\\folder"),
            PathBuf::from("\\\\server\\share\\folder")
        );
        // Forward slash UNC path
        assert_eq!(
            normalize("//server/share/folder"),
            PathBuf::from("\\\\server\\share\\folder")
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_reserved_names() {
        assert!(is_windows_reserved_name(Path::new("CON")));
        assert!(is_windows_reserved_name(Path::new("con")));  // case insensitive
        assert!(is_windows_reserved_name(Path::new("NUL")));
        assert!(is_windows_reserved_name(Path::new("COM1")));
        assert!(is_windows_reserved_name(Path::new("LPT9")));
        assert!(is_windows_reserved_name(Path::new("PRN.txt")));  // with extension
        assert!(is_windows_reserved_name(Path::new("AUX.log")));

        // Not reserved
        assert!(!is_windows_reserved_name(Path::new("regular.txt")));
        assert!(!is_windows_reserved_name(Path::new("CONSOLE")));
        assert!(!is_windows_reserved_name(Path::new("COM10")));  // only 1-9
    }
}
