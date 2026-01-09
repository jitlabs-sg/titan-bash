//! Command executor - runs external commands

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use anyhow::{bail, Context, Result};

use crate::task::{register_pid, unregister_pid, TaskId, TaskManager};
use super::builtin;
use super::parser::{
    needs_shell_features, split_args, Command as AstCommand, RedirectMode, Word, QuoteMode,
};
use glob::glob;
use os_pipe::{PipeReader, PipeWriter};
use super::path;
use super::busybox;
use super::venv;
use super::Shell;

/// Execute command via cmd /C (for commands needing shell features)
fn execute_via_cmd(cmd: &str, cwd: &Path) -> Result<i32> {
    let mut child = Command::new("cmd")
        .args(["/C", cmd])
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to execute: {}", cmd))?;

    let status = child.wait()?;
    Ok(status.code().unwrap_or(-1))
}

/// Execute command directly without shell (for simple commands)
fn execute_direct(cmd: &str, cwd: &Path) -> Result<i32> {
    let args = split_args(cmd);
    if args.is_empty() {
        return Ok(0);
    }

    let exe = &args[0];
    let cmd_args = &args[1..];

    let mut child = Command::new(exe)
        .args(cmd_args)
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to execute: {}", cmd))?;

    let status = child.wait()?;
    Ok(status.code().unwrap_or(-1))
}

/// Execute a command synchronously (foreground)
/// Uses smart dispatch: direct execution for simple commands, cmd /C for shell features
pub fn execute(cmd: &str, cwd: &Path) -> Result<i32> {
    // Commands with shell features (pipes, redirects, etc.) need cmd /C
    if needs_shell_features(cmd) {
        return execute_via_cmd(cmd, cwd);
    }

    // Try direct execution first
    match execute_direct(cmd, cwd) {
        Ok(code) => Ok(code),
        Err(_) => {
            // Fallback to cmd /C if direct execution fails
            // This handles cases like internal cmd commands (dir, echo, etc.)
            execute_via_cmd(cmd, cwd)
        }
    }
}

/// Execute a command in background
pub fn execute_background(
    tasks: &mut TaskManager,
    cmd: &str,
    cwd: &Path,
    aliases: &HashMap<String, String>,
) -> Result<TaskId> {
    let cmd_owned = cmd.to_string();
    let cwd_owned = cwd.to_path_buf();
    let aliases_owned = aliases.clone();
    let use_shell = needs_shell_features(cmd);

    let id = tasks.spawn(cmd, move |pid| {
        // For background jobs, discard output by default.
        //
        // Why: piping + user-space draining can still backpressure high-throughput loggers under
        // CPU contention (common in ML/GPU workloads), which makes the child appear "stuttery" or
        // "hung". Discarding output avoids that class of stalls and matches the current UX (we
        // don't print background output anyway; only job status is shown).
        let io = IoStreams {
            stdin: InputStream::Null,
            stdout: OutputStream::Null,
            stderr: OutputStream::Null,
        };

        let mut child = if use_shell {
            spawn_cmd_with_io(&cmd_owned, &cwd_owned, io)?
        } else {
            let args = split_args(&cmd_owned);
            if args.is_empty() {
                return Ok((0, String::new()));
            }
            let aliased = expand_alias_argv(&aliases_owned, &args);
            let expanded = expand_argv(0, &aliased);
            if expanded.is_empty() {
                return Ok((0, String::new()));
            }
            spawn_external_stage(&expanded, &cwd_owned, io)?
        };

        let child_pid = child.id();
        *pid.lock().unwrap() = Some(child_pid);
        register_pid(child_pid);

        let status = child.wait()?;
        unregister_pid(child_pid);

        Ok((status.code().unwrap_or(-1), String::new()))
    })?;

    println!("[{}] Started: {}", id, cmd);
    Ok(id)
}

/// Execute with output capture (for piping)
pub fn execute_capture(cmd: &str, cwd: &Path) -> Result<(i32, String, String)> {
    let mut child = Command::new("cmd")
        .args(["/C", cmd])
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to execute: {}", cmd))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Read stdout
    let stdout_handle = thread::spawn(move || {
        let mut buf = String::new();
        if let Some(out) = stdout {
            let reader = BufReader::new(out);
            for line in reader.lines().map_while(Result::ok) {
                println!("{}", line);
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    });

    // Read stderr
    let stderr_handle = thread::spawn(move || {
        let mut buf = String::new();
        if let Some(err) = stderr {
            let reader = BufReader::new(err);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("{}", line);
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    });

    let status = child.wait()?;
    let stdout_buf = stdout_handle.join().unwrap_or_default();
    let stderr_buf = stderr_handle.join().unwrap_or_default();

    Ok((status.code().unwrap_or(-1), stdout_buf, stderr_buf))
}

/// Execute a parsed AST (foreground).
pub fn execute_ast(shell: &mut Shell, cmd: &AstCommand) -> Result<i32> {
    execute_node_with_io(shell, cmd, IoStreams::inherit())
}

#[derive(Debug)]
enum InputStream {
    Inherit,
    Null,
    Pipe(PipeReader),
    File(fs::File),
}

impl InputStream {
    fn try_clone(&self) -> Result<InputStream> {
        Ok(match self {
            InputStream::Inherit => InputStream::Inherit,
            InputStream::Null => InputStream::Null,
            InputStream::Pipe(r) => InputStream::Pipe(r.try_clone()?),
            InputStream::File(f) => InputStream::File(f.try_clone()?),
        })
    }

    fn into_stdio(self) -> Stdio {
        match self {
            InputStream::Inherit => Stdio::inherit(),
            InputStream::Null => Stdio::null(),
            InputStream::Pipe(r) => Stdio::from(r),
            InputStream::File(f) => Stdio::from(f),
        }
    }
}

#[derive(Debug)]
enum OutputStream {
    Inherit,
    Null,
    Pipe(PipeWriter),
    File(fs::File),
}

impl OutputStream {
    fn try_clone(&self) -> Result<OutputStream> {
        Ok(match self {
            OutputStream::Inherit => OutputStream::Inherit,
            OutputStream::Null => OutputStream::Null,
            OutputStream::Pipe(w) => OutputStream::Pipe(w.try_clone()?),
            OutputStream::File(f) => OutputStream::File(f.try_clone()?),
        })
    }

    fn into_stdio(self) -> Stdio {
        match self {
            OutputStream::Inherit => Stdio::inherit(),
            OutputStream::Null => Stdio::null(),
            OutputStream::Pipe(w) => Stdio::from(w),
            OutputStream::File(f) => Stdio::from(f),
        }
    }
}

#[derive(Debug)]
struct IoStreams {
    stdin: InputStream,
    stdout: OutputStream,
    stderr: OutputStream,
}

impl IoStreams {
    fn inherit() -> Self {
        Self {
            stdin: InputStream::Inherit,
            stdout: OutputStream::Inherit,
            stderr: OutputStream::Inherit,
        }
    }

    fn try_clone(&self) -> Result<IoStreams> {
        Ok(IoStreams {
            stdin: self.stdin.try_clone()?,
            stdout: self.stdout.try_clone()?,
            stderr: self.stderr.try_clone()?,
        })
    }
}

#[derive(Clone, Copy)]
struct RedirectSpec<'a> {
    target: &'a Word,
    mode: &'a RedirectMode,
}

fn split_redirects<'a>(cmd: &'a AstCommand) -> (&'a AstCommand, Vec<RedirectSpec<'a>>) {
    let mut redirects: Vec<RedirectSpec<'a>> = Vec::new();
    let mut current = cmd;
    while let AstCommand::Redirect { cmd, target, mode } = current {
        redirects.push(RedirectSpec { target, mode });
        current = cmd;
    }
    // Redirects are nested outermost-last; reverse to apply in left-to-right order.
    redirects.reverse();
    (current, redirects)
}

fn apply_redirects(shell: &mut Shell, mut io: IoStreams, redirects: &[RedirectSpec<'_>]) -> Result<IoStreams> {
    for r in redirects {
        match r.mode {
            RedirectMode::MergeStderrToStdout => {
                io.stderr = io.stdout.try_clone()?;
            }
            RedirectMode::Input => {
                let input_path = resolve_redirect_target(shell, r.target)?;
                let f = fs::File::open(&input_path)
                    .with_context(|| format!("redirect: cannot read '{}'", input_path.display()))?;
                io.stdin = InputStream::File(f);
            }
            RedirectMode::Overwrite | RedirectMode::Append => {
                let output_path = resolve_redirect_target(shell, r.target)?;
                let target_text = expand_word_first(shell, r.target)?;
                if path::is_windows_reserved_name(&output_path) {
                    bail!("redirect: {}", path::reserved_name_error(&target_text));
                }

                let f = fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(matches!(r.mode, RedirectMode::Overwrite))
                    .append(matches!(r.mode, RedirectMode::Append))
                    .open(&output_path)
                    .with_context(|| format!("redirect: cannot open '{}'", output_path.display()))?;
                io.stdout = OutputStream::File(f);
            }
            RedirectMode::StderrOverwrite | RedirectMode::StderrAppend => {
                let output_path = resolve_redirect_target(shell, r.target)?;
                let target_text = expand_word_first(shell, r.target)?;
                if path::is_windows_reserved_name(&output_path) {
                    bail!("redirect: {}", path::reserved_name_error(&target_text));
                }

                let f = fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(matches!(r.mode, RedirectMode::StderrOverwrite))
                    .append(matches!(r.mode, RedirectMode::StderrAppend))
                    .open(&output_path)
                    .with_context(|| format!("redirect: cannot open '{}'", output_path.display()))?;
                io.stderr = OutputStream::File(f);
            }
        }
    }
    Ok(io)
}

fn execute_node_with_io(shell: &mut Shell, cmd: &AstCommand, io: IoStreams) -> Result<i32> {
    let (base, redirects) = split_redirects(cmd);
    let io = if redirects.is_empty() {
        io
    } else {
        apply_redirects(shell, io, &redirects)?
    };

    match base {
        AstCommand::Empty => Ok(0),
        AstCommand::Simple(argv) => execute_simple_with_io(shell, argv, io),
        AstCommand::Sequence(list) => {
            let mut last = 0;
            for c in list {
                last = execute_node_with_io(shell, c, IoStreams::inherit())?;
            }
            Ok(last)
        }
        AstCommand::Pipeline(stages) => execute_pipeline_with_io(shell, stages, io),
        AstCommand::And(left, right) => {
            let code = execute_node_with_io(shell, left, io)?;
            if code == 0 {
                execute_node_with_io(shell, right, IoStreams::inherit())
            } else {
                Ok(code)
            }
        }
        AstCommand::Or(left, right) => {
            let code = execute_node_with_io(shell, left, io)?;
            if code != 0 {
                execute_node_with_io(shell, right, IoStreams::inherit())
            } else {
                Ok(code)
            }
        }
        AstCommand::Background(_) => bail!("Background jobs must be handled by Shell"),
        AstCommand::Redirect { .. } => unreachable!("redirects flattened above"),
    }
}

fn execute_pipeline_with_io(shell: &mut Shell, stages: &[AstCommand], io: IoStreams) -> Result<i32> {
    if stages.is_empty() {
        return Ok(0);
    }

    let IoStreams {
        stdin: pipeline_stdin,
        stdout: pipeline_stdout,
        stderr: pipeline_stderr,
    } = io;

    let cwd = shell.cwd.clone();
    let stderr_base = pipeline_stderr;

    enum StageHandle {
        Builtin(thread::JoinHandle<Result<i32>>),
        External(std::process::Child),
    }

    let mut handles: Vec<StageHandle> = Vec::new();
    let mut prev_reader: Option<PipeReader> = None;

    for (idx, stage) in stages.iter().enumerate() {
        let (base, redirects) = split_redirects(stage);
        let AstCommand::Simple(words) = base else {
            bail!("pipeline: unsupported stage");
        };

        let aliased = expand_alias_words(&shell.aliases, words);
        let expanded = expand_words(shell, &aliased)?;
        if expanded.is_empty() {
            bail!("pipeline: empty stage");
        }

        let is_last = idx + 1 == stages.len();

        let stdin = if idx == 0 {
            pipeline_stdin.try_clone()?
        } else {
            InputStream::Pipe(prev_reader.take().ok_or_else(|| anyhow::anyhow!("pipeline: missing input"))?)
        };

        let (stdout, next_reader) = if is_last {
            (pipeline_stdout.try_clone()?, None)
        } else {
            let (r, w) = os_pipe::pipe()?;
            (OutputStream::Pipe(w), Some(r))
        };
        prev_reader = next_reader;

        let stderr = stderr_base.try_clone()?;
        let stage_io = IoStreams { stdin, stdout, stderr };
        let stage_io = apply_redirects(shell, stage_io, &redirects)?;

        let name = expanded[0].clone();
        let args: Vec<String> = expanded.iter().skip(1).cloned().collect();

        if builtin::is_builtin(&name) {
            if builtin::is_state_builtin(&name) {
                bail!("'{}' cannot be used in a pipeline", name);
            }

            let stage_cwd = cwd.clone();
            let handle = thread::spawn(move || {
                let mut temp_shell = Shell {
                    cwd: stage_cwd,
                    tasks: TaskManager::new(),
                    aliases: HashMap::new(),
                    vars: HashMap::new(),
                    last_status: 0,
                    should_exit: false,
                };
                run_builtin_stage(&mut temp_shell, &name, &args, stage_io)
            });
            handles.push(StageHandle::Builtin(handle));
        } else {
            let child = spawn_external_stage(&expanded, &cwd, stage_io)?;
            handles.push(StageHandle::External(child));
        }
    }

    let mut exit_codes: Vec<i32> = Vec::new();
    for handle in handles {
        match handle {
            StageHandle::Builtin(h) => {
                exit_codes.push(h.join().unwrap_or_else(|_| Ok(1))?);
            }
            StageHandle::External(mut child) => {
                let status = child.wait()?;
                exit_codes.push(status.code().unwrap_or(-1));
            }
        }
    }

    Ok(*exit_codes.last().unwrap_or(&0))
}

fn run_builtin_stage(shell: &mut Shell, name: &str, args: &[String], io: IoStreams) -> Result<i32> {
    // stdin
    let mut stdin_box: Box<dyn BufRead> = match io.stdin {
        InputStream::Inherit => Box::new(BufReader::new(io::stdin())),
        InputStream::Null => Box::new(BufReader::new(io::empty())),
        InputStream::Pipe(r) => Box::new(BufReader::new(r)),
        InputStream::File(f) => Box::new(BufReader::new(f)),
    };

    // stdout
    let mut stdout_box: Box<dyn Write> = match io.stdout {
        OutputStream::Inherit => Box::new(io::stdout()),
        OutputStream::Null => Box::new(io::sink()),
        OutputStream::Pipe(w) => Box::new(w),
        OutputStream::File(f) => Box::new(f),
    };

    // stderr
    let mut stderr_box: Box<dyn Write> = match io.stderr {
        OutputStream::Inherit => Box::new(io::stderr()),
        OutputStream::Null => Box::new(io::sink()),
        OutputStream::Pipe(w) => Box::new(w),
        OutputStream::File(f) => Box::new(f),
    };

    match builtin::run_builtin_io(
        shell,
        name,
        args,
        &mut *stdin_box,
        &mut *stdout_box,
        &mut *stderr_box,
    ) {
        Ok(code) => {
            let _ = stdout_box.flush();
            let _ = stderr_box.flush();
            Ok(code)
        }
        Err(e) => {
            let _ = writeln!(stderr_box, "{}", e);
            let _ = stderr_box.flush();
            Ok(1)
        }
    }
}

fn execute_simple_with_io(shell: &mut Shell, argv: &[Word], io: IoStreams) -> Result<i32> {
    if argv.is_empty() {
        return Ok(0);
    }

    let aliased = expand_alias_words(&shell.aliases, argv);
    let expanded = expand_words(shell, &aliased)?;
    if expanded.is_empty() {
        return Ok(0);
    }

    let name = &expanded[0];
    let args: Vec<String> = expanded.iter().skip(1).cloned().collect();

    // Python venv activation must happen in-process (affects PATH/VIRTUAL_ENV).
    if let Some(code) = venv::try_activate_from_command(shell, name)? {
        return Ok(code);
    }

    if builtin::is_builtin(name) {
        return run_builtin_stage(shell, name, &args, io);
    }

    let io_direct = io.try_clone()?;
    match spawn_external_direct(&expanded, &shell.cwd, io_direct) {
        Ok(mut child) => Ok(child.wait()?.code().unwrap_or(-1)),
        Err(e) => {
            let io_ps1 = io.try_clone()?;
            if let Some(mut child) = try_spawn_ps1_fallback(&expanded, &shell.cwd, io_ps1)? {
                return Ok(child.wait()?.code().unwrap_or(-1));
            }

            if is_not_found_error(&e) {
                let io_bb = io.try_clone()?;
                if let Some(mut child) = try_spawn_busybox_applet(&expanded, &shell.cwd, io_bb)? {
                    return Ok(child.wait()?.code().unwrap_or(-1));
                }
            }

            let cmdline = join_cmdline(&expanded);
            let mut child = spawn_cmd_with_io(&cmdline, &shell.cwd, io)?;
            Ok(child.wait()?.code().unwrap_or(-1))
        }
    }
}

fn spawn_external_stage(argv: &[String], cwd: &Path, io: IoStreams) -> Result<std::process::Child> {
    let io_direct = io.try_clone()?;
    match spawn_external_direct(argv, cwd, io_direct) {
        Ok(child) => Ok(child),
        Err(e) => {
            let io_ps1 = io.try_clone()?;
            if let Some(child) = try_spawn_ps1_fallback(argv, cwd, io_ps1)? {
                return Ok(child);
            }

            if is_not_found_error(&e) {
                let io_bb = io.try_clone()?;
                if let Some(child) = try_spawn_busybox_applet(argv, cwd, io_bb)? {
                    return Ok(child);
                }
            }

            let cmdline = join_cmdline(argv);
            spawn_cmd_with_io(&cmdline, cwd, io)
        }
    }
}

fn try_spawn_ps1_fallback(
    argv: &[String],
    cwd: &Path,
    io: IoStreams,
) -> Result<Option<std::process::Child>> {
    let Some(cmd) = argv.first() else { return Ok(None) };
    let cmd_path = Path::new(cmd);
    if cmd_path.extension().is_some() {
        return Ok(None);
    }

    let Some(script_path) = find_ps1_candidate(cmd, cwd) else {
        return Ok(None);
    };

    let args_only: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();
    let script_str = script_path.to_string_lossy().to_string();
    Ok(Some(spawn_powershell_with_io(&script_str, &args_only, cwd, io)?))
}

fn find_ps1_candidate(cmd: &str, cwd: &Path) -> Option<PathBuf> {
    let candidate_name = format!("{}.ps1", cmd);

    // If the command includes a path component, resolve relative to cwd and check directly.
    if cmd.contains('\\') || cmd.contains('/') || cmd.contains(':') {
        let candidate = path::resolve_fs(cwd, &candidate_name);
        return candidate.is_file().then_some(candidate);
    }

    // 1) Current directory
    let candidate = path::resolve_fs(cwd, &candidate_name);
    if candidate.is_file() {
        return Some(candidate);
    }

    // 2) PATH
    let Ok(path_env) = std::env::var("PATH") else {
        return None;
    };

    for dir in path_env.split(';').filter(|d| !d.is_empty()) {
        let mut p = PathBuf::from(dir);
        p.push(&candidate_name);
        let p = PathBuf::from(path::add_long_path_prefix(&p.to_string_lossy()));
        if p.is_file() {
            return Some(p);
        }
    }

    None
}

fn spawn_external_direct(argv: &[String], cwd: &Path, io: IoStreams) -> Result<std::process::Child> {
    if argv.is_empty() {
        bail!("execute: empty argv");
    }

    let exe_path = &argv[0];
    let args_only: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();

    let lower = exe_path.to_ascii_lowercase();
    if lower.ends_with(".ps1") {
        return spawn_powershell_with_io(exe_path, &args_only, cwd, io);
    }
    if lower.ends_with(".bat") || lower.ends_with(".cmd") {
        return spawn_cmd_script_with_io(exe_path, &args_only, cwd, io);
    }

    let mut cmd = Command::new(exe_path);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(io.stdin.into_stdio())
        .stdout(io.stdout.into_stdio())
        .stderr(io.stderr.into_stdio());

    cmd.spawn()
        .with_context(|| format!("Failed to execute: {}", exe_path))
}

fn is_not_found_error(err: &anyhow::Error) -> bool {
    err.chain().any(|e| {
        e.downcast_ref::<io::Error>()
            .is_some_and(|ioe| ioe.kind() == io::ErrorKind::NotFound)
    })
}

fn try_spawn_busybox_applet(
    argv: &[String],
    cwd: &Path,
    io: IoStreams,
) -> Result<Option<std::process::Child>> {
    let Some(cmd) = argv.first() else { return Ok(None) };
    if busybox::looks_like_path(cmd) {
        return Ok(None);
    }

    let applet = busybox::normalize_applet_name(cmd);
    if !busybox::has_applet(&applet) {
        return Ok(None);
    }

    let mut applet_argv: Vec<String> = Vec::with_capacity(argv.len());
    applet_argv.push(applet.clone());
    applet_argv.extend(argv.iter().skip(1).cloned());

    let Some(bb_argv) = busybox::resolve_busybox_argv(&applet, &applet_argv) else {
        return Ok(None);
    };

    Ok(Some(spawn_external_direct(&bb_argv, cwd, io)?))
}

fn spawn_cmd_with_io(cmdline: &str, cwd: &Path, io: IoStreams) -> Result<std::process::Child> {
    Command::new("cmd")
        .args(["/C", cmdline])
        .current_dir(cwd)
        .stdin(io.stdin.into_stdio())
        .stdout(io.stdout.into_stdio())
        .stderr(io.stderr.into_stdio())
        .spawn()
        .with_context(|| format!("Failed to execute via cmd: {}", cmdline))
}

fn spawn_cmd_script_with_io(script: &str, args: &[&str], cwd: &Path, io: IoStreams) -> Result<std::process::Child> {
    Command::new("cmd")
        .args(["/C", script])
        .args(args)
        .current_dir(cwd)
        .stdin(io.stdin.into_stdio())
        .stdout(io.stdout.into_stdio())
        .stderr(io.stderr.into_stdio())
        .spawn()
        .with_context(|| format!("Failed to execute script: {}", script))
}

fn spawn_powershell_with_io(script: &str, args: &[&str], cwd: &Path, io: IoStreams) -> Result<std::process::Child> {
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            script,
        ])
        .args(args)
        .current_dir(cwd)
        .stdin(io.stdin.into_stdio())
        .stdout(io.stdout.into_stdio())
        .stderr(io.stderr.into_stdio())
        .spawn()
        .with_context(|| format!("Failed to execute script: {}", script))
}

fn resolve_redirect_target(shell: &mut Shell, target: &Word) -> Result<std::path::PathBuf> {
    let expanded = expand_word_first(shell, target)?;
    Ok(path::resolve_fs(&shell.cwd, &expanded))
}

/// Expand aliases in a simple argv vector (used by legacy/background execution paths)
fn expand_alias_argv(aliases: &HashMap<String, String>, argv: &[String]) -> Vec<String> {
    const MAX_EXPANSIONS: usize = 16;

    if argv.is_empty() {
        return Vec::new();
    }

    let mut current: Vec<String> = argv.to_vec();
    let mut seen: HashSet<String> = HashSet::new();

    for _ in 0..MAX_EXPANSIONS {
        let Some(first) = current.first() else { break };
        let Some(replacement) = aliases.get(first) else { break };

        // Stop if we detect a loop
        if !seen.insert(first.to_string()) {
            break;
        }

        let mut repl = split_args(replacement);
        // If alias is empty, effectively drop the first word
        if repl.is_empty() {
            current.remove(0);
        } else {
            repl.extend(current.iter().skip(1).cloned());
            current = repl;
        }
    }

    current
}

/// Expand special vars ($?) and environment variables in argv (legacy/background path)
fn expand_argv(last_status: i32, argv: &[String]) -> Vec<String> {
    let status = last_status.to_string();
    argv.iter()
        .map(|arg| {
            let with_status = arg.replace("${?}", &status).replace("$?", &status);
            path::expand_env(&with_status)
        })
        .collect()
}

fn expand_alias_words(aliases: &HashMap<String, String>, argv: &[Word]) -> Vec<Word> {
    const MAX_EXPANSIONS: usize = 16;

    let mut current = argv.to_vec();
    let mut seen: HashSet<String> = HashSet::new();

    for _ in 0..MAX_EXPANSIONS {
        let Some(first) = current.first() else {
            break;
        };
        // First word as string (without expansions)
        let first_text = first
            .parts
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        let Some(replacement) = aliases.get(&first_text) else {
            break;
        };
        if !seen.insert(first_text.clone()) {
            break;
        }

        let repl = split_args(replacement);
        let mut new_words: Vec<Word> = repl
            .into_iter()
            .map(|s| Word::from_str(&s))
            .collect();
        new_words.extend_from_slice(&current[1..]);
        current = new_words;
    }

    current
}

/// Expand environment variables and glob patterns in all arguments
fn expand_words(shell: &mut Shell, argv: &[Word]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for w in argv {
        let parts = expand_word_list(shell, w)?;
        out.extend(parts);
    }
    Ok(out)
}

/// Expand a single word into one or more arguments (glob aware)
fn expand_word_list(shell: &mut Shell, word: &Word) -> Result<Vec<String>> {
    let status = shell.last_status.to_string();
    let mut literal = String::new();
    let mut any_unquoted = false;

    for part in &word.parts {
        match part.quote {
            QuoteMode::Single => {
                literal.push_str(&part.text);
            }
            QuoteMode::Double | QuoteMode::None => {
                any_unquoted = true;
                let mut expanded = part.text.replace("${?}", &status).replace("$?", &status);
                expanded = path::expand_env(&expanded);
                literal.push_str(&expanded);
            }
        }
    }

    // If entirely single-quoted, no glob expansion
    if !any_unquoted {
        return Ok(vec![literal]);
    }

    let has_glob = literal.contains('*') || literal.contains('?') || literal.contains('[');
    if !has_glob {
        return Ok(vec![literal]);
    }

    // Resolve relative pattern for globbing
    let pattern_path = path::resolve(&shell.cwd, &literal);
    let pattern_str = pattern_path.to_string_lossy().to_string();
    let mut matches = Vec::new();
    if let Ok(paths) = glob(&pattern_str) {
        for p in paths.flatten() {
            matches.push(p.to_string_lossy().to_string());
        }
    }

    if matches.is_empty() {
        Ok(vec![literal])
    } else {
        Ok(matches)
    }
}

fn expand_word_first(shell: &mut Shell, word: &Word) -> Result<String> {
    let list = expand_word_list(shell, word)?;
    Ok(list.into_iter().next().unwrap_or_default())
}

fn execute_simple_stream(shell: &mut Shell, argv: &[Word], stdin: Option<&[u8]>) -> Result<i32> {
    if argv.is_empty() {
        return Ok(0);
    }

    // Expand aliases, then environment variables
    let aliased = expand_alias_words(&shell.aliases, argv);
    let expanded = expand_words(shell, &aliased)?;
    if expanded.is_empty() {
        return Ok(0);
    }
    let name = &expanded[0];
    let args: Vec<String> = expanded.iter().skip(1).cloned().collect();

    if builtin::is_builtin(name) {
        if builtin::is_state_builtin(name) && stdin.is_some() {
            bail!("'{}' cannot be used in a pipeline/redirect", name);
        }
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let code = builtin::run_builtin_captured(shell, name, &args, &mut out)?;
        let _ = out.flush();
        return Ok(code);
    }

    execute_external_stream(&expanded, &shell.cwd, stdin)
        .or_else(|_| execute_via_cmd_stream(&join_cmdline(&expanded), &shell.cwd, stdin))
}

fn execute_simple_capture(
    shell: &mut Shell,
    argv: &[Word],
    stdin: Option<&[u8]>,
) -> Result<(i32, Vec<u8>)> {
    if argv.is_empty() {
        return Ok((0, Vec::new()));
    }

    // Expand aliases, then environment variables
    let aliased = expand_alias_words(&shell.aliases, argv);
    let expanded = expand_words(shell, &aliased)?;
    if expanded.is_empty() {
        return Ok((0, Vec::new()));
    }
    let name = &expanded[0];
    let args: Vec<String> = expanded.iter().skip(1).cloned().collect();

    if builtin::is_builtin(name) {
        if builtin::is_state_builtin(name) {
            bail!("'{}' cannot be used in a pipeline/redirect", name);
        }
        let mut out = Vec::<u8>::new();
        let code = builtin::run_builtin_captured(shell, name, &args, &mut out)?;
        return Ok((code, out));
    }

    execute_external_capture(&expanded, &shell.cwd, stdin)
        .or_else(|_| execute_via_cmd_capture(&join_cmdline(&expanded), &shell.cwd, stdin))
}

fn execute_external_stream(argv: &[String], cwd: &Path, stdin: Option<&[u8]>) -> Result<i32> {
    if argv.is_empty() {
        return Ok(0);
    }

    let exe_path = &argv[0];
    let args_only: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();

    // Handle Windows script types explicitly
    if exe_path.to_ascii_lowercase().ends_with(".ps1") {
        return execute_powershell_stream(exe_path, &args_only, cwd, stdin);
    }
    if exe_path.to_ascii_lowercase().ends_with(".bat") || exe_path.to_ascii_lowercase().ends_with(".cmd") {
        return execute_cmd_script_stream(exe_path, &args_only, cwd, stdin);
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd);

    if let Some(input) = stdin {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute: {}", argv[0]))?;

        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(input)?;
        }

        let status = child.wait()?;
        return Ok(status.code().unwrap_or(-1));
    }

    let mut child = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to execute: {}", argv[0]))?;

    let status = child.wait()?;
    Ok(status.code().unwrap_or(-1))
}

fn execute_external_capture(
    argv: &[String],
    cwd: &Path,
    stdin: Option<&[u8]>,
) -> Result<(i32, Vec<u8>)> {
    if argv.is_empty() {
        return Ok((0, Vec::new()));
    }

    let exe_path = &argv[0];
    let args_only: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();
    if exe_path.to_ascii_lowercase().ends_with(".ps1") {
        return execute_powershell_capture(exe_path, &args_only, cwd, stdin);
    }
    if exe_path.to_ascii_lowercase().ends_with(".bat") || exe_path.to_ascii_lowercase().ends_with(".cmd") {
        return execute_cmd_script_capture(exe_path, &args_only, cwd, stdin);
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::inherit());
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to execute: {}", argv[0]))?;

    let write_handle = if let Some(input) = stdin {
        let input = input.to_vec();
        match child.stdin.take() {
            Some(mut child_stdin) => Some(thread::spawn(move || {
                let _ = child_stdin.write_all(&input);
            })),
            None => None,
        }
    } else {
        None
    };

    let mut out = Vec::new();
    if let Some(mut child_stdout) = child.stdout.take() {
        child_stdout.read_to_end(&mut out)?;
    }

    let status = child.wait()?;
    if let Some(h) = write_handle {
        let _ = h.join();
    }

    Ok((status.code().unwrap_or(-1), out))
}

fn execute_via_cmd_stream(cmdline: &str, cwd: &Path, stdin: Option<&[u8]>) -> Result<i32> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", cmdline]).current_dir(cwd);

    if let Some(input) = stdin {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute via cmd: {}", cmdline))?;

        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(input)?;
        }

        let status = child.wait()?;
        return Ok(status.code().unwrap_or(-1));
    }

    let mut child = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to execute via cmd: {}", cmdline))?;

    let status = child.wait()?;
    Ok(status.code().unwrap_or(-1))
}

fn execute_via_cmd_capture(cmdline: &str, cwd: &Path, stdin: Option<&[u8]>) -> Result<(i32, Vec<u8>)> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", cmdline])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::inherit());
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to execute via cmd: {}", cmdline))?;

    let write_handle = if let Some(input) = stdin {
        let input = input.to_vec();
        match child.stdin.take() {
            Some(mut child_stdin) => Some(thread::spawn(move || {
                let _ = child_stdin.write_all(&input);
            })),
            None => None,
        }
    } else {
        None
    };

    let mut out = Vec::new();
    if let Some(mut child_stdout) = child.stdout.take() {
        child_stdout.read_to_end(&mut out)?;
    }

    let status = child.wait()?;
    if let Some(h) = write_handle {
        let _ = h.join();
    }

    Ok((status.code().unwrap_or(-1), out))
}

fn join_cmdline(argv: &[String]) -> String {
    argv.iter().map(quote_cmd_arg).collect::<Vec<_>>().join(" ")
}

fn quote_cmd_arg(arg: &String) -> String {
    if arg.contains(' ') || arg.contains('\t') || arg.contains('"') {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.clone()
    }
}

fn execute_cmd_script_stream(script: &str, args: &[&str], cwd: &Path, stdin: Option<&[u8]>) -> Result<i32> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", script]).args(args).current_dir(cwd);

    if let Some(input) = stdin {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute script: {}", script))?;

        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(input)?;
        }

        let status = child.wait()?;
        Ok(status.code().unwrap_or(-1))
    } else {
        let status = cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute script: {}", script))?
            .wait()?;
        Ok(status.code().unwrap_or(-1))
    }
}

fn execute_powershell_stream(script: &str, args: &[&str], cwd: &Path, stdin: Option<&[u8]>) -> Result<i32> {
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        script,
    ])
    .args(args)
    .current_dir(cwd);

    if let Some(input) = stdin {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute script: {}", script))?;

        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(input)?;
        }

        let status = child.wait()?;
        Ok(status.code().unwrap_or(-1))
    } else {
        let status = cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute script: {}", script))?
            .wait()?;
        Ok(status.code().unwrap_or(-1))
    }
}

fn execute_cmd_script_capture(
    script: &str,
    args: &[&str],
    cwd: &Path,
    stdin: Option<&[u8]>,
) -> Result<(i32, Vec<u8>)> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", script])
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::inherit());
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to execute script: {}", script))?;

    let write_handle = if let Some(input) = stdin {
        let input = input.to_vec();
        match child.stdin.take() {
            Some(mut child_stdin) => Some(thread::spawn(move || {
                let _ = child_stdin.write_all(&input);
            })),
            None => None,
        }
    } else {
        None
    };

    let mut out = Vec::new();
    if let Some(mut child_stdout) = child.stdout.take() {
        child_stdout.read_to_end(&mut out)?;
    }

    let status = child.wait()?;
    if let Some(h) = write_handle {
        let _ = h.join();
    }

    Ok((status.code().unwrap_or(-1), out))
}

fn execute_powershell_capture(
    script: &str,
    args: &[&str],
    cwd: &Path,
    stdin: Option<&[u8]>,
) -> Result<(i32, Vec<u8>)> {
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        script,
    ])
    .args(args)
    .current_dir(cwd)
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit());

    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::inherit());
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to execute script: {}", script))?;

    let write_handle = if let Some(input) = stdin {
        let input = input.to_vec();
        match child.stdin.take() {
            Some(mut child_stdin) => Some(thread::spawn(move || {
                let _ = child_stdin.write_all(&input);
            })),
            None => None,
        }
    } else {
        None
    };

    let mut out = Vec::new();
    if let Some(mut child_stdout) = child.stdout.take() {
        child_stdout.read_to_end(&mut out)?;
    }

    let status = child.wait()?;
    if let Some(h) = write_handle {
        let _ = h.join();
    }

    Ok((status.code().unwrap_or(-1), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_alias_simple() {
        let mut aliases = HashMap::new();
        aliases.insert("ll".to_string(), "ls -la".to_string());

        let argv = vec!["ll".to_string(), "src".to_string()];
        assert_eq!(
            expand_alias_argv(&aliases, &argv),
            vec!["ls".to_string(), "-la".to_string(), "src".to_string()]
        );
    }

    #[test]
    fn test_expand_alias_chained_and_quoted() {
        let mut aliases = HashMap::new();
        aliases.insert("a".to_string(), "b".to_string());
        aliases.insert("b".to_string(), r#"echo "hello world""#.to_string());

        let argv = vec!["a".to_string()];
        assert_eq!(
            expand_alias_argv(&aliases, &argv),
            vec!["echo".to_string(), "hello world".to_string()]
        );
    }

    #[test]
    fn test_expand_alias_recursion_stops() {
        let mut aliases = HashMap::new();
        aliases.insert("a".to_string(), "a".to_string());

        let argv = vec!["a".to_string()];
        assert_eq!(expand_alias_argv(&aliases, &argv), vec!["a".to_string()]);
    }

    #[test]
    fn test_expand_alias_empty_replacement() {
        let mut aliases = HashMap::new();
        aliases.insert("noop".to_string(), "".to_string());

        let argv = vec!["noop".to_string(), "x".to_string()];
        assert_eq!(expand_alias_argv(&aliases, &argv), vec!["x".to_string()]);
    }

    #[test]
    fn test_expand_argv_status() {
        let argv = vec!["echo".to_string(), "$?".to_string(), "${?}".to_string()];
        let expanded = expand_argv(42, &argv);
        assert_eq!(expanded, vec!["echo".to_string(), "42".to_string(), "42".to_string()]);
    }
}
