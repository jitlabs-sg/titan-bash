//! Cross-platform interrupt handling (Ctrl+C).
//!
//! On Windows, titanbash installs a console control handler so Ctrl+C does not
//! terminate the process while waiting on child processes. Instead, we expose a
//! simple flag that the REPL/builtins can poll.

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT,
        CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    static INSTALLED: OnceLock<()> = OnceLock::new();
    static CTRL_SEEN: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                CTRL_SEEN.store(true, Ordering::SeqCst);
                1
            }
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                crate::task::kill_registered_pids_best_effort();
                0
            }
            _ => 0,
        }
    }

    pub fn install() {
        INSTALLED.get_or_init(|| unsafe {
            // Install a handler so Ctrl+C doesn't terminate titanbash while
            // waiting on child processes.
            let _ = SetConsoleCtrlHandler(Some(handler), 1);
        });
    }

    pub fn seen() -> bool {
        CTRL_SEEN.load(Ordering::SeqCst)
    }

    pub fn mark_seen() {
        CTRL_SEEN.store(true, Ordering::SeqCst);
    }

    pub fn take() -> bool {
        CTRL_SEEN.swap(false, Ordering::SeqCst)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn install() {}
    pub fn seen() -> bool {
        false
    }
    pub fn mark_seen() {}
    pub fn take() -> bool {
        false
    }
}

pub use imp::{install, mark_seen, seen, take};
