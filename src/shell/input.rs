//! Crossterm-based input layer for TITAN Bash
//!
//! This module provides non-blocking input with paste detection.

use std::io::{self, Write, Stdout};
use std::time::{Duration, Instant};
use std::path::PathBuf;

use crossterm::{
    cursor::{self, MoveToColumn},
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, poll, read},
    style::Print,
    terminal::{self, Clear, ClearType},
    execute,
};

use super::completer::TitanHelper;
use super::parser;
use unicode_width::UnicodeWidthChar;

const PASTE_THRESHOLD: Duration = Duration::from_millis(50);
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";

fn enable_bracketed_paste(stdout: &mut Stdout) {
    // Best-effort: on terminals that support bracketed paste, this disables the
    // "multi-line paste warning" UX and wraps pasted content in ESC[200~/ESC[201~.
    let _ = stdout.write_all(b"\x1b[?2004h");
    let _ = stdout.flush();
}

fn disable_bracketed_paste(stdout: &mut Stdout) {
    let _ = stdout.write_all(b"\x1b[?2004l");
    let _ = stdout.flush();
}

#[derive(Debug)]
pub enum InputResult {
    Line(String),
    Paste(Vec<String>),
    Interrupt,
    Eof,
}

#[derive(Debug, Default)]
struct LineBuffer {
    text: String,
    cursor: usize,
}

impl LineBuffer {
    fn new() -> Self { Self::default() }
    fn clear(&mut self) { self.text.clear(); self.cursor = 0; }
    
    fn insert(&mut self, c: char) {
        if self.cursor == self.text.chars().count() {
            self.text.push(c);
        } else {
            let byte_pos = self.text.char_indices()
                .nth(self.cursor).map(|(i, _)| i).unwrap_or(self.text.len());
            self.text.insert(byte_pos, c);
        }
        self.cursor += 1;
    }
    
    #[allow(dead_code)]
    fn insert_str(&mut self, s: &str) { for c in s.chars() { self.insert(c); } }
    
    fn backspace(&mut self) -> bool {
        if self.cursor == 0 { return false; }
        self.cursor -= 1;
        let byte_pos = self.text.char_indices()
            .nth(self.cursor).map(|(i, _)| i).unwrap_or(self.text.len());
        self.text.remove(byte_pos);
        true
    }
    
    fn delete(&mut self) -> bool {
        if self.cursor >= self.text.chars().count() { return false; }
        let byte_pos = self.text.char_indices()
            .nth(self.cursor).map(|(i, _)| i).unwrap_or(self.text.len());
        self.text.remove(byte_pos);
        true
    }
    
    fn move_left(&mut self) -> bool {
        if self.cursor > 0 { self.cursor -= 1; true } else { false }
    }
    
    fn move_right(&mut self) -> bool {
        if self.cursor < self.text.chars().count() { self.cursor += 1; true } else { false }
    }
    
    fn move_home(&mut self) { self.cursor = 0; }
    fn move_end(&mut self) { self.cursor = self.text.chars().count(); }
    
    fn skip_left_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor > 0 && chars[self.cursor - 1].is_whitespace() { self.cursor -= 1; }
        while self.cursor > 0 && !chars[self.cursor - 1].is_whitespace() { self.cursor -= 1; }
    }

    fn skip_right_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor < self.text.chars().count() - 1 && chars[self.cursor + 1].is_whitespace() { self.cursor += 1; }
        while self.cursor < self.text.chars().count() - 1 && !chars[self.cursor + 1].is_whitespace() { self.cursor += 1; }
        if self.cursor < self.text.chars().count() { self.cursor += 1; }
    }

    fn kill_line(&mut self) {
        let byte_pos = self.text.char_indices()
            .nth(self.cursor).map(|(i, _)| i).unwrap_or(self.text.len());
        self.text.truncate(byte_pos);
    }
    
    fn delete_word(&mut self) -> bool {
        if self.cursor == 0 { return false; }
        let chars: Vec<char> = self.text.chars().collect();
        let mut pos = self.cursor;
        while pos > 0 && chars[pos - 1].is_whitespace() { pos -= 1; }
        while pos > 0 && !chars[pos - 1].is_whitespace() { pos -= 1; }
        let start_byte = self.text.char_indices().nth(pos).map(|(i, _)| i).unwrap_or(0);
        let end_byte = self.text.char_indices().nth(self.cursor).map(|(i, _)| i).unwrap_or(self.text.len());
        self.text.replace_range(start_byte..end_byte, "");
        self.cursor = pos;
        true
    }
    
    fn as_str(&self) -> &str { &self.text }
    fn set_text(&mut self, text: String) { self.text = text; self.cursor = self.text.chars().count(); }
    fn len(&self) -> usize { self.text.chars().count() }
}

struct History {
    entries: Vec<String>,
    position: Option<usize>,
    saved_line: String,
}

impl History {
    fn new() -> Self { Self { entries: Vec::new(), position: None, saved_line: String::new() } }
    
    fn add(&mut self, line: String) {
        if line.is_empty() { return; }
        if self.entries.last().map(|s| s.as_str()) == Some(&line) { return; }
        self.entries.push(line);
    }
    
    fn up(&mut self, current: &str) -> Option<&str> {
        if self.entries.is_empty() { return None; }
        match self.position {
            None => {
                self.saved_line = current.to_string();
                self.position = Some(self.entries.len() - 1);
                Some(&self.entries[self.entries.len() - 1])
            }
            Some(pos) => {
                if pos > 0 { self.position = Some(pos - 1); Some(&self.entries[pos - 1]) }
                else { Some(&self.entries[0]) }
            }
        }
    }
    
    fn down(&mut self) -> Option<&str> {
        match self.position {
            None => None,
            Some(pos) => {
                if pos + 1 < self.entries.len() { self.position = Some(pos + 1); Some(&self.entries[pos + 1]) }
                else { self.position = None; Some(&self.saved_line) }
            }
        }
    }
    
    fn reset_position(&mut self) { self.position = None; self.saved_line.clear(); }
    fn entries(&self) -> &[String] { &self.entries }
    fn load(&mut self, entries: Vec<String>) { self.entries = entries; }

    fn reverse_search(&self, query: &str, from: Option<usize>) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }
        let mut i = from.unwrap_or_else(|| self.entries.len().saturating_sub(1));
        loop {
            if self.entries.get(i)?.contains(query) {
                return Some(i);
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
        None
    }
}

struct PasteDetector {
    last_input: Instant,
    in_paste: bool,
}

impl PasteDetector {
    fn new() -> Self { Self { last_input: Instant::now(), in_paste: false } }
    
    fn check(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_input);
        self.last_input = now;
        if elapsed < PASTE_THRESHOLD { self.in_paste = true; }
        self.in_paste
    }
    
    fn has_pending(&self) -> bool { poll(Duration::from_millis(10)).unwrap_or(false) }
    fn end_paste(&mut self) { self.in_paste = false; }
}

pub struct CrosstermInput {
    buffer: LineBuffer,
    history: History,
    paste_detector: PasteDetector,
    helper: TitanHelper,
    prompt: String,
    prompt_len: usize,
}
impl CrosstermInput {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            buffer: LineBuffer::new(),
            history: History::new(),
            paste_detector: PasteDetector::new(),
            helper: TitanHelper::new(cwd),
            prompt: String::new(),
            prompt_len: 0,
        }
    }
    
    pub fn set_cwd(&mut self, cwd: PathBuf) { self.helper.set_cwd(cwd); }
    pub fn add_history(&mut self, line: String) { self.history.add(line); }
    pub fn history_entries(&self) -> &[String] { self.history.entries() }
    pub fn load_history(&mut self, entries: Vec<String>) { self.history.load(entries); }
    
    pub fn read_line(&mut self, prompt: &str) -> io::Result<InputResult> {
        let mut stdout = io::stdout();
        self.prompt = prompt.to_string();
        self.prompt_len = visible_width(&self.prompt);
        print!("{}", self.prompt);
        stdout.flush()?;
        self.buffer.clear();
        self.history.reset_position();
        enable_bracketed_paste(&mut stdout);
        terminal::enable_raw_mode()?;
        let result = self.input_loop(&mut stdout);
        let _ = terminal::disable_raw_mode();
        disable_bracketed_paste(&mut stdout);
        execute!(stdout, Print("\r\n"))?;
        result
    }
    fn input_loop(&mut self, stdout: &mut Stdout) -> io::Result<InputResult> {
        let mut paste_buffer: Vec<String> = Vec::new();
        let mut in_paste_collection = false;
        let mut in_bracketed_paste = false;
        let mut vt_seq = String::new();

        #[derive(Debug)]
        struct SearchState {
            query: String,
            index: Option<usize>,
            saved_text: String,
            saved_cursor: usize,
        }

        let mut search: Option<SearchState> = None;

        loop {
            if poll(Duration::from_millis(100))? {
                if let Event::Key(key) = read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    let is_paste = self.paste_detector.check();
                    let has_pending = self.paste_detector.has_pending();

                    if let Some(state) = search.as_mut() {
                        match key.code {
                            KeyCode::Esc => {
                                let saved = state.saved_text.clone();
                                let saved_cursor = state.saved_cursor;
                                search = None;
                                self.buffer.set_text(saved);
                                self.buffer.cursor = saved_cursor;
                                self.redraw_line(stdout)?;
                            }
                            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                let saved = state.saved_text.clone();
                                let saved_cursor = state.saved_cursor;
                                search = None;
                                self.buffer.set_text(saved);
                                self.buffer.cursor = saved_cursor;
                                self.redraw_line(stdout)?;
                            }
                            KeyCode::Enter => {
                                let selection = state
                                    .index
                                    .and_then(|i| state.query.as_str().is_empty().then_some(i).or(Some(i)))
                                    .and_then(|i| self.history.entries.get(i))
                                    .cloned()
                                    .unwrap_or_else(|| state.saved_text.clone());
                                search = None;
                                self.buffer.set_text(selection);
                                self.redraw_line(stdout)?;
                            }
                            KeyCode::Backspace => {
                                state.query.pop();
                                state.index = self.history.reverse_search(&state.query, None);
                                let matched = state.index.and_then(|i| self.history.entries.get(i)).map(|s| s.as_str()).unwrap_or("");
                                self.redraw_search(stdout, &state.query, matched)?;
                            }
                            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if let Some(i) = state.index {
                                    if i > 0 {
                                        state.index = self.history.reverse_search(&state.query, Some(i - 1));
                                    }
                                } else {
                                    state.index = self.history.reverse_search(&state.query, None);
                                }
                                let matched = state.index.and_then(|i| self.history.entries.get(i)).map(|s| s.as_str()).unwrap_or("");
                                self.redraw_search(stdout, &state.query, matched)?;
                            }
                            KeyCode::Char(c) => {
                                if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) {
                                    state.query.push(c);
                                    state.index = self.history.reverse_search(&state.query, None);
                                    let matched = state.index.and_then(|i| self.history.entries.get(i)).map(|s| s.as_str()).unwrap_or("");
                                    self.redraw_search(stdout, &state.query, matched)?;
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Always allow Ctrl+C to interrupt (even during bracketed paste parsing).
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        self.paste_detector.end_paste();
                        vt_seq.clear();
                        return Ok(InputResult::Interrupt);
                    }

                    // Bracketed paste support (ESC[200~ ... ESC[201~).
                    // This avoids terminal-host multi-line paste warnings and lets us treat a paste
                    // as "insert into buffer; user presses Enter to execute".
                    if key.code == KeyCode::Esc {
                        vt_seq.clear();
                        vt_seq.push('\x1b');
                        continue;
                    }

                    if !vt_seq.is_empty() {
                        if let KeyCode::Char(c) = key.code {
                            vt_seq.push(c);
                            let expected = if in_bracketed_paste {
                                BRACKETED_PASTE_END
                            } else {
                                BRACKETED_PASTE_START
                            };
                            if expected.starts_with(&vt_seq) {
                                if vt_seq == expected {
                                    if in_bracketed_paste {
                                        let line = self.buffer.text.clone();
                                        if !line.is_empty() {
                                            paste_buffer.push(line);
                                        }
                                        if !paste_buffer.is_empty() {
                                            let joined =
                                                join_pasted_commands(std::mem::take(&mut paste_buffer));
                                            self.buffer.set_text(joined);
                                            self.redraw_line(stdout)?;
                                        }
                                        in_bracketed_paste = false;
                                        self.paste_detector.end_paste();
                                    } else {
                                        in_bracketed_paste = true;
                                        paste_buffer.clear();
                                    }
                                    vt_seq.clear();
                                }
                                continue;
                            }
                        }
                        // Not a bracketed paste sequence; drop the leading ESC and handle this key normally.
                        vt_seq.clear();
                    }

                    if in_bracketed_paste {
                        match key.code {
                            KeyCode::Enter => {
                                let line = self.buffer.text.clone();
                                paste_buffer.push(line);
                                self.buffer.clear();
                                execute!(stdout, Print("\r\n"))?;
                            }
                            KeyCode::Char(c) => {
                                self.buffer.insert(c);
                                if self.buffer.cursor == self.buffer.len() {
                                    execute!(stdout, Print(c))?;
                                } else {
                                    self.redraw_line(stdout)?;
                                }
                            }
                            KeyCode::Backspace => {
                                if self.buffer.backspace() { self.redraw_line(stdout)?; }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.paste_detector.end_paste();
                            return Ok(InputResult::Interrupt);
                        }
                        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let saved_text = self.buffer.text.clone();
                            let saved_cursor = self.buffer.cursor;
                            let mut state = SearchState {
                                query: String::new(),
                                index: None,
                                saved_text,
                                saved_cursor,
                            };
                            state.index = self.history.reverse_search("", None);
                            let matched = state.index.and_then(|i| self.history.entries.get(i)).map(|s| s.as_str()).unwrap_or("");
                            self.redraw_search(stdout, &state.query, matched)?;
                            search = Some(state);
                        }
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if self.buffer.text.is_empty() && paste_buffer.is_empty() {
                                self.paste_detector.end_paste();
                                return Ok(InputResult::Eof);
                            }
                        }
                        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            execute!(stdout, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
                            self.redraw_line(stdout)?;
                        }
                        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.buffer.kill_line();
                            self.redraw_line(stdout)?;
                        }
                        KeyCode::Char('w') | KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if self.buffer.delete_word() { self.redraw_line(stdout)?; }
                        }
                        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.buffer.move_home();
                            self.update_cursor(stdout)?;
                        }
                        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.buffer.move_end();
                            self.update_cursor(stdout)?;
                        }
                        KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.buffer.skip_left_word();
                            self.update_cursor(stdout)?;
                        }
                        KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.buffer.skip_right_word();
                            self.update_cursor(stdout)?;
                        }
                        KeyCode::Enter => {
                            let line = self.buffer.text.clone();
                            if in_paste_collection || (is_paste && has_pending) {
                                paste_buffer.push(line.clone());
                                self.buffer.clear();
                                if has_pending {
                                    in_paste_collection = true;
                                    execute!(stdout, Print("\r\n"))?;
                                    continue;
                                }
                            }
                            if !paste_buffer.is_empty() {
                                if !line.is_empty() && paste_buffer.last().map(|s| s.as_str()) != Some(&line) {
                                    paste_buffer.push(line);
                                }
                                let joined = join_pasted_commands(paste_buffer);
                                self.buffer.set_text(joined);
                                self.redraw_line(stdout)?;
                                in_paste_collection = false;
                                self.paste_detector.end_paste();
                                paste_buffer = Vec::new();
                                continue;
                            }
                            self.paste_detector.end_paste();
                            return Ok(InputResult::Line(line));
                        }
                        KeyCode::Tab => { self.handle_completion(stdout)?; }
                        KeyCode::Backspace => {
                            if self.buffer.backspace() { self.redraw_line(stdout)?; }
                        }
                        KeyCode::Delete => {
                            if self.buffer.delete() { self.redraw_line(stdout)?; }
                        }
                        KeyCode::Left => {
                            if self.buffer.move_left() { self.update_cursor(stdout)?; }
                        }
                        KeyCode::Right => {
                            if self.buffer.move_right() { self.update_cursor(stdout)?; }
                        }
                        KeyCode::Up => {
                            if let Some(hist) = self.history.up(self.buffer.as_str()) {
                                self.buffer.set_text(hist.to_string());
                                self.redraw_line(stdout)?;
                            }
                        }
                        KeyCode::Down => {
                            if let Some(hist) = self.history.down() {
                                self.buffer.set_text(hist.to_string());
                                self.redraw_line(stdout)?;
                            }
                        }
                        KeyCode::Home => {
                            self.buffer.move_home();
                            self.update_cursor(stdout)?;
                        }
                        KeyCode::End => {
                            self.buffer.move_end();
                            self.update_cursor(stdout)?;
                        }
                        KeyCode::Char(c) => {
                            self.buffer.insert(c);
                            if self.buffer.cursor == self.buffer.len() {
                                execute!(stdout, Print(c))?;
                            } else {
                                self.redraw_line(stdout)?;
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                if in_paste_collection {
                    let line = self.buffer.text.clone();
                    if !line.is_empty() { paste_buffer.push(line); }
                    if !paste_buffer.is_empty() {
                        let joined = join_pasted_commands(paste_buffer);
                        self.buffer.set_text(joined);
                        self.redraw_line(stdout)?;
                        in_paste_collection = false;
                        self.paste_detector.end_paste();
                        paste_buffer = Vec::new();
                        continue;
                    }
                }
                self.paste_detector.end_paste();
            }
        }
    }
    fn redraw_line(&self, stdout: &mut Stdout) -> io::Result<()> {
        execute!(
            stdout,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            Print(&self.prompt),
            Print(&self.buffer.text)
        )?;
        self.update_cursor(stdout)
    }

    fn redraw_search(&self, stdout: &mut Stdout, query: &str, matched: &str) -> io::Result<()> {
        let line = format!("(reverse-i-search)`{}`: {}", query, matched);
        execute!(
            stdout,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            Print(line)
        )?;
        stdout.flush()
    }
    
    fn update_cursor(&self, stdout: &mut Stdout) -> io::Result<()> {
        let byte_pos = self.text_byte_pos(self.buffer.cursor);
        let col = self.prompt_len + visible_width(&self.buffer.text[..byte_pos]);
        execute!(stdout, MoveToColumn(clamp_u16(col)))?;
        stdout.flush()
    }
    
    fn handle_completion(&mut self, stdout: &mut Stdout) -> io::Result<()> {
        use rustyline::completion::Completer;
        let line = self.buffer.as_str();
        let pos = self.text_byte_pos(self.buffer.cursor);
        let history = rustyline::history::DefaultHistory::new();
        let ctx = rustyline::Context::new(&history);
        
        if let Ok((start, candidates)) = self.helper.complete(line, pos, &ctx) {
            match candidates.len() {
                0 => {}
                1 => {
                    let completion = &candidates[0].replacement;
                    let prefix = &line[..start];
                    let suffix = &line[pos..];
                    let new_text = format!("{}{}{}", prefix, completion, suffix);
                    let new_cursor = prefix.chars().count() + completion.chars().count();
                    self.buffer.set_text(new_text);
                    self.buffer.cursor = new_cursor;
                    self.redraw_line(stdout)?;
                }
                _ => {
                    let mut out = String::new();
                    out.push_str("\r\n");
                    for candidate in &candidates {
                        out.push_str(&candidate.display);
                        out.push_str("  ");
                    }
                    out.push_str("\r\n");
                    execute!(stdout, Print(out))?;
                    let common = Self::common_prefix(&candidates);
                    if common.len() > (pos - start) {
                        let prefix = &line[..start];
                        let suffix = &line[pos..];
                        let new_text = format!("{}{}{}", prefix, common, suffix);
                        let new_cursor = prefix.chars().count() + common.chars().count();
                        self.buffer.set_text(new_text);
                        self.buffer.cursor = new_cursor;
                    }
                    self.redraw_line(stdout)?;
                }
            }
        }
        Ok(())
    }
    
    fn text_byte_pos(&self, char_pos: usize) -> usize {
        self.buffer.text.char_indices().nth(char_pos).map(|(i, _)| i).unwrap_or(self.buffer.text.len())
    }
    
    fn common_prefix(candidates: &[rustyline::completion::Pair]) -> String {
        if candidates.is_empty() { return String::new(); }
        let first = &candidates[0].replacement;
        let mut prefix_len = first.chars().count();
        for candidate in &candidates[1..] {
            let common = first.chars().zip(candidate.replacement.chars())
                .take_while(|(a, b)| a == b).count();
            prefix_len = prefix_len.min(common);
        }
        first.chars().take(prefix_len).collect()
    }
}

fn clamp_u16(value: usize) -> u16 {
    value.min(u16::MAX as usize) as u16
}

fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            let _ = chars.next(); // '['
            while let Some(seq_char) = chars.next() {
                if matches!(seq_char as u32, 0x40..=0x7E) {
                    break;
                }
            }
            continue;
        }
        width += UnicodeWidthChar::width(c).unwrap_or(0);
    }
    width
}

fn looks_like_windows_prompt_path(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    s.contains(":\\") || s.contains(":/") || s.starts_with("\\\\")
}

fn ends_with_windows_executable_ext(s: &str) -> bool {
    let s = s.trim_end().to_ascii_lowercase();
    s.ends_with(".exe")
        || s.ends_with(".com")
        || s.ends_with(".bat")
        || s.ends_with(".cmd")
        || s.ends_with(".ps1")
}

/// Strip common shell prompt prefixes from a pasted line.
///
/// This is intended for "paste transcript" UX (e.g. `PS C:\Users\me> reg ...`).
///
/// Returns `(command, stripped)` where `stripped` is true when a known prompt was removed.
pub fn strip_prompt_prefix(line: &str) -> (String, bool) {
    let s = line.trim_start();
    if s.is_empty() {
        return (String::new(), false);
    }

    // Docs often prefix commands with "$ ".
    if s.starts_with('$') && s[1..].chars().next().is_some_and(|c| c.is_whitespace()) {
        return (s[1..].trim_start().to_string(), true);
    }

    // Bash continuation prompt: "> <cmd>"
    if s.starts_with('>') && s[1..].chars().next().is_some_and(|c| c.is_whitespace()) {
        return (s[1..].trim_start().to_string(), true);
    }

    // PowerShell default prompt: "PS C:\Path> <cmd>" or sometimes "PS> <cmd>"
    if s.starts_with("PS") {
        if let Some(pos) = s.find('>') {
            let before = &s[..pos];
            let after = s[pos + 1..].trim_start();

            // "PS>" (no path) or "PS <path>"
            let valid = if before == "PS" {
                true
            } else {
                let path_part = before.strip_prefix("PS").unwrap_or(before).trim();
                looks_like_windows_prompt_path(path_part)
            };

            if valid {
                return (after.to_string(), true);
            }
        }
    }

    // TITAN Bash prompt: "titan C:\Path> <cmd>"
    if let Some(rest) = s.strip_prefix("titan") {
        if rest.chars().next().is_some_and(|c| c.is_whitespace()) {
            if let Some(pos) = s.find('>') {
                let before = &s[..pos];
                let after = s[pos + 1..].trim_start();
                let path_part = before.strip_prefix("titan").unwrap_or(before).trim();
                if looks_like_windows_prompt_path(path_part) {
                    return (after.to_string(), true);
                }
            }
        }
    }

    // cmd.exe prompt: "C:\Path> <cmd>"
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            if let Some(pos) = s.find('>') {
                let before = s[..pos].trim_end();
                let after = s[pos + 1..].trim_start();

                // Avoid mis-detecting absolute command invocations like:
                //   C:\Windows\System32\ipconfig.exe /all > out.txt
                // The cmd prompt prefix is a path, not a command + args; treat obvious "command-ish" prefixes as non-prompt.
                let looks_like_prompt = looks_like_windows_prompt_path(before)
                    && !ends_with_windows_executable_ext(before)
                    && !before.contains(" /");

                if looks_like_prompt {
                    return (after.to_string(), true);
                }
            }
        }
    }

    (line.to_string(), false)
}

/// Normalize lines from a multi-line paste.
///
/// If the paste looks like a terminal transcript (prompt-prefixed commands + output),
/// this returns only the command lines with prompt prefixes stripped.
pub fn normalize_pasted_lines(lines: Vec<String>) -> Vec<String> {
    let mut parsed: Vec<(bool, String)> = Vec::with_capacity(lines.len());
    let mut any_prompted_command = false;

    for line in lines {
        let (cmd, stripped) = strip_prompt_prefix(&line);
        let cmd = cmd.trim().to_string();
        if stripped && !cmd.is_empty() {
            any_prompted_command = true;
        }
        parsed.push((stripped, cmd));
    }

    let mut out = Vec::new();
    for (stripped, cmd) in parsed {
        if cmd.is_empty() {
            continue;
        }
        if any_prompted_command {
            if stripped {
                out.push(cmd);
            }
        } else {
            if cmd.starts_with('#') {
                continue;
            }
            out.push(cmd);
        }
    }
    out
}

fn merge_continued_pasted_commands(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut buffer = String::new();

    for line in lines {
        if buffer.is_empty() {
            buffer = line;
        } else {
            if parser::ends_with_line_continuation_backslash(&buffer) {
                let trimmed_len = buffer.trim_end().len();
                if trimmed_len > 0 {
                    buffer.truncate(trimmed_len - 1);
                }
                buffer.push_str(&line);
            } else {
                buffer.push('\n');
                buffer.push_str(&line);
            }
        }

        if parser::is_incomplete(&buffer) {
            continue;
        }

        let cmd = buffer.trim();
        if !cmd.is_empty() {
            out.push(cmd.to_string());
        }
        buffer.clear();
    }

    if !buffer.trim().is_empty() {
        out.push(buffer.trim().to_string());
    }

    out
}

fn join_pasted_commands(lines: Vec<String>) -> String {
    merge_continued_pasted_commands(normalize_pasted_lines(lines)).join("; ")
}

pub fn split_pasted_commands(input: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;
    
    for c in input.chars() {
        if escape_next { current.push(c); escape_next = false; continue; }
        match c {
            '\\' => { escape_next = true; current.push(c); }
            '\'' if !in_double_quote => { in_single_quote = !in_single_quote; current.push(c); }
            '"' if !in_single_quote => { in_double_quote = !in_double_quote; current.push(c); }
            '\n' if !in_single_quote && !in_double_quote => {
                let cmd = current.trim().to_string();
                if !cmd.is_empty() && !cmd.starts_with('#') { commands.push(cmd); }
                current.clear();
            }
            _ => { current.push(c); }
        }
    }
    let cmd = current.trim().to_string();
    if !cmd.is_empty() && !cmd.starts_with('#') { commands.push(cmd); }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_buffer_insert() {
        let mut buf = LineBuffer::new();
        buf.insert('h');
        buf.insert('i');
        assert_eq!(buf.as_str(), "hi");
        assert_eq!(buf.cursor, 2);
    }

    #[test]
    fn test_line_buffer_backspace() {
        let mut buf = LineBuffer::new();
        buf.insert_str("hello");
        buf.backspace();
        assert_eq!(buf.as_str(), "hell");
    }

    #[test]
    fn test_history_navigation() {
        let mut hist = History::new();
        hist.add("cmd1".to_string());
        hist.add("cmd2".to_string());
        hist.add("cmd3".to_string());
        assert_eq!(hist.up("current"), Some("cmd3"));
        assert_eq!(hist.up("current"), Some("cmd2"));
        assert_eq!(hist.down(), Some("cmd3"));
    }

    #[test]
    fn test_split_simple_commands() {
        let input = "echo line1\necho line2\necho line3";
        let commands = split_pasted_commands(input);
        assert_eq!(commands, vec!["echo line1", "echo line2", "echo line3"]);
    }

    #[test]
    fn test_split_with_quotes() {
        let input = "echo \"hello\nworld\"\necho done";
        let commands = split_pasted_commands(input);
        assert_eq!(commands, vec!["echo \"hello\nworld\"", "echo done"]);
    }

    #[test]
    fn test_strip_prompt_prefix_powershell() {
        let (cmd, stripped) = strip_prompt_prefix("PS C:\\Users\\me>   reg delete HKLM\\Foo /f");
        assert!(stripped);
        assert_eq!(cmd, "reg delete HKLM\\Foo /f");
    }

    #[test]
    fn test_strip_prompt_prefix_cmd() {
        let (cmd, stripped) = strip_prompt_prefix("C:\\Users\\me> dir");
        assert!(stripped);
        assert_eq!(cmd, "dir");
    }

    #[test]
    fn test_strip_prompt_prefix_titan() {
        let (cmd, stripped) = strip_prompt_prefix("titan C:\\Users\\me> sha256sum file.txt");
        assert!(stripped);
        assert_eq!(cmd, "sha256sum file.txt");
    }

    #[test]
    fn test_strip_prompt_prefix_dollar() {
        let (cmd, stripped) = strip_prompt_prefix("$ echo hi");
        assert!(stripped);
        assert_eq!(cmd, "echo hi");
    }

    #[test]
    fn test_strip_prompt_prefix_continuation_prompt() {
        let (cmd, stripped) = strip_prompt_prefix("> echo hi");
        assert!(stripped);
        assert_eq!(cmd, "echo hi");
    }

    #[test]
    fn test_strip_prompt_prefix_does_not_break_redirects() {
        let (cmd, stripped) = strip_prompt_prefix("echo hi > out.txt");   
        assert!(!stripped);
        assert_eq!(cmd, "echo hi > out.txt");
    }

    #[test]
    fn test_normalize_paste_transcript() {
        let lines = vec![
            "PS C:\\Users\\me> echo one".to_string(),
            "some output".to_string(),
            "PS C:\\Users\\me> echo two".to_string(),
        ];
        let cmds = normalize_pasted_lines(lines);
        assert_eq!(cmds, vec!["echo one", "echo two"]);
    }

    #[test]
    fn test_join_paste_with_continuation_lines() {
        let lines = vec![
            "$ echo one \\".to_string(),
            "> two".to_string(),
            "$ echo three".to_string(),
        ];
        let joined = join_pasted_commands(lines);
        assert_eq!(joined, "echo one two; echo three");
    }
}
