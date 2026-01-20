//! Shell core module

pub mod path;
pub mod builtin;
pub mod executor;
pub mod parser;
pub mod completer;
pub mod input;
pub mod busybox;
pub mod venv;

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use anyhow::Result;
use colored::Colorize;

use crate::task::TaskManager;

/// Main shell state
pub struct Shell {
    /// Current working directory
    pub cwd: PathBuf,
    /// Task manager for background jobs
    pub tasks: TaskManager,
    /// Command aliases (bash-style)
    pub aliases: HashMap<String, String>,
    /// Shell variables (non-exported)
    pub vars: HashMap<String, String>,
    /// Last command exit status (for $?)
    pub last_status: i32,
    /// Should exit
    pub should_exit: bool,
    /// Exit warning shown (for running jobs confirmation)
    pub exit_warned: bool,
}

impl Shell {
    pub fn new() -> Result<Self> {
        Ok(Self {
            cwd: env::current_dir()?,
            tasks: TaskManager::new(),
            aliases: HashMap::new(),
            vars: HashMap::new(),
            last_status: 0,
            should_exit: false,
            exit_warned: false,
        })
    }

    /// Execute a command line
    pub fn execute(&mut self, line: &str) -> Result<()> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        if line.starts_with('#') {
            // Treat full-line comments as no-ops (useful for pasted snippets and .titan scripts).
            return Ok(());
        }

        // Parse into AST and execute.
        let parsed = parser::parse(line)?;
        match parsed {
            parser::Command::Background(_cmd) => {
                // For now, reuse the existing background runner (string-based) to keep TaskManager
                // output capture behavior unchanged.
                let cmd_str = line.trim_end_matches('&').trim();
                executor::execute_background(&mut self.tasks, cmd_str, &self.cwd, &self.aliases)?;
                self.last_status = 0;
            }
            cmd => {
                let code = executor::execute_ast(self, &cmd)?;
                self.last_status = code;
            }
        }

        Ok(())
    }

    /// Get prompt string
    pub fn prompt(&self) -> String {
        fn shorten(s: &str, max: usize) -> String {
            if s.len() <= max {
                return s.to_string();
            }
            let head = s.chars().take(max / 2).collect::<String>();
            let tail = s.chars().rev().take(max / 2 - 1).collect::<String>();
            format!("{}…{}", head, tail.chars().rev().collect::<String>())
        }

        let cwd_str = shorten(&self.cwd.display().to_string(), 64);

        let mut out = String::new();
        if let Ok(venv) = env::var("VIRTUAL_ENV") {
            let venv = venv.trim().to_string();
            if !venv.is_empty() {
                let venv_path = PathBuf::from(&venv);
                let name = venv_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "venv".to_string());
                out.push_str(&format!("({}) ", name).bright_green().to_string());
            }
        }
        out.push_str(&"titan".bright_cyan().bold().to_string());
        out.push(' ');
        out.push_str(&cwd_str.white().to_string());
        out.push_str("> ");
        out
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new().expect("Failed to initialize shell")
    }
}
