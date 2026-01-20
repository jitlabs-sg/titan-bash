//! Tab completion for TITAN Bash
//!
//! Copied from CLI_TUI_DEEP_DIVE_ANALYSIS.md Section 6.1

use std::borrow::Cow;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::Helper;
use rustyline::Context;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use super::busybox;

/// Built-in commands for tab completion
const BUILTIN_COMMANDS: &[&str] = &[
    "cd", "pwd", "ls", "dir", "cat", "type", "echo", "clear", "cls",
    "exit", "quit", "jobs", "export", "set", "env", "printenv", "which", "where",
    "activate", "deactivate",
    "mkdir", "rm", "del", "cp", "copy", "mv", "move", "touch",
    "history", "help", "head", "tail", "whoami", "hostname",
    "md5sum", "sha1sum", "sha256sum", "sha512sum", "fg", "wait", "kill",
];

pub struct TitanHelper {
    /// Current working directory for path completion
    pub cwd: PathBuf,
    path_cmds: Arc<RwLock<Vec<String>>>,
    last_path_env: Arc<RwLock<String>>,
}

impl TitanHelper {
    pub fn new(cwd: std::path::PathBuf) -> Self {
        let last_path = std::env::var("PATH").unwrap_or_default();
        let helper = Self {
            cwd,
            path_cmds: Arc::new(RwLock::new(Vec::new())),
            last_path_env: Arc::new(RwLock::new(last_path)),
        };
        helper.refresh_path_commands();
        helper
    }

    pub fn set_cwd(&mut self, cwd: std::path::PathBuf) {
        self.cwd = cwd;
    }

    fn refresh_path_commands(&self) {
        let path_env_current = std::env::var("PATH").unwrap_or_default();
        let mut set: HashSet<String> = HashSet::new();
        for builtin in BUILTIN_COMMANDS {
            set.insert((*builtin).to_string());
        }

        // Add BusyBox applets (if a bundled BusyBox is available).
        for applet in busybox::applets() {
            set.insert(applet);
        }

        for dir in path_env_current.split(';') {
            if dir.is_empty() {
                continue;
            }
            let path = PathBuf::from(dir);
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    if let Ok(ft) = entry.file_type() {
                        if ft.is_file() {
                            if let Some(name) = entry.file_name().to_str() {
                                let lower = name.to_ascii_lowercase();
                                if lower.ends_with(".exe")
                                    || lower.ends_with(".bat")
                                    || lower.ends_with(".cmd")
                                    || lower.ends_with(".ps1")
                                {
                                    let stem = lower
                                        .trim_end_matches(".exe")
                                        .trim_end_matches(".bat")
                                        .trim_end_matches(".cmd")
                                        .trim_end_matches(".ps1")
                                        .to_string();
                                    set.insert(stem);
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Ok(mut w) = self.path_cmds.write() {
            *w = set.into_iter().collect();
            w.sort();
        }

        if let Ok(mut w) = self.last_path_env.write() {
            *w = path_env_current;
        }
    }

    /// Complete file/directory paths
    fn complete_path(&self, partial: &str) -> Vec<Pair> {
        let mut candidates = Vec::new();

        // Determine base path and prefix to search
        let (search_dir, prefix) = if partial.contains('/') || partial.contains('\\') {
            // Has path separator - split into dir and filename prefix
            let path = std::path::Path::new(partial);
            if let Some(parent) = path.parent() {
                let parent_path = if parent.as_os_str().is_empty() {
                    self.cwd.clone()
                } else if parent.is_absolute() {
                    parent.to_path_buf()
                } else {
                    self.cwd.join(parent)
                };
                let prefix = path.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                (parent_path, prefix)
            } else {
                (self.cwd.clone(), partial.to_string())
            }
        } else {
            // No separator - search in cwd
            (self.cwd.clone(), partial.to_string())
        };

        // Read directory and find matches
        if let Ok(entries) = std::fs::read_dir(&search_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name().to_string_lossy().to_string();
                let name_lower = name.to_lowercase();
                let prefix_lower = prefix.to_lowercase();

                if name_lower.starts_with(&prefix_lower) {
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    let display = if is_dir {
                        format!("{}/", name)
                    } else {
                        name.clone()
                    };

                    // Build replacement - need to include the path up to the prefix
                    let replacement = if partial.contains('/') || partial.contains('\\') {
                        let parent = std::path::Path::new(partial).parent()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if parent.is_empty() {
                            display.clone()
                        } else {
                            format!("{}/{}", parent.replace('\\', "/"), if is_dir { format!("{}/", name) } else { name })
                        }
                    } else {
                        display.clone()
                    };

                    candidates.push(Pair {
                        display,
                        replacement,
                    });
                }
            }
        }

        candidates.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
        candidates
    }
}

impl Completer for TitanHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if let Ok(current) = std::env::var("PATH") {
            if let Ok(last) = self.last_path_env.read() {
                if *last != current {
                    self.refresh_path_commands();
                }
            }
        }

        let line_to_cursor = &line[..pos];

        // Quote-aware tokenization (only for completion; not full parser).
        let mut tokens: Vec<(usize, String)> = Vec::new();
        let mut buf = String::new();
        let mut token_start: Option<usize> = None;
        let mut in_single = false;
        let mut in_double = false;
        for (i, c) in line_to_cursor.char_indices() {
            match c {
                '\'' if !in_double => {
                    if token_start.is_none() {
                        token_start = Some(i);
                    }
                    in_single = !in_single;
                    buf.push(c);
                }
                '"' if !in_single => {
                    if token_start.is_none() {
                        token_start = Some(i);
                    }
                    in_double = !in_double;
                    buf.push(c);
                }
                ' ' | '\t' if !in_single && !in_double => {
                    if !buf.is_empty() {
                        tokens.push((token_start.unwrap_or(i), std::mem::take(&mut buf)));
                        token_start = None;
                    }
                }
                _ => {
                    if token_start.is_none() {
                        token_start = Some(i);
                    }
                    buf.push(c);
                }
            }
        }
        if !buf.is_empty() {
            tokens.push((token_start.unwrap_or(0), buf));
        }

        if tokens.is_empty() {
            // Empty line - show all commands
            let list_guard = self.path_cmds.read().unwrap_or_else(|p| p.into_inner());
            let candidates: Vec<Pair> = list_guard
                .iter()
                .map(|cmd| Pair {
                    display: cmd.clone(),
                    replacement: cmd.clone(),
                })
                .collect();
            return Ok((0, candidates));
        }

        let ends_with_space = line_to_cursor.ends_with(' ') || line_to_cursor.ends_with('\t');
        let (current_start, current_raw) = if ends_with_space {
            (pos, String::new())
        } else {
            tokens.last().cloned().unwrap_or((pos, String::new()))
        };

        let is_first_word = tokens.len() == 1 && !ends_with_space && current_start == 0;

        if is_first_word {
            // Complete command name
            let list_guard = self.path_cmds.read().unwrap_or_else(|p| p.into_inner());
            let candidates: Vec<Pair> = list_guard
                .iter()
                .filter(|cmd| cmd.starts_with(&current_raw.to_lowercase()))
                .map(|cmd| Pair {
                    display: cmd.clone(),
                    replacement: cmd.clone(),
                })
                .collect();
            Ok((current_start, candidates))
        } else {
            // Complete path
            let quote = current_raw.chars().next().filter(|c| *c == '"' || *c == '\'');
            let partial = quote
                .map(|q| current_raw.trim_start_matches(q).to_string())
                .unwrap_or_else(|| current_raw.clone());

            let mut start = current_start;
            if quote.is_some() {
                // Keep the opening quote as part of the prefix.
                start = start.saturating_add(1);
            }

            let mut candidates = self.complete_path(&partial);
            for cand in &mut candidates {
                if quote.is_none() && cand.replacement.contains(' ') {
                    cand.replacement = format!("\"{}\"", cand.replacement);
                }
            }

            Ok((start, candidates))
        }
    }
}

impl Highlighter for TitanHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[90m{}\x1b[0m", hint))  // Gray color
    }
}

impl Hinter for TitanHelper {
    type Hint = String;
}

impl Validator for TitanHelper {}

impl Helper for TitanHelper {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_completion() {
        let helper = TitanHelper::new(std::env::current_dir().unwrap());
        let (_start, candidates) = helper.complete("c", 1, &Context::new(&rustyline::history::DefaultHistory::new())).unwrap();
        assert!(candidates.iter().any(|p| p.replacement == "cd"));
        assert!(candidates.iter().any(|p| p.replacement == "cat"));
        assert!(candidates.iter().any(|p| p.replacement == "clear"));
    }
}
