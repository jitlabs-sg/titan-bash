//! TITAN Bash - Modern shell for Windows
//!
//! Features:
//! - Path normalization (forward/backward slashes both work)
//! - Never freezes (async I/O)
//! - Fast startup
//! - Windows Terminal integration

pub mod shell;
pub mod task;
// pub mod tui;  // TODO: Phase 3
// pub mod compat;  // TODO: Phase 2

pub use shell::Shell;
