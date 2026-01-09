//! Command parser - bash-like parsing with full operator support
//!
//! Supports:
//! - Simple commands: `ls -la`
//! - Pipelines: `ls | grep foo | head`
//! - And/Or: `cmd1 && cmd2`, `cmd1 || cmd2`
//! - Redirects: `echo hi > file.txt`, `cat < input.txt`
//! - Background: `cmd &`
//!
//! Operator precedence (low to high):
//! 1. `||` (or)
//! 2. `&&` (and)
//! 3. `|` (pipe)
//! 4. `>`, `>>`, `<` (redirect)
//! 5. simple command

use anyhow::{bail, Result};

/// Redirect mode for file I/O
#[derive(Debug, Clone, PartialEq)]
pub enum RedirectMode {
    /// `>` - overwrite file
    Overwrite,
    /// `>>` - append to file
    Append,
    /// `<` - read from file
    Input,
    /// `2>` - stderr overwrite
    StderrOverwrite,
    /// `2>>` - stderr append
    StderrAppend,
    /// `2>&1` or `|&` - merge stderr into stdout
    MergeStderrToStdout,
}

/// Quoting mode for parts of a word
#[derive(Debug, Clone, PartialEq)]
pub enum QuoteMode {
    None,
    Single,
    Double,
}

/// A part of a word with its quoting mode
#[derive(Debug, Clone, PartialEq)]
pub struct WordPart {
    pub text: String,
    pub quote: QuoteMode,
}

/// A shell word, possibly composed of multiple quoted/unquoted parts
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub parts: Vec<WordPart>,
}

impl Word {
    pub fn from_str(s: &str) -> Self {
        Self {
            parts: vec![WordPart {
                text: s.to_string(),
                quote: QuoteMode::None,
            }],
        }
    }
}

impl From<&str> for Word {
    fn from(value: &str) -> Self {
        Word::from_str(value)
    }
}

impl From<String> for Word {
    fn from(value: String) -> Self {
        Word::from_str(&value)
    }
}

/// Parsed command AST
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Empty command (no-op)
    Empty,
    /// Simple command with arguments: `ls -la`
    Simple(Vec<Word>),
    /// Sequence: `cmd1; cmd2; cmd3`
    Sequence(Vec<Command>),
    /// Pipeline: `cmd1 | cmd2 | cmd3`
    Pipeline(Vec<Command>),
    /// And: `cmd1 && cmd2` (run cmd2 only if cmd1 succeeds)
    And(Box<Command>, Box<Command>),
    /// Or: `cmd1 || cmd2` (run cmd2 only if cmd1 fails)
    Or(Box<Command>, Box<Command>),
    /// Redirect: `cmd > file` or `cmd >> file` or `cmd < file`
    Redirect {
        cmd: Box<Command>,
        target: Word,
        mode: RedirectMode,
    },
    /// Background: `cmd &`
    Background(Box<Command>),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(Word),
    Pipe,
    PipeAnd,
    AndIf,
    OrIf,
    RedirectOut,
    RedirectOutAppend,
    RedirectErrOut,
    RedirectErrOutAppend,
    RedirectIn,
    Ampersand,
    Semicolon,
}

/// Check if command needs shell features (pipes, redirects, etc.)
/// Note: This is kept for backward compatibility but the new AST-based
/// execution handles these internally.
pub fn needs_shell_features(cmd: &str) -> bool {
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
            '>' | '<' => return true,
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

/// Split a command string into argv-style arguments.
///
/// - Respects single/double quotes
/// - Does not treat `\\` as a general escape (so `C:\\Users\\x` works)
pub fn split_args(input: &str) -> Vec<String> {
    tokenize(input)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| match t {
            Token::Word(w) => Some(
                w.parts
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        })
        .collect()
}

/// Parse a command line into an AST.
pub fn parse(input: &str) -> Result<Command> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(Command::Empty);
    }

    let mut parser = Parser { tokens, pos: 0 };
    let mut cmd = parser.parse_sequence()?;

    // Background operator must be at the end: `cmd &`
    if parser.consume(Token::Ampersand) {
        if !parser.is_eof() {
            bail!("'&' must appear at end of command");
        }
        cmd = Command::Background(Box::new(cmd));
    }

    if !parser.is_eof() {
        bail!("Unexpected token: {:?}", parser.peek());
    }

    Ok(cmd)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn is_eof(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn consume(&mut self, expected: Token) -> bool {
        if self.peek() == Some(&expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_word(&mut self) -> Result<Word> {
        match self.next() {
            Some(Token::Word(w)) => Ok(w),
            other => bail!("Expected word, got: {:?}", other),
        }
    }

    fn parse_sequence(&mut self) -> Result<Command> {
        let mut parts = Vec::new();
        loop {
            let cmd = self.parse_or()?;
            parts.push(cmd);
            if self.consume(Token::Semicolon) {
                if self.is_eof() {
                    break;
                }
                continue;
            }
            break;
        }

        if parts.len() == 1 {
            Ok(parts.remove(0))
        } else {
            Ok(Command::Sequence(parts))
        }
    }

    fn parse_or(&mut self) -> Result<Command> {
        let mut left = self.parse_and()?;
        while self.consume(Token::OrIf) {
            let right = self.parse_and()?;
            left = Command::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Command> {
        let mut left = self.parse_pipeline()?;
        while self.consume(Token::AndIf) {
            let right = self.parse_pipeline()?;
            left = Command::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_pipeline(&mut self) -> Result<Command> {
        let mut cmds = vec![self.parse_redirect()?];
        loop {
            if self.consume(Token::Pipe) {
                cmds.push(self.parse_redirect()?);
            } else if self.consume(Token::PipeAnd) {
                if let Some(prev) = cmds.pop() {
                    cmds.push(Command::Redirect {
                        cmd: Box::new(prev),
                        target: Word::from_str("&1"),
                        mode: RedirectMode::MergeStderrToStdout,
                    });
                }
                cmds.push(self.parse_redirect()?);
            } else {
                break;
            }
        }
        if cmds.len() == 1 {
            Ok(cmds.remove(0))
        } else {
            Ok(Command::Pipeline(cmds))
        }
    }

    fn parse_redirect(&mut self) -> Result<Command> {
        let mut cmd = self.parse_simple()?;

        loop {
            let mode = match self.peek() {
                Some(Token::RedirectOut) => Some(RedirectMode::Overwrite),
                Some(Token::RedirectOutAppend) => Some(RedirectMode::Append),
                Some(Token::RedirectErrOut) => Some(RedirectMode::StderrOverwrite),
                Some(Token::RedirectErrOutAppend) => Some(RedirectMode::StderrAppend),
                Some(Token::RedirectIn) => Some(RedirectMode::Input),
                _ => None,
            };

            let Some(mode) = mode else { break };
            self.next(); // consume redirect operator

            let target = self.expect_word()?;

            let mode = if matches!(mode, RedirectMode::StderrOverwrite | RedirectMode::StderrAppend)
                && target.parts.len() == 1
                && target.parts[0].quote == QuoteMode::None
                && target.parts[0].text == "&1"
            {
                RedirectMode::MergeStderrToStdout
            } else {
                mode
            };

            cmd = Command::Redirect {
                cmd: Box::new(cmd),
                target,
                mode,
            };
        }

        Ok(cmd)
    }

    fn parse_simple(&mut self) -> Result<Command> {
        let mut parts: Vec<Word> = Vec::new();

        while let Some(Token::Word(_)) = self.peek() {
            parts.push(self.expect_word()?);
        }

        if parts.is_empty() {
            bail!("Expected command")
        } else {
            Ok(Command::Simple(parts))
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    let mut parts: Vec<WordPart> = Vec::new();
    let mut mode = QuoteMode::None;

    let chars: Vec<char> = input.chars().collect();
    let mut i = 0usize;

    fn push_part(mode: QuoteMode, buf: &mut String, parts: &mut Vec<WordPart>) {
        if !buf.is_empty() {
            parts.push(WordPart {
                text: std::mem::take(buf),
                quote: mode,
            });
        }
    }

    fn finish_word(tokens: &mut Vec<Token>, buf: &mut String, parts: &mut Vec<WordPart>) {
        push_part(QuoteMode::None, buf, parts);
        if !parts.is_empty() {
            tokens.push(Token::Word(Word {
                parts: std::mem::take(parts),
            }));
        }
    }

    while i < chars.len() {
        let ch = chars[i];

        match ch {
            '\'' if mode != QuoteMode::Double => {
                if mode == QuoteMode::Single {
                    push_part(QuoteMode::Single, &mut buf, &mut parts);
                    mode = QuoteMode::None;
                } else {
                    push_part(QuoteMode::None, &mut buf, &mut parts);
                    mode = QuoteMode::Single;
                }
            }
            '"' if mode != QuoteMode::Single => {
                if mode == QuoteMode::Double {
                    push_part(QuoteMode::Double, &mut buf, &mut parts);
                    mode = QuoteMode::None;
                } else {
                    push_part(QuoteMode::None, &mut buf, &mut parts);
                    mode = QuoteMode::Double;
                }
            }
            // Allow escaping quotes via backslash inside double quotes, mirror previous behavior otherwise
            '\\' if i + 1 < chars.len() => {
                let next = chars[i + 1];
                if mode == QuoteMode::Double && next == '"' {
                    buf.push(next);
                    i += 1;
                } else {
                    buf.push(ch);
                }
            }
            c if mode == QuoteMode::Single || mode == QuoteMode::Double => {
                buf.push(c);
            }
            c if c.is_whitespace() => {
                finish_word(&mut tokens, &mut buf, &mut parts);
            }
            '2' if mode == QuoteMode::None && i + 1 < chars.len() && chars[i + 1] == '>' => {
                finish_word(&mut tokens, &mut buf, &mut parts);
                if i + 2 < chars.len() && chars[i + 2] == '>' {
                    tokens.push(Token::RedirectErrOutAppend);
                    i += 2;
                } else {
                    tokens.push(Token::RedirectErrOut);
                    i += 1;
                }
            }
            '|' => {
                finish_word(&mut tokens, &mut buf, &mut parts);
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token::OrIf);
                    i += 1;
                } else if i + 1 < chars.len() && chars[i + 1] == '&' {
                    tokens.push(Token::PipeAnd);
                    i += 1;
                } else {
                    tokens.push(Token::Pipe);
                }
            }
            '&' => {
                finish_word(&mut tokens, &mut buf, &mut parts);
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    tokens.push(Token::AndIf);
                    i += 1;
                } else {
                    tokens.push(Token::Ampersand);
                }
            }
            '>' => {
                finish_word(&mut tokens, &mut buf, &mut parts);
                if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token::RedirectOutAppend);
                    i += 1;
                } else {
                    tokens.push(Token::RedirectOut);
                }
            }
            '<' => {
                finish_word(&mut tokens, &mut buf, &mut parts);
                tokens.push(Token::RedirectIn);
            }
            ';' => {
                finish_word(&mut tokens, &mut buf, &mut parts);
                tokens.push(Token::Semicolon);
            }
            other => {
                buf.push(other);
            }
        }

        i += 1;
    }

    if mode == QuoteMode::Single || mode == QuoteMode::Double {
        bail!("Unclosed quote");
    }

    finish_word(&mut tokens, &mut buf, &mut parts);

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_split() {
        assert_eq!(split_args("ls -la"), vec!["ls", "-la"]);
    }

    #[test]
    fn test_quoted_args() {
        assert_eq!(split_args(r#"echo "hello world""#), vec!["echo", "hello world"]);
        assert_eq!(split_args("echo 'hello world'"), vec!["echo", "hello world"]);
    }

    #[test]
    fn test_mixed_quotes() {
        assert_eq!(
            split_args(r#"echo "hello 'world'""#),
            vec!["echo", "hello 'world'"]
        );
        assert_eq!(
            split_args("echo 'hello \"world\"'"),
            vec!["echo", "hello \"world\""]
        );
    }

    #[test]
    fn test_parse_pipeline() {
        assert_eq!(
            parse("ls | findstr src").unwrap(),
            Command::Pipeline(vec![
                Command::Simple(vec!["ls".into()]),
                Command::Simple(vec!["findstr".into(), "src".into()])
            ])
        );
    }

    #[test]
    fn test_parse_and_then() {
        assert_eq!(
            parse("cd .. && pwd").unwrap(),
            Command::And(
                Box::new(Command::Simple(vec!["cd".into(), "..".into()])),
                Box::new(Command::Simple(vec!["pwd".into()]))
            )
        );
    }

    #[test]
    fn test_parse_sequence_with_semicolon() {
        assert_eq!(
            parse("echo a; echo b").unwrap(),
            Command::Sequence(vec![
                Command::Simple(vec!["echo".into(), "a".into()]),
                Command::Simple(vec!["echo".into(), "b".into()])
            ])
        );
    }
}
