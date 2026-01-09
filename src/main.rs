//! TITAN Bash - Modern shell for Windows
//!
//! Usage:
//!   titanbash                  Interactive shell
//!   titanbash -c "command"     Execute single command
//!   titanbash script.titan     Execute script file

use std::env;
use std::fs;
use std::io::{BufRead, Write};
use std::process::Command;
use anyhow::Result;
use colored::Colorize;

use titan_bash::Shell;
use titan_bash::shell::input::{CrosstermInput, InputResult, normalize_pasted_lines, strip_prompt_prefix};
use titan_bash::shell::path as shell_path;
use titan_bash::shell::busybox;

#[cfg(windows)]
mod ctrlc {
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    };

    static CTRL_SEEN: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                CTRL_SEEN.store(true, Ordering::SeqCst);
                1
            }
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                titan_bash::task::kill_registered_pids_best_effort();
                0
            }
            _ => 0,
        }
    }

    pub fn install() {
        unsafe {
            // Install a handler so Ctrl+C doesn't terminate titanbash while waiting on child processes.
            let _ = SetConsoleCtrlHandler(Some(handler), 1);
        }
    }

    pub fn take() -> bool {
        CTRL_SEEN.swap(false, Ordering::SeqCst)
    }
}

fn load_titanbashrc(shell: &mut Shell) {
    let Some(home) = dirs::home_dir() else {
        return;
    };

    let preferred = home.join(".titanbashrc");
    let legacy = home.join(".titanrc");
    let path = if preferred.exists() { preferred } else { legacy };

    let Ok(content) = fs::read_to_string(&path) else {
        return;
    };

    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Err(e) = shell.execute(line) {
            eprintln!("titanbash: {}:{}: {}", path.display(), idx + 1, e);
        }
        if shell.should_exit {
            break;
        }
    }
}

/// Ensure we have a console window (for double-click launch)
#[cfg(windows)]
fn ensure_console() {
    use windows_sys::Win32::System::Console::{AllocConsole, GetConsoleWindow};
    unsafe {
        if GetConsoleWindow().is_null() {
            AllocConsole();
        }
    }
}

#[cfg(not(windows))]
fn ensure_console() {}

fn main() -> Result<()> {
    // Ensure we have a console (allows double-click to work)
    ensure_console();
    #[cfg(windows)]
    ctrlc::install();
    titan_bash::task::init_kill_on_close_job_best_effort();
    // If a bundled BusyBox is present, prepend its directory to the process PATH so
    // child process resolution matches interactive expectations.
    busybox::prepend_busybox_dir_to_path();

    // Parse command line args
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        match args[1].as_str() {
            "-c" => {
                // Execute single command
                if args.len() < 3 {
                    eprintln!("titanbash: -c requires an argument");
                    std::process::exit(1);
                }
                let cmd = args[2..].join(" ");
                let code = execute_command(&cmd)?;
                std::process::exit(code);
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-v" | "--version" => {
                println!("TITAN Bash v{}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            path if !path.starts_with('-') => {
                // Execute script file
                let script_args = args.iter().skip(2).cloned().collect::<Vec<_>>();
                let code = execute_script(path, &script_args)?;
                std::process::exit(code);
            }
            _ => {
                eprintln!("titanbash: unknown option: {}", args[1]);
                std::process::exit(1);
            }
        }
    }

    // Interactive mode
    let code = run_repl()?;
    std::process::exit(code);
}

fn print_help() {
    println!("{}", "TITAN Bash - Modern shell for Windows".bold());
    println!();
    println!("Usage:");
    println!("  titanbash                  Start interactive shell");
    println!("  titanbash -c \"command\"     Execute single command");
    println!("  titanbash script.titan     Execute script file");
    println!("  titanbash -h, --help       Show this help");
    println!("  titanbash -v, --version    Show version");
    println!();
    println!("Features:");
    println!("  - Path normalization: C:\\, C:/, /c/ all work");
    println!("  - Tab completion: commands and paths");
    println!("  - Multi-line input: quotes and backslash continuation");
    println!("  - Background jobs: command &");
    println!();
    println!("Type 'help' in the shell for built-in commands.");
}

fn execute_command(cmd: &str) -> Result<i32> {
    let mut shell = Shell::new()?;
    load_titanbashrc(&mut shell);
    match shell.execute(cmd) {
        Ok(()) => Ok(shell.last_status),
        Err(e) => {
            eprintln!("{}: {}", "error".red(), e);
            Ok(1)
        }
    }
}

fn execute_script(path: &str, script_args: &[String]) -> Result<i32> {
    let cwd = env::current_dir()?;
    let resolved = shell_path::resolve_fs(&cwd, path);
    let lower = resolved.to_string_lossy().to_ascii_lowercase();

    // Windows script types should be executed by their native hosts.
    if lower.ends_with(".ps1") {
        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&resolved)
            .args(script_args)
            .current_dir(&cwd)
            .spawn()?
            .wait()?;
        return Ok(status.code().unwrap_or(-1));
    }

    if lower.ends_with(".bat") || lower.ends_with(".cmd") {
        let status = Command::new("cmd")
            .args(["/C"])
            .arg(&resolved)
            .args(script_args)
            .current_dir(&cwd)
            .spawn()?
            .wait()?;
        return Ok(status.code().unwrap_or(-1));
    }

    // Treat everything else as a titanbash script file (line-based).
    let content = fs::read_to_string(&resolved)?;
    let mut shell = Shell::new()?;
    load_titanbashrc(&mut shell);

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Err(e) = shell.execute(line) {
            shell.last_status = 1;
            eprintln!("titanbash: {}: {}", line, e);
        }
        if shell.should_exit {
            break;
        }
    }

    Ok(shell.last_status)
}

/// Print fancy box banner (from CLI_TUI_DEEP_DIVE_ANALYSIS.md Section 6.3)
fn print_banner() {
    let version = env!("CARGO_PKG_VERSION");

    // Box characters (Unicode rounded corners)
    let tl = '\u{256D}'; // top-left
    let tr = '\u{256E}'; // top-right
    let bl = '\u{2570}'; // bottom-left
    let br = '\u{256F}'; // bottom-right
    let h = '\u{2500}';  // horizontal
    let v = '\u{2502}';  // vertical

    let content_width = 44;
    let title = format!(" TITAN Bash v{} ", version);
    let title_pad = (content_width - title.len()) / 2;

    // Top border with title
    print!("{}", format!("{}", tl).bright_black());
    print!("{}", h.to_string().repeat(title_pad).bright_black());
    print!("{}", title.bold().cyan());
    print!("{}", h.to_string().repeat(content_width - title_pad - title.len()).bright_black());
    println!("{}", format!("{}", tr).bright_black());

    // Content line
    let info = "Modern shell for Windows";
    let info_pad = (content_width - info.len()) / 2;
    print!("{}", format!("{}", v).bright_black());
    print!("{}", " ".repeat(info_pad));
    print!("{}", info.white());
    print!("{}", " ".repeat(content_width - info_pad - info.len()));
    println!("{}", format!("{}", v).bright_black());

    // Bottom border
    print!("{}", format!("{}", bl).bright_black());
    print!("{}", h.to_string().repeat(content_width).bright_black());
    println!("{}", format!("{}", br).bright_black());

    // Hints
    println!("  {} for help, {} to exit, {} for completion",
        "help".green(),
        "exit".green(),
        "Tab".yellow()
    );
    println!();
}

/// Check if input is incomplete and needs continuation
fn is_incomplete(input: &str) -> bool {
    let trimmed = input.trim_end();
    if trimmed.ends_with('\\') {
        return true;
    }
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_char = '\0';
    for c in input.chars() {
        match c {
            '\'' if !in_double && prev_char != '\\' => in_single = !in_single,
            '"' if !in_single && prev_char != '\\' => in_double = !in_double,
            _ => {}
        }
        prev_char = c;
    }
    in_single || in_double
}

fn escape_history_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape_history_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn run_repl() -> Result<i32> {
    print_banner();

    let mut shell = Shell::new()?;
    load_titanbashrc(&mut shell);
    let mut input = CrosstermInput::new(shell.cwd.clone());

    // Load history
    let history_path = dirs::home_dir()
        .map(|h| {
            let preferred = h.join(".titanbash_history");
            let legacy = h.join(".titan_history");
            if preferred.exists() {
                return preferred;
            }
            if legacy.exists() {
                if fs::copy(&legacy, &preferred).is_ok() {
                    return preferred;
                }
                return legacy;
            }
            preferred
        })
        .unwrap_or_else(|| ".titanbash_history".into());
    
    if let Ok(file) = fs::File::open(&history_path) {
        let reader = std::io::BufReader::new(file);
        let mut entries: Vec<String> = reader
            .lines()
            .filter_map(|l| l.ok())
            .map(|l| unescape_history_line(&l))
            .collect();
        const MAX_HISTORY: usize = 5000;
        if entries.len() > MAX_HISTORY {
            entries = entries.split_off(entries.len() - MAX_HISTORY);
        }
        input.load_history(entries);
    }

    let mut history_writer = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)
    {
        Ok(f) => Some(std::io::BufWriter::new(f)),
        Err(_) => None,
    };
    let mut last_written = input.history_entries().last().cloned();

    // For multi-line input (quotes/backslash continuation)
    let mut input_buffer = String::new();

    loop {
        // Check for completed background jobs
        let completed = shell.tasks.check_completed();
        for (id, code, cmd) in completed {
            println!("\n[{}] Done ({}) {}", id, code, cmd);
        }

        // Update cwd for completion
        input.set_cwd(shell.cwd.clone());

        // Build prompt
        let prompt = if input_buffer.is_empty() {
            shell.prompt()
        } else {
            "> ".to_string()
        };

        match input.read_line(&prompt) {
            Ok(InputResult::Line(line)) => {
                let (line, _stripped) = strip_prompt_prefix(&line);
                // Handle multi-line continuation
                if input_buffer.is_empty() {
                    input_buffer = line;
                } else {
                    if input_buffer.trim_end().ends_with('\\') {
                        input_buffer.truncate(input_buffer.trim_end().len() - 1);
                    }
                    input_buffer.push('\n');
                    input_buffer.push_str(&line);
                }

                // Check if input is complete
                if is_incomplete(&input_buffer) {
                    continue;
                }

                let full_input = input_buffer.trim();
                if full_input.is_empty() {
                    input_buffer.clear();
                    continue;
                }

                // Execute and add to history
                input.add_history(full_input.to_string());
                if last_written.as_deref() != Some(full_input) {
                    if let Some(w) = history_writer.as_mut() {
                        let _ = writeln!(w, "{}", escape_history_line(full_input));
                        let _ = w.flush();
                    }
                    last_written = Some(full_input.to_string());
                }
                if let Err(e) = shell.execute(full_input) {
                    eprintln!("{}: {}", "error".red(), e);
                }
                #[cfg(windows)]
                {
                    if ctrlc::take() {
                        // Match common shell behavior (130 = interrupted)
                        shell.last_status = 130;
                        println!("^C");
                    }
                }

                input_buffer.clear();
                if shell.should_exit {
                    break;
                }
            }
            Ok(InputResult::Paste(lines)) => {
                // Execute each pasted command (with transcript-friendly prompt stripping)
                for cmd in normalize_pasted_lines(lines) {
                    let cmd = cmd.trim();
                    input.add_history(cmd.to_string());
                    if last_written.as_deref() != Some(cmd) {
                        if let Some(w) = history_writer.as_mut() {
                            let _ = writeln!(w, "{}", escape_history_line(cmd));
                            let _ = w.flush();
                        }
                        last_written = Some(cmd.to_string());
                    }
                    if let Err(e) = shell.execute(cmd) {
                        eprintln!("{}: {}", "error".red(), e);
                    }
                    #[cfg(windows)]
                    {
                        if ctrlc::take() {
                            shell.last_status = 130;
                            println!("^C");
                        }
                    }
                    if shell.should_exit {
                        break;
                    }
                }
                if shell.should_exit {
                    break;
                }
            }
            Ok(InputResult::Interrupt) => {
                input_buffer.clear();
                println!("^C");
            }
            Ok(InputResult::Eof) => {
                break;
            }
            Err(e) => {
                eprintln!("Input error: {}", e);
                break;
            }
        }
    }

    if let Some(w) = history_writer.as_mut() {
        let _ = w.flush();
    }

    println!("Goodbye!");
    Ok(shell.last_status)
}
