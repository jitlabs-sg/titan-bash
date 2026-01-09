//! Built-in commands
//!
//! These are implemented directly in TITAN Bash for:
//! 1. Consistent path handling
//! 2. Better performance
//! 3. Cross-platform compatibility

use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::SystemTime;
use anyhow::{Result, Context};
use colored::Colorize;
use glob::glob;
use sha2::{Digest, Sha256};
use sha1::Sha1;

use super::Shell;
use super::path;
use super::parser::split_args;
use super::busybox;
use super::venv;
use crate::task::{TaskId, TaskStatus};

/// Builtins that affect shell state (must run in main process)
const STATE_BUILTINS: &[&str] = &[
    "cd", "export", "set", "alias", "unalias", "activate", "deactivate", "exit", "quit", "fg", "wait", "kill",
];

/// All builtin command names
const ALL_BUILTINS: &[&str] = &[
    "cd", "pwd", "ls", "dir", "cat", "type", "echo",
    "clear", "cls", "exit", "quit", "help", "jobs",
    "export", "set", "env", "printenv",
    "alias", "unalias", "which", "where", "mkdir", "rm",
    "del", "cp", "copy", "mv", "move", "touch", "history",
    "head", "tail", "whoami", "hostname",
    "md5sum", "sha1sum", "sha256sum", "sha512sum",
    "activate", "deactivate", "fg", "wait", "kill",
];

pub fn is_builtin(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ALL_BUILTINS.contains(&lower.as_str())
}

pub fn is_state_builtin(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    STATE_BUILTINS.contains(&lower.as_str())
}

/// Check if command contains shell operators that require cmd.exe handling
/// Returns true if the command should be passed to external executor
pub fn has_shell_operators(cmd: &str) -> bool {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let chars: Vec<char> = cmd.chars().collect();
    let len = chars.len();

    for i in 0..len {
        let ch = chars[i];

        match ch {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            _ if in_single_quote || in_double_quote => continue,
            '|' => return true,
            '>' => return true,
            '<' => return true,
            ';' => return true,
            '&' => {
                if i + 1 < len && chars[i + 1] == '&' {
                    return true;
                }
            }
            _ => {}
        }
    }

    false
}

/// Expand glob patterns in a path argument
fn expand_glob(cwd: &Path, pattern: &str) -> Vec<String> {
    if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
        return vec![pattern.to_string()];
    }

    let normalized = path::normalize(pattern);

    let abs_pattern = if normalized.is_absolute() {
        normalized.to_string_lossy().to_string()
    } else {
        cwd.join(normalized).to_string_lossy().to_string()
    };

    match glob(&abs_pattern) {
        Ok(paths) => {
            let matches: Vec<String> = paths
                .filter_map(|p| p.ok())
                .map(|p| p.to_string_lossy().to_string())
                .collect();

            if matches.is_empty() {
                vec![pattern.to_string()]
            } else {
                matches
            }
        }
        Err(_) => vec![pattern.to_string()],
    }
}

/// Try to execute a builtin command
/// Returns Some(exit_code) if it was a builtin, None if not
pub fn try_builtin(shell: &mut Shell, cmd: &str) -> Result<Option<i32>> {
    // If command contains shell operators, skip builtin and let cmd.exe handle it
    if has_shell_operators(cmd) {
        return Ok(None);
    }

    let args = split_args(cmd);
    if args.is_empty() {
        return Ok(None);
    }

    let command = args[0].to_lowercase();
    let rest: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    match command.as_str() {
        "cd" => {
            let code = builtin_cd(shell, &rest)?;
            Ok(Some(code))
        }
        "pwd" => {
            let code = builtin_pwd(shell)?;
            Ok(Some(code))
        }
        "ls" | "dir" => {
            let code = builtin_ls(shell, &rest)?;
            Ok(Some(code))
        }
        "cat" | "type" => {
            let code = builtin_cat(shell, &rest)?;
            Ok(Some(code))
        }
        "echo" => {
            let code = builtin_echo(&rest)?;
            Ok(Some(code))
        }
        "clear" | "cls" => {
            let code = builtin_clear()?;
            Ok(Some(code))
        }
        "exit" | "quit" => {
            shell.should_exit = true;
            Ok(Some(0))
        }
        "help" => {
            let code = builtin_help()?;
            Ok(Some(code))
        }
        "jobs" => {
            let code = builtin_jobs(shell)?;
            Ok(Some(code))
        }
        "export" | "set" => {
            let code = builtin_export(&rest)?;
            Ok(Some(code))
        }
        "env" | "printenv" => {
            let code = builtin_env(&rest)?;
            Ok(Some(code))
        }
        "alias" => {
            let code = builtin_alias(shell, &rest)?;
            Ok(Some(code))
        }
        "unalias" => {
            let code = builtin_unalias(shell, &rest)?;
            Ok(Some(code))
        }
        "which" | "where" => {
            let code = builtin_which(&rest)?;
            Ok(Some(code))
        }
        "mkdir" => {
            let code = builtin_mkdir(shell, &rest)?;
            Ok(Some(code))
        }
        "rm" | "del" => {
            let code = builtin_rm(shell, &rest)?;
            Ok(Some(code))
        }
        "cp" | "copy" => {
            let code = builtin_cp(shell, &rest)?;
            Ok(Some(code))
        }
        "mv" | "move" => {
            let code = builtin_mv(shell, &rest)?;
            Ok(Some(code))
        }
        "touch" => {
            let code = builtin_touch(shell, &rest)?;
            Ok(Some(code))
        }
        "history" => {
            let code = builtin_history(&rest)?;
            Ok(Some(code))
        }
        "head" => {
            let code = builtin_head(shell, &rest)?;
            Ok(Some(code))
        }
        "tail" => {
            let code = builtin_tail(shell, &rest)?;
            Ok(Some(code))
        }
        "whoami" => {
            let code = builtin_whoami()?;
            Ok(Some(code))
        }
        "hostname" => {
            let code = builtin_hostname()?;
            Ok(Some(code))
        }
        "md5sum" => {
            let code = builtin_checksum(HashKind::Md5, shell, &rest)?;
            Ok(Some(code))
        }
        "sha1sum" => {
            let code = builtin_checksum(HashKind::Sha1, shell, &rest)?;
            Ok(Some(code))
        }
        "sha256sum" => {
            let code = builtin_checksum(HashKind::Sha256, shell, &rest)?;
            Ok(Some(code))
        }
        "sha512sum" => {
            let code = builtin_checksum(HashKind::Sha512, shell, &rest)?;
            Ok(Some(code))
        }
        "fg" => {
            let code = builtin_fg(shell, &rest)?;
            Ok(Some(code))
        }
        "wait" => {
            let code = builtin_wait(shell, &rest)?;
            Ok(Some(code))
        }
        "kill" => {
            let code = builtin_kill(shell, &rest)?;
            Ok(Some(code))
        }
        _ => Ok(None),
    }
}

/// Run builtin with explicit stdin/stdout/stderr streams.
pub fn run_builtin_io(
    shell: &mut Shell,
    name: &str,
    args: &[String],
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<i32> {
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let lower = name.to_ascii_lowercase();

    match lower.as_str() {
        "cd" => builtin_cd(shell, &args_ref),
        "pwd" => builtin_pwd_impl(shell, stdout),
        "ls" | "dir" => builtin_ls_impl(shell, &args_ref, stdout, stderr),
        "cat" | "type" => builtin_cat_impl(shell, &args_ref, stdin, stdout, stderr),
        "echo" => builtin_echo_impl(&args_ref, stdout),
        "clear" | "cls" => builtin_clear_impl(stdout),
        "exit" | "quit" => {
            shell.should_exit = true;
            Ok(0)
        }
        "help" => builtin_help_impl(stdout),
        "jobs" => builtin_jobs_impl(shell, stdout),
        "export" | "set" => builtin_export_impl(&args_ref, stdout),
        "env" | "printenv" => builtin_env_impl(&args_ref, stdout),
        "alias" => builtin_alias_impl(shell, &args_ref, stdout),
        "unalias" => builtin_unalias(shell, &args_ref),
        "activate" => builtin_activate(shell, &args_ref),
        "deactivate" => builtin_deactivate(shell),
        "which" | "where" => builtin_which_impl(&args_ref, stdout),
        "mkdir" => builtin_mkdir(shell, &args_ref),
        "rm" | "del" => builtin_rm(shell, &args_ref),
        "cp" | "copy" => builtin_cp(shell, &args_ref),
        "mv" | "move" => builtin_mv(shell, &args_ref),
        "touch" => builtin_touch(shell, &args_ref),
        "history" => builtin_history_impl(&args_ref, stdout),
        "head" => builtin_head_impl(shell, &args_ref, stdin, stdout),
        "tail" => builtin_tail_impl(shell, &args_ref, stdin, stdout),
        "whoami" => builtin_whoami_impl(stdout),
        "hostname" => builtin_hostname_impl(stdout),
        "md5sum" => builtin_checksum_impl(HashKind::Md5, shell, &args_ref, stdin, stdout, stderr),
        "sha1sum" => builtin_checksum_impl(HashKind::Sha1, shell, &args_ref, stdin, stdout, stderr),
        "sha256sum" => builtin_checksum_impl(HashKind::Sha256, shell, &args_ref, stdin, stdout, stderr),
        "sha512sum" => builtin_checksum_impl(HashKind::Sha512, shell, &args_ref, stdin, stdout, stderr),
        "fg" => builtin_fg(shell, &args_ref),
        "wait" => builtin_wait(shell, &args_ref),
        "kill" => builtin_kill(shell, &args_ref),
        _ => Err(anyhow::anyhow!("Unknown builtin: {}", name)),
    }
}

/// Run builtin with captured output (stdout)
pub fn run_builtin_captured(
    shell: &mut Shell,
    name: &str,
    args: &[String],
    output: &mut dyn Write,
) -> Result<i32> {
    let mut stdin = BufReader::new(io::empty());
    let stderr_handle = io::stderr();
    let mut stderr = stderr_handle.lock();
    run_builtin_io(shell, name, args, &mut stdin, output, &mut stderr)
}

/// cd - change directory
fn builtin_cd(shell: &mut Shell, args: &[&str]) -> Result<i32> {
    let target = if args.is_empty() {
        // cd with no args goes to home
        dirs::home_dir().unwrap_or_else(|| shell.cwd.clone())
    } else {
        // Normalize and resolve the path
        let raw_path = args[0];
        let expanded = path::expand_env(raw_path);
        path::resolve_fs(&shell.cwd, &expanded)
    };

    // Check if directory exists
    if !target.is_dir() {
        anyhow::bail!("cd: {}: No such directory", target.display());
    }

    // Change directory
    env::set_current_dir(&target)?;
    shell.cwd = target;

    Ok(0)
}

/// pwd - print working directory
fn builtin_pwd_impl(shell: &Shell, out: &mut dyn Write) -> Result<i32> {
    writeln!(out, "{}", shell.cwd.display())?;
    Ok(0)
}

fn builtin_pwd(shell: &Shell) -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_pwd_impl(shell, &mut out)
}

/// ls - list directory
fn builtin_ls_impl(
    shell: &Shell,
    args: &[&str],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    // Parse options vs path arguments
    let mut show_all = false;
    let mut long_format = false;
    let mut target_path: Option<&str> = None;

    for arg in args {
        if arg.starts_with('-') {
            for ch in arg.chars().skip(1) {
                match ch {
                    'a' => show_all = true,
                    'l' => long_format = true,
                    _ => {}
                }
            }
        } else if target_path.is_none() {
            target_path = Some(arg);
        }
    }

    let target = match target_path {
        Some(p) => {
            let expanded = path::expand_env(p);
            let resolved = path::resolve_fs(&shell.cwd, &expanded);

            // Check for Windows reserved device names - provide helpful warning
            if path::is_windows_reserved_name(&resolved) {
                writeln!(err, "ls: warning: '{}' is a Windows reserved device name", p)?;
            }

            resolved
        }
        None => shell.cwd.clone(),
    };

    let entries = fs::read_dir(&target)
        .with_context(|| format!("ls: cannot access '{}'", target.display()))?;

    let mut items: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            if show_all {
                true
            } else {
                // Hide dotfiles by default
                !e.file_name().to_string_lossy().starts_with('.')
            }
        })
        .collect();

    // Sort by name
    items.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    if long_format {
        // Long format: one entry per line with details
        for entry in items {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if let Ok(meta) = entry.metadata() {
                let is_dir = meta.is_dir();
                let size = meta.len();
                let modified = meta.modified()
                    .map(|t| {
                        let datetime: chrono::DateTime<chrono::Local> = t.into();
                        datetime.format("%Y-%m-%d %H:%M").to_string()
                    })
                    .unwrap_or_else(|_| "????-??-?? ??:??".to_string());

                let type_char = if is_dir { "d" } else { "-" };
                let colored_name = if is_dir {
                    name_str.blue().bold().to_string()
                } else {
                    name_str.to_string()
                };

                writeln!(out, "{} {:>10} {} {}", type_char, size, modified, colored_name)?;
            }
        }
    } else {
        // Short format: multi-column layout
        let names: Vec<_> = items.iter().map(|e| {
            let name = e.file_name();
            let name_str = name.to_string_lossy().to_string();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (name_str, is_dir)
        }).collect();

        if names.is_empty() {
            return Ok(0);
        }

        // Get terminal width (default 80)
        let term_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        // Find max name length
        let max_len = names.iter().map(|(n, _)| n.len()).max().unwrap_or(10);
        let col_width = max_len + 2; // 2 spaces padding
        let num_cols = (term_width / col_width).max(1);

        // Print in columns
        for (i, (name, is_dir)) in names.iter().enumerate() {
            let formatted = if *is_dir {
                format!("{:<width$}", name.blue().bold(), width = col_width)
            } else {
                format!("{:<width$}", name, width = col_width)
            };
            write!(out, "{}", formatted)?;

            // Newline after last column or last item
            if (i + 1) % num_cols == 0 || i == names.len() - 1 {
                writeln!(out)?;
            }
        }
    }

    Ok(0)
}

fn builtin_ls(shell: &Shell, args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    builtin_ls_impl(shell, args, &mut out, &mut err)
}

/// cat - display file contents (streaming for large files)
fn builtin_cat_impl(
    shell: &Shell,
    args: &[&str],
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    if args.is_empty() {
        for line in stdin.lines() {
            writeln!(out, "{}", line?)?;
        }
        return Ok(0);
    }

    for arg in args {
        if arg.starts_with('-') {
            continue;
        }
        if *arg == "-" {
            for line in stdin.lines() {
                writeln!(out, "{}", line?)?;
            }
            continue;
        }

        let expanded = path::expand_env(arg);
        let paths = expand_glob(&shell.cwd, &expanded);

        for path_str in paths {
            let target = path::resolve_fs(&shell.cwd, &path_str);

            // Check for Windows reserved device names
            if path::is_windows_reserved_name(&target) {
                writeln!(
                    err,
                    "cat: warning: '{}' is a Windows reserved device name - reading from device",
                    path_str
                )?;
            }

            let file = File::open(&target)
                .with_context(|| format!("cat: {}: No such file", target.display()))?;

            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line.with_context(|| format!("cat: error reading {}", target.display()))?;
                writeln!(out, "{}", line)?;
            }
        }
    }

    Ok(0)
}

fn builtin_cat(shell: &Shell, args: &[&str]) -> Result<i32> {
    let mut stdin = BufReader::new(io::stdin());
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    builtin_cat_impl(shell, args, &mut stdin, &mut out, &mut err)
}

/// echo - print arguments
fn builtin_echo_impl(args: &[&str], out: &mut dyn Write) -> Result<i32> {
    let output = args.join(" ");
    // Expand environment variables
    let expanded = path::expand_env(&output);
    writeln!(out, "{}", expanded)?;
    Ok(0)
}

fn builtin_echo(args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_echo_impl(args, &mut out)
}

fn parse_head_tail_args<'a>(args: &'a [&'a str]) -> (usize, Vec<&'a str>) {
    let mut count = 10usize;
    let mut files = Vec::new();

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i];
        if arg == "--" {
            files.extend_from_slice(&args[i + 1..]);
            break;
        }
        if arg == "-n" {
            if i + 1 < args.len() {
                count = args[i + 1].parse().unwrap_or(count);
                i += 2;
                continue;
            }
        }
        if let Some(rest) = arg.strip_prefix("-n") {
            if !rest.is_empty() {
                count = rest.parse().unwrap_or(count);
                i += 1;
                continue;
            }
        }
        if arg.chars().all(|c| c.is_ascii_digit()) {
            count = arg.parse().unwrap_or(count);
            i += 1;
            continue;
        }
        files.push(arg);
        i += 1;
    }

    (count, files)
}

/// head - show first N lines
fn builtin_head_impl(
    shell: &Shell,
    args: &[&str],
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<i32> {
    let (count, files) = parse_head_tail_args(args);
    if files.is_empty() {
        for (idx, line) in stdin.lines().enumerate() {
            if idx >= count {
                break;
            }
            writeln!(out, "{}", line?)?;
        }
        return Ok(0);
    }
    for file in files {
        let expanded = path::expand_env(file);
        let target = path::resolve_fs(&shell.cwd, &expanded);
        let f = File::open(&target).with_context(|| format!("head: cannot open '{}'", target.display()))?;
        let reader = BufReader::new(f);
        for (idx, line) in reader.lines().enumerate() {
            if idx >= count {
                break;
            }
            writeln!(out, "{}", line.with_context(|| format!("head: error reading {}", target.display()))?)?;
        }
    }
    Ok(0)
}

fn builtin_head(shell: &Shell, args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let mut stdin = BufReader::new(io::empty());
    let mut out = stdout.lock();
    builtin_head_impl(shell, args, &mut stdin, &mut out)
}

/// tail - show last N lines (simple implementation)
fn builtin_tail_impl(
    shell: &Shell,
    args: &[&str],
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<i32> {
    use std::collections::VecDeque;

    let (count, files) = parse_head_tail_args(args);
    if files.is_empty() {
        let mut ring: VecDeque<String> = VecDeque::with_capacity(count.max(1));
        for line in stdin.lines() {
            let line = line?;
            if ring.len() == count {
                ring.pop_front();
            }
            ring.push_back(line);
        }
        for line in ring {
            writeln!(out, "{}", line)?;
        }
        return Ok(0);
    }
    for file in files {
        let expanded = path::expand_env(file);
        let target = path::resolve_fs(&shell.cwd, &expanded);
        let f = File::open(&target).with_context(|| format!("tail: cannot open '{}'", target.display()))?;
        let reader = BufReader::new(f);
        let mut ring: VecDeque<String> = VecDeque::with_capacity(count.max(1));
        for line in reader.lines() {
            let line = line?;
            if ring.len() == count {
                ring.pop_front();
            }
            ring.push_back(line);
        }
        for line in ring {
            writeln!(out, "{}", line)?;
        }
    }
    Ok(0)
}

fn builtin_tail(shell: &Shell, args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let mut stdin = BufReader::new(io::empty());
    let mut out = stdout.lock();
    builtin_tail_impl(shell, args, &mut stdin, &mut out)
}

fn builtin_whoami_impl(out: &mut dyn Write) -> Result<i32> {
    let user = env::var("USERNAME")
        .or_else(|_| env::var("USER"))
        .unwrap_or_else(|_| "unknown".to_string());
    writeln!(out, "{}", user)?;
    Ok(0)
}

fn builtin_whoami() -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_whoami_impl(&mut out)
}

fn builtin_hostname_impl(out: &mut dyn Write) -> Result<i32> {
    let host = env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    writeln!(out, "{}", host)?;
    Ok(0)
}

fn builtin_hostname() -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_hostname_impl(&mut out)
}

#[derive(Clone, Copy)]
enum HashKind {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashKind {
    fn name(&self) -> &'static str {
        match self {
            HashKind::Md5 => "md5sum",
            HashKind::Sha1 => "sha1sum",
            HashKind::Sha256 => "sha256sum",
            HashKind::Sha512 => "sha512sum",
        }
    }
}

/// checksum - compute hashes (file or stdin)
fn builtin_checksum_impl(
    kind: HashKind,
    shell: &Shell,
    args: &[&str],
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let mut exit_code = 0;

    let inputs: Vec<&str> = if args.is_empty() { vec!["-"] } else { args.iter().copied().filter(|a| *a != "--").collect() };

    for arg in inputs {
        if arg.starts_with('-') && arg != "-" {
            if arg == "--help" || arg == "-h" {
                writeln!(out, "Usage: {} [FILE...]", kind.name())?;
                writeln!(out, "  - (or no args) reads from stdin")?;
                return Ok(0);
            }
            continue;
        }

        if arg == "-" {
            let mut buf = [0u8; 64 * 1024];
            match kind {
                HashKind::Md5 => {
                    let mut ctx = md5::Context::new();
                    loop {
                        let n = stdin.read(&mut buf)?;
                        if n == 0 { break; }
                        ctx.consume(&buf[..n]);
                    }
                    writeln!(out, "{:x} *-", ctx.compute())?;
                }
                HashKind::Sha1 => {
                    let mut hasher = Sha1::new();
                    loop {
                        let n = stdin.read(&mut buf)?;
                        if n == 0 { break; }
                        hasher.update(&buf[..n]);
                    }
                    writeln!(out, "{:x} *-", hasher.finalize())?;
                }
                HashKind::Sha256 => {
                    let mut hasher = Sha256::new();
                    loop {
                        let n = stdin.read(&mut buf)?;
                        if n == 0 { break; }
                        hasher.update(&buf[..n]);
                    }
                    writeln!(out, "{:x} *-", hasher.finalize())?;
                }
                HashKind::Sha512 => {
                    let mut hasher = sha2::Sha512::new();
                    loop {
                        let n = stdin.read(&mut buf)?;
                        if n == 0 { break; }
                        hasher.update(&buf[..n]);
                    }
                    writeln!(out, "{:x} *-", hasher.finalize())?;
                }
            }
            continue;
        }

        let expanded = path::expand_env(arg);
        let paths = expand_glob(&shell.cwd, &expanded);

        for path_str in paths {
            let target = path::resolve_fs(&shell.cwd, &path_str);

            if path::is_windows_reserved_name(&target) {
                writeln!(err, "{}: warning: '{}' is a Windows reserved device name - hashing a device may block", kind.name(), path_str)?;
            }

            if target.is_dir() {
                writeln!(err, "{}: {}: Is a directory", kind.name(), path_str)?;
                exit_code = 1;
                continue;
            }

            let mut file = match File::open(&target) {
                Ok(f) => f,
                Err(e) => {
                    writeln!(err, "{}: {}: {}", kind.name(), path_str, e)?;
                    exit_code = 1;
                    continue;
                }
            };

            let mut buf = [0u8; 64 * 1024];
            let digest = match kind {
                HashKind::Md5 => {
                    let mut ctx = md5::Context::new();
                    loop {
                        let n = file.read(&mut buf)?;
                        if n == 0 { break; }
                        ctx.consume(&buf[..n]);
                    }
                    format!("{:x}", ctx.compute())
                }
                HashKind::Sha1 => {
                    let mut hasher = Sha1::new();
                    loop {
                        let n = file.read(&mut buf)?;
                        if n == 0 { break; }
                        hasher.update(&buf[..n]);
                    }
                    format!("{:x}", hasher.finalize())
                }
                HashKind::Sha256 => {
                    let mut hasher = Sha256::new();
                    loop {
                        let n = file.read(&mut buf)?;
                        if n == 0 { break; }
                        hasher.update(&buf[..n]);
                    }
                    format!("{:x}", hasher.finalize())
                }
                HashKind::Sha512 => {
                    let mut hasher = sha2::Sha512::new();
                    loop {
                        let n = file.read(&mut buf)?;
                        if n == 0 { break; }
                        hasher.update(&buf[..n]);
                    }
                    format!("{:x}", hasher.finalize())
                }
            };
            writeln!(out, "{}  {}", digest, path_str)?;
        }
    }

    Ok(exit_code)
}

fn builtin_checksum(kind: HashKind, shell: &Shell, args: &[&str]) -> Result<i32> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();
    builtin_checksum_impl(kind, shell, args, &mut stdin, &mut stdout, &mut stderr)
}

/// clear - clear screen
fn builtin_clear_impl(out: &mut dyn Write) -> Result<i32> {
    // ANSI escape codes work in Windows Terminal
    write!(out, "\x1B[2J\x1B[1;1H")?;
    out.flush()?;
    Ok(0)
}

fn builtin_clear() -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_clear_impl(&mut out)
}

/// help - show help
fn builtin_help_impl(out: &mut dyn Write) -> Result<i32> {
    writeln!(out, "{}", "TITAN Bash - Modern shell for Windows".bold())?;
    writeln!(out)?;
    writeln!(out, "Built-in commands:")?;
    writeln!(out, "  {}       Change directory (supports all path formats)", "cd".green())?;
    writeln!(out, "  {}      Print working directory", "pwd".green())?;
    writeln!(out, "  {}       List directory contents (-l, -a)", "ls".green())?;
    writeln!(out, "  {}      Display file contents", "cat".green())?;
    writeln!(out, "  {}     Print text", "echo".green())?;
    writeln!(out, "  {}    Clear screen", "clear".green())?;
    writeln!(out, "  {}    Define or show aliases", "alias".green())?;
    writeln!(out, "  {}  Remove aliases", "unalias".green())?;
    writeln!(out, "  {}  Activate python venv in this shell", "activate".green())?;
    writeln!(out, "  {}  Deactivate python venv", "deactivate".green())?;
    writeln!(out, "  {}     Exit shell", "exit".green())?;
    writeln!(out, "  {}     Show background jobs", "jobs".green())?;
    writeln!(out, "  {}        Bring job to foreground", "fg".green())?;
    writeln!(out, "  {}      Wait for background job(s)", "wait".green())?;
    writeln!(out, "  {}      Kill background job", "kill".green())?;
    writeln!(out, "  {}   Set environment variable", "export".green())?;
    writeln!(out, "  {} / {}    Show environment variables", "env".green(), "printenv".green())?;
    writeln!(out, "  {}    Locate a command", "which".green())?;
    writeln!(out, "  {}    Create directory", "mkdir".green())?;
    writeln!(out, "  {}       Remove file/directory", "rm".green())?;
    writeln!(out, "  {}       Copy file", "cp".green())?;
    writeln!(out, "  {}       Move/rename file", "mv".green())?;
    writeln!(out, "  {}    Create file or update timestamp", "touch".green())?;
    writeln!(out, "  {}  Show command history", "history".green())?;
    writeln!(out, "  {}        Show first lines of file", "head".green())?;
    writeln!(out, "  {}         Show last lines of file", "tail".green())?;
    writeln!(out, "  {}          Print current user", "whoami".green())?;
    writeln!(out, "  {}       Print machine name", "hostname".green())?;
    writeln!(out, "  {}     Compute MD5 hashes", "md5sum".green())?;
    writeln!(out, "  {}     Compute SHA-1 hashes", "sha1sum".green())?;
    writeln!(out, "  {}     Compute SHA-256 hashes", "sha256sum".green())?;
    writeln!(out, "  {}     Compute SHA-512 hashes", "sha512sum".green())?;
    writeln!(out)?;
    writeln!(out, "Path formats (all work!):")?;
    writeln!(out, "  C:\\Users\\xxx")?;
    writeln!(out, "  C:/Users/xxx")?;
    writeln!(out, "  /c/Users/xxx")?;
    writeln!(out, "  ~/Documents")?;
    writeln!(out, "  ~username/Documents")?;
    writeln!(out)?;
    writeln!(out, "Background jobs:")?;
    writeln!(out, "  command &     Run in background")?;
    writeln!(out, "  jobs          List jobs")?;
    writeln!(out, "  fg [id]       Wait for a job and remove it")?;
    writeln!(out, "  wait [id..]   Wait for job(s)")?;
    writeln!(out, "  kill <id>     Terminate a job (taskkill)")?;
    Ok(0)
}

fn builtin_activate(shell: &mut Shell, args: &[&str]) -> Result<i32> {
    let venv_dir = if let Some(arg) = args.first() {
        // Support both: `activate venv` and `activate venv\\Scripts\\activate`.
        if let Some(dir) = venv::try_extract_venv_dir(&shell.cwd, arg) {
            dir
        } else {
            let expanded = path::expand_env(arg);
            path::resolve(&shell.cwd, &expanded)
        }
    } else {
        venv::find_default_venv_dir(&shell.cwd)
            .ok_or_else(|| anyhow::anyhow!("activate: no venv found (try: activate .venv or activate venv)"))?
    };

    venv::activate(shell, &venv_dir)?;
    Ok(0)
}

fn builtin_deactivate(shell: &mut Shell) -> Result<i32> {
    venv::deactivate(shell)?;
    Ok(0)
}

fn builtin_help() -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_help_impl(&mut out)
}

/// jobs - list background jobs
fn builtin_jobs_impl(shell: &Shell, out: &mut dyn Write) -> Result<i32> {
    let jobs = shell.tasks.list();
    if jobs.is_empty() {
        writeln!(out, "No background jobs")?;
    } else {
        for (id, status, cmd) in jobs {
            writeln!(out, "[{}] {} {}", id, status, cmd)?;
        }
    }
    Ok(0)
}

fn builtin_jobs(shell: &Shell) -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_jobs_impl(shell, &mut out)
}

fn parse_job_id(arg: &str) -> Result<TaskId> {
    arg.parse::<TaskId>()
        .with_context(|| format!("invalid job id: {}", arg))
}

fn last_running_job_id(shell: &Shell) -> Option<TaskId> {
    let mut last: Option<TaskId> = None;
    for (id, _, _) in shell.tasks.list() {
        if matches!(shell.tasks.status(id), Some(TaskStatus::Running)) {
            last = Some(id);
        }
    }
    last
}

/// fg - bring a job to foreground (best-effort: just waits for it)
fn builtin_fg(shell: &mut Shell, args: &[&str]) -> Result<i32> {
    let id = if args.is_empty() {
        last_running_job_id(shell).ok_or_else(|| anyhow::anyhow!("fg: no jobs"))?
    } else {
        parse_job_id(args[0])?
    };

    let status = shell
        .tasks
        .wait_and_remove(id)
        .ok_or_else(|| anyhow::anyhow!("fg: {}: no such job", id))?;

    match status {
        TaskStatus::Completed(code) => Ok(code),
        TaskStatus::Failed(msg) => anyhow::bail!("fg: {}", msg),
        TaskStatus::Running => Ok(0),
    }
}

/// wait - wait for background job(s)
fn builtin_wait(shell: &mut Shell, args: &[&str]) -> Result<i32> {
    let mut ids: Vec<TaskId> = if args.is_empty() {
        shell.tasks.list().into_iter().map(|(id, _, _)| id).collect()
    } else {
        args.iter().map(|a| parse_job_id(a)).collect::<Result<Vec<_>>>()?
    };

    ids.sort_unstable();
    ids.dedup();

    let mut last_code = 0;
    for id in ids {
        let status = shell
            .tasks
            .wait_and_remove(id)
            .ok_or_else(|| anyhow::anyhow!("wait: {}: no such job", id))?;
        match status {
            TaskStatus::Completed(code) => last_code = code,
            TaskStatus::Failed(msg) => anyhow::bail!("wait: {}", msg),
            TaskStatus::Running => {}
        }
    }

    Ok(last_code)
}

/// kill - terminate a background job (Windows: taskkill /T)
fn builtin_kill(shell: &mut Shell, args: &[&str]) -> Result<i32> {
    if args.is_empty() {
        anyhow::bail!("kill: missing job id");
    }
    let id = parse_job_id(args[0])?;
    shell.tasks.kill(id)?;
    Ok(0)
}

fn escape_single_quotes(value: &str) -> String {
    value.replace('\'', r#"'\''"#)
}

/// alias - define or show aliases
fn builtin_alias_impl(shell: &mut Shell, args: &[&str], out: &mut dyn Write) -> Result<i32> {
    if args.is_empty() {
        let mut keys: Vec<&String> = shell.aliases.keys().collect();
        keys.sort();
        for k in keys {
            let v = shell.aliases.get(k).map(|s| s.as_str()).unwrap_or_default();
            writeln!(out, "alias {}='{}'", k, escape_single_quotes(v))?;
        }
        return Ok(0);
    }

    for arg in args {
        if let Some((name, value)) = arg.split_once('=') {
            if name.is_empty() {
                anyhow::bail!("alias: invalid name");
            }
            shell.aliases.insert(name.to_string(), value.to_string());
        } else {
            let Some(value) = shell.aliases.get(*arg) else {
                anyhow::bail!("alias: {}: not found", arg);
            };
            writeln!(out, "alias {}='{}'", arg, escape_single_quotes(value))?;
        }
    }

    Ok(0)
}

fn builtin_alias(shell: &mut Shell, args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_alias_impl(shell, args, &mut out)
}

/// unalias - remove aliases
fn builtin_unalias(shell: &mut Shell, args: &[&str]) -> Result<i32> {
    if args.is_empty() {
        anyhow::bail!("unalias: missing operand");
    }

    if args.iter().any(|a| *a == "-a") {
        shell.aliases.clear();
        return Ok(0);
    }

    for name in args {
        if shell.aliases.remove(*name).is_none() {
            anyhow::bail!("unalias: {}: not found", name);
        }
    }

    Ok(0)
}

/// export - set environment variable
fn builtin_export_impl(args: &[&str], out: &mut dyn Write) -> Result<i32> {
    if args.is_empty() {
        // Show all environment variables
        for (key, value) in env::vars() {
            writeln!(out, "{}={}", key, value)?;
        }
        return Ok(0);
    }

    for arg in args {
        if let Some((key, value)) = arg.split_once('=') {
            // SAFETY: We're a shell, setting env vars is expected behavior
            unsafe { env::set_var(key, value); }
        } else {
            // Just the name, show value
            if let Ok(value) = env::var(arg) {
                writeln!(out, "{}={}", arg, value)?;
            }
        }
    }

    Ok(0)
}

fn builtin_export(args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_export_impl(args, &mut out)
}

/// env / printenv - show environment variables
fn builtin_env_impl(args: &[&str], out: &mut dyn Write) -> Result<i32> {
    if args.is_empty() {
        for (key, value) in env::vars() {
            writeln!(out, "{}={}", key, value)?;
        }
        return Ok(0);
    }

    for key in args {
        if let Some(val) = env::var_os(key) {
            writeln!(out, "{}", val.to_string_lossy())?;
        }
    }
    Ok(0)
}

fn builtin_env(args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_env_impl(args, &mut out)
}

/// which - locate command
fn builtin_which_impl(args: &[&str], out: &mut dyn Write) -> Result<i32> {
    let mut all_found = true;
    for name in args {
        match which::which(name) {
            Ok(path) => writeln!(out, "{}", path.display())?,
            Err(_) => {
                if !busybox::looks_like_path(name) {
                    let applet = busybox::normalize_applet_name(name);
                    if busybox::has_applet(&applet) {
                        if let Some(bb) = busybox::get() {
                            writeln!(out, "{} (busybox applet: {})", bb.path.display(), applet)?;
                            continue;
                        }
                    }
                }

                all_found = false;
                writeln!(out, "{}: not found", name)?;
            }
        }
    }
    Ok(if all_found { 0 } else { 1 })
}

fn builtin_which(args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_which_impl(args, &mut out)
}

/// mkdir - create directory
fn builtin_mkdir(shell: &Shell, args: &[&str]) -> Result<i32> {
    let create_parents = args.iter().any(|a| *a == "-p");

    for arg in args.iter().filter(|a| !a.starts_with('-')) {
        let expanded = path::expand_env(arg);
        let target = path::resolve_fs(&shell.cwd, &expanded);

        // Check for Windows reserved device names
        if path::is_windows_reserved_name(&target) {
            anyhow::bail!("mkdir: {}", path::reserved_name_error(arg));
        }

        if create_parents {
            fs::create_dir_all(&target)?;
        } else {
            fs::create_dir(&target)?;
        }
    }

    Ok(0)
}

/// rm - remove file/directory
fn builtin_rm(shell: &Shell, args: &[&str]) -> Result<i32> {
    let recursive = args.iter().any(|a| *a == "-r" || *a == "-rf");
    let force = args.iter().any(|a| *a == "-f" || *a == "-rf");

    for arg in args.iter().filter(|a| !a.starts_with('-')) {
        let expanded = path::expand_env(arg);
        let target = path::resolve_fs(&shell.cwd, &expanded);

        if target.is_dir() {
            if recursive {
                fs::remove_dir_all(&target)?;
            } else {
                if !force {
                    anyhow::bail!("rm: {}: is a directory (use -r)", target.display());
                }
            }
        } else if target.exists() {
            fs::remove_file(&target)?;
        } else if !force {
            anyhow::bail!("rm: {}: No such file or directory", target.display());
        }
    }

    Ok(0)
}

/// cp - copy file
fn builtin_cp(shell: &Shell, args: &[&str]) -> Result<i32> {
    if args.len() < 2 {
        anyhow::bail!("cp: missing destination");
    }

    let recursive = args.iter().any(|a| *a == "-r");
    let paths: Vec<_> = args.iter().filter(|a| !a.starts_with('-')).collect();

    if paths.len() < 2 {
        anyhow::bail!("cp: missing destination");
    }

    let dest = path::resolve_fs(&shell.cwd, &path::expand_env(paths[paths.len() - 1]));

    // Check destination for reserved names
    if path::is_windows_reserved_name(&dest) {
        anyhow::bail!("cp: {}", path::reserved_name_error(paths[paths.len() - 1]));
    }

    for src_arg in &paths[..paths.len() - 1] {
        let src = path::resolve_fs(&shell.cwd, &path::expand_env(src_arg));

        if src.is_dir() {
            if recursive {
                copy_dir_all(&src, &dest)?;
            } else {
                anyhow::bail!("cp: {}: is a directory (use -r)", src.display());
            }
        } else {
            let dest_path = if dest.is_dir() {
                dest.join(src.file_name().ok_or_else(|| anyhow::anyhow!("cannot get filename"))?)
            } else {
                dest.clone()
            };
            fs::copy(&src, &dest_path)?;
        }
    }

    Ok(0)
}

/// Helper: copy directory recursively
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

/// mv - move/rename
fn builtin_mv(shell: &Shell, args: &[&str]) -> Result<i32> {
    if args.len() < 2 {
        anyhow::bail!("mv: missing destination");
    }

    let paths: Vec<_> = args.iter().filter(|a| !a.starts_with('-')).collect();

    if paths.len() < 2 {
        anyhow::bail!("mv: missing destination");
    }

    let dest = path::resolve_fs(&shell.cwd, &path::expand_env(paths[paths.len() - 1]));

    // Check destination for reserved names
    if path::is_windows_reserved_name(&dest) {
        anyhow::bail!("mv: {}", path::reserved_name_error(paths[paths.len() - 1]));
    }

    for src_arg in &paths[..paths.len() - 1] {
        let src = path::resolve_fs(&shell.cwd, &path::expand_env(src_arg));
        let dest_path = if dest.is_dir() {
            dest.join(src.file_name().ok_or_else(|| anyhow::anyhow!("cannot get filename"))?)
        } else {
            dest.clone()
        };
        fs::rename(&src, &dest_path)?;
    }

    Ok(0)
}

/// touch - create empty file or update file timestamp
fn builtin_touch(shell: &Shell, args: &[&str]) -> Result<i32> {
    if args.is_empty() {
        anyhow::bail!("touch: missing file operand");
    }

    for arg in args.iter().filter(|a| !a.starts_with('-')) {
        let expanded = path::expand_env(arg);
        let target = path::resolve_fs(&shell.cwd, &expanded);

        // Check for Windows reserved device names
        if path::is_windows_reserved_name(&target) {
            anyhow::bail!("touch: {}", path::reserved_name_error(arg));
        }

        if target.exists() {
            // Update the file's modification time
            // On Windows, we can use filetime crate or simply open and close the file
            // For simplicity, we'll use set_modified with current time
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .with_context(|| format!("touch: cannot touch '{}': Permission denied", target.display()))?;
            file.set_modified(SystemTime::now())?;
        } else {
            // Create empty file
            fs::File::create(&target)
                .with_context(|| format!("touch: cannot touch '{}'", target.display()))?;
        }
    }

    Ok(0)
}

/// history - show command history
fn builtin_history_impl(args: &[&str], out: &mut dyn Write) -> Result<i32> {
    // History is stored in ~/.titanbash_history (fallback: ~/.titan_history)
    let history_path = dirs::home_dir()
        .map(|h| {
            let preferred = h.join(".titanbash_history");
            if preferred.exists() {
                return preferred;
            }
            h.join(".titan_history")
        })
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;

    if !history_path.exists() {
        writeln!(out, "No history yet")?;
        return Ok(0);
    }

    let content = fs::read_to_string(&history_path)
        .with_context(|| format!("Cannot read history file: {}", history_path.display()))?;

    let lines: Vec<&str> = content.lines().collect();

    // Parse optional -n argument to limit entries
    let limit = args.iter()
        .find(|a| a.starts_with("-"))
        .and_then(|a| a.trim_start_matches('-').parse::<usize>().ok())
        .unwrap_or(lines.len());

    let start = if lines.len() > limit { lines.len() - limit } else { 0 };

    for (i, line) in lines.iter().enumerate().skip(start) {
        writeln!(out, "{:>5}  {}", i + 1, line)?;
    }

    Ok(0)
}

fn builtin_history(args: &[&str]) -> Result<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    builtin_history_impl(args, &mut out)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_builtin_case_insensitive() {
        assert!(is_builtin("ls"));
        assert!(is_builtin("LS"));
        assert!(is_builtin("cD"));
        assert!(!is_builtin("definitely_not_a_builtin"));
    }

    #[test]
    fn test_is_state_builtin() {
        assert!(is_state_builtin("cd"));
        assert!(is_state_builtin("EXPORT"));
        assert!(is_state_builtin("quit"));
        assert!(!is_state_builtin("pwd"));
        assert!(!is_state_builtin("ls"));
    }

    #[test]
    fn test_has_shell_operators_pipe() {
        assert!(has_shell_operators("ls | grep foo"));
        assert!(has_shell_operators("cat file.txt | head"));
    }

    #[test]
    fn test_has_shell_operators_redirect() {
        assert!(has_shell_operators("echo hi > file.txt"));
        assert!(has_shell_operators("echo hi >> file.txt"));
        assert!(has_shell_operators("cat < input.txt"));
    }

    #[test]
    fn test_has_shell_operators_and() {
        assert!(has_shell_operators("cd .. && dir"));
        assert!(has_shell_operators("mkdir foo && cd foo"));
    }

    #[test]
    fn test_has_shell_operators_semicolon() {
        assert!(has_shell_operators("echo a; echo b"));
    }

    #[test]
    fn test_has_shell_operators_quoted() {
        // Operators inside quotes should NOT trigger
        assert!(!has_shell_operators("echo 'hello | world'"));
        assert!(!has_shell_operators(r#"echo "hello > world""#));
        assert!(!has_shell_operators("echo 'a && b'"));
    }

    #[test]
    fn test_has_shell_operators_simple() {
        assert!(!has_shell_operators("ls -la"));
        assert!(!has_shell_operators("cd /tmp"));
        assert!(!has_shell_operators("echo hello"));
    }

    #[test]
    fn test_has_shell_operators_background() {
        // Single & at end is background, not &&
        assert!(!has_shell_operators("sleep 10 &"));
        // But && should trigger
        assert!(has_shell_operators("echo a && echo b"));
    }

    #[test]
    fn test_alias_set_get_unalias() {
        let mut shell = Shell::new().unwrap();

        let mut out = Vec::<u8>::new();
        builtin_alias_impl(&mut shell, &["ll=ls -la"], &mut out).unwrap();

        let mut out = Vec::<u8>::new();
        builtin_alias_impl(&mut shell, &["ll"], &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("alias ll='ls -la'"));

        builtin_unalias(&mut shell, &["ll"]).unwrap();

        let mut out = Vec::<u8>::new();
        assert!(builtin_alias_impl(&mut shell, &["ll"], &mut out).is_err());
    }

    #[test]
    fn test_unalias_all() {
        let mut shell = Shell::new().unwrap();

        let mut out = Vec::<u8>::new();
        builtin_alias_impl(&mut shell, &["a=echo a"], &mut out).unwrap();
        builtin_alias_impl(&mut shell, &["b=echo b"], &mut out).unwrap();

        builtin_unalias(&mut shell, &["-a"]).unwrap();

        let mut out = Vec::<u8>::new();
        builtin_alias_impl(&mut shell, &[], &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().trim().is_empty());
    }

    #[test]
    fn test_sha256sum_file() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let shell = Shell::new().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "titanbash_sha256sum_test_{}.txt",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        fs::write(&tmp, b"hello").unwrap();
        let path_str = tmp.to_string_lossy().to_string();

        let args = [path_str.as_str()];
        let mut stdin = BufReader::new(io::empty());
        let mut out = Vec::<u8>::new();
        let mut err = Vec::<u8>::new();
        let code = builtin_checksum_impl(HashKind::Sha256, &shell, &args, &mut stdin, &mut out, &mut err).unwrap();
        assert_eq!(code, 0);

        let stdout = String::from_utf8(out).unwrap();
        assert!(err.is_empty());
        assert!(stdout.contains("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"));
        assert!(stdout.contains(&path_str));

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_md5sum_file() {
        let shell = Shell::new().unwrap();
        let tmp = std::env::temp_dir().join("titanbash_md5sum_test.txt");
        fs::write(&tmp, b"hello").unwrap();
        let path_str = tmp.to_string_lossy().to_string();

        let args = [path_str.as_str()];
        let mut stdin = BufReader::new(io::empty());
        let mut out = Vec::<u8>::new();
        let mut err = Vec::<u8>::new();
        let code = builtin_checksum_impl(HashKind::Md5, &shell, &args, &mut stdin, &mut out, &mut err).unwrap();
        assert_eq!(code, 0);
        let stdout = String::from_utf8(out).unwrap();
        assert!(err.is_empty());
        assert!(stdout.contains("5d41402abc4b2a76b9719d911017c592"));

        let _ = fs::remove_file(&tmp);
    }
}
