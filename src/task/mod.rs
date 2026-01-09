//! Task management - background jobs

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use std::process::{Command, Stdio};
use anyhow::{bail, Context, Result};

pub type TaskId = u32;

/// Task status
#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Running,
    Completed(i32),
    Failed(String),
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Running => write!(f, "Running"),
            TaskStatus::Completed(code) => write!(f, "Done ({})", code),
            TaskStatus::Failed(msg) => write!(f, "Failed: {}", msg),
        }
    }
}

static REGISTERED_PIDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

fn pid_registry() -> &'static Mutex<HashSet<u32>> {
    REGISTERED_PIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Register a spawned child PID so it can be best-effort terminated during console close events.
pub fn register_pid(pid: u32) {
    let mut guard = pid_registry().lock().unwrap_or_else(|p| p.into_inner());
    guard.insert(pid);
}

/// Remove a PID from the close-event termination registry.
pub fn unregister_pid(pid: u32) {
    let mut guard = pid_registry().lock().unwrap_or_else(|p| p.into_inner());
    guard.remove(&pid);
}

/// Best-effort termination for PIDs registered via [`register_pid`].
///
/// This is intended for console close/logoff/shutdown events where `Drop` may not run.
pub fn kill_registered_pids_best_effort() {
    #[cfg(not(windows))]
    {
        return;
    }

    #[cfg(windows)]
    {
        let pids: Vec<u32> = {
            let mut guard = pid_registry().lock().unwrap_or_else(|p| p.into_inner());
            let pids = guard.iter().copied().collect::<Vec<_>>();
            guard.clear();
            pids
        };

        if pids.is_empty() {
            return;
        }

        let mut args: Vec<String> = Vec::with_capacity(2 + pids.len() * 2);
        args.push("/T".to_string());
        args.push("/F".to_string());
        for pid in pids {
            args.push("/PID".to_string());
            args.push(pid.to_string());
        }

        let _ = Command::new("taskkill")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(windows)]
#[derive(Copy, Clone)]
struct ProcessJobHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for ProcessJobHandle {}
#[cfg(windows)]
unsafe impl Sync for ProcessJobHandle {}

#[cfg(windows)]
static PROCESS_JOB: OnceLock<Option<ProcessJobHandle>> = OnceLock::new();
#[cfg(windows)]
static PROCESS_JOB_WARNED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn warn_job_once(msg: &str) {
    if PROCESS_JOB_WARNED.swap(true, Ordering::SeqCst) {
        return;
    }
    eprintln!("titanbash: {}", msg);
}

/// Best-effort: place the current titanbash process into a Windows Job Object with
/// `KILL_ON_JOB_CLOSE`, so that when titanbash exits (including console window close),
/// all child/grandchild processes are terminated by the OS.
pub fn init_kill_on_close_job_best_effort() {
    #[cfg(not(windows))]
    {
        return;
    }

    #[cfg(windows)]
    {
        let _ = PROCESS_JOB.get_or_init(|| {
            match create_kill_on_close_job_and_assign_self() {
                Ok(h) => Some(h),
                Err(e) => {
                    warn_job_once(&format!("job object disabled (fallback to taskkill): {}", e));
                    None
                }
            }
        });
    }
}

#[cfg(windows)]
fn create_kill_on_close_job_and_assign_self() -> Result<ProcessJobHandle> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            bail!("CreateJobObjectW failed (err={})", GetLastError());
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of_mut!(info).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            let err = GetLastError();
            let _ = CloseHandle(job);
            bail!("SetInformationJobObject failed (err={})", err);
        }

        let ok = AssignProcessToJobObject(job, GetCurrentProcess());
        if ok == 0 {
            let err = GetLastError();
            let _ = CloseHandle(job);
            bail!("AssignProcessToJobObject failed (err={})", err);
        }

        Ok(ProcessJobHandle(job))
    }
}

/// A background task
struct Task {
    command: String,
    status: Arc<Mutex<TaskStatus>>,
    output: Arc<Mutex<String>>,
    pid: Arc<Mutex<Option<u32>>>,
    started: Instant,
    handle: Option<JoinHandle<()>>,
}

/// Manages background tasks
pub struct TaskManager {
    tasks: HashMap<TaskId, Task>,
    next_id: TaskId,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            next_id: 1,
        }
    }

    /// Spawn a new background task
    pub fn spawn<F>(&mut self, cmd: &str, f: F) -> Result<TaskId>
    where
        F: FnOnce(Arc<Mutex<Option<u32>>>) -> Result<(i32, String)> + Send + 'static,
    {
        let id = self.next_id;
        self.next_id += 1;

        let status = Arc::new(Mutex::new(TaskStatus::Running));
        let output = Arc::new(Mutex::new(String::new()));
        let pid = Arc::new(Mutex::new(None));

        let status_clone = status.clone();
        let output_clone = output.clone();
        let pid_clone = pid.clone();

        let handle = thread::spawn(move || {
            match f(pid_clone) {
                Ok((code, out)) => {
                    *output_clone.lock().unwrap() = out;
                    *status_clone.lock().unwrap() = TaskStatus::Completed(code);
                }
                Err(e) => {
                    *status_clone.lock().unwrap() = TaskStatus::Failed(e.to_string());
                }
            }
        });

        self.tasks.insert(id, Task {
            command: cmd.to_string(),
            status,
            output,
            pid,
            started: Instant::now(),
            handle: Some(handle),
        });

        Ok(id)
    }

    /// List all tasks
    pub fn list(&self) -> Vec<(TaskId, String, String)> {
        let mut result = Vec::new();

        for (&id, task) in &self.tasks {
            let status = task.status.lock().unwrap().clone();
            let elapsed = task.started.elapsed();
            let status_str = match status {
                TaskStatus::Running => format!("Running ({:.1}s)", elapsed.as_secs_f32()),
                TaskStatus::Completed(code) => format!("Done (exit {})", code),
                TaskStatus::Failed(ref msg) => format!("Failed: {}", msg),
            };
            result.push((id, status_str, task.command.clone()));
        }

        result.sort_by_key(|(id, _, _)| *id);
        result
    }

    /// Get task status
    pub fn status(&self, id: TaskId) -> Option<TaskStatus> {
        self.tasks.get(&id).map(|t| t.status.lock().unwrap().clone())
    }

    /// Get task output
    pub fn output(&self, id: TaskId) -> Option<String> {
        self.tasks.get(&id).map(|t| t.output.lock().unwrap().clone())
    }

    pub fn pid(&self, id: TaskId) -> Option<u32> {
        self.tasks.get(&id).and_then(|t| *t.pid.lock().unwrap())
    }

    pub fn kill(&mut self, id: TaskId) -> Result<()> {
        let Some(task) = self.tasks.get(&id) else {
            bail!("kill: {}: no such job", id);
        };
        let Some(pid) = *task.pid.lock().unwrap() else {
            bail!("kill: {}: process not started yet", id);
        };

        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("kill: failed to execute taskkill for pid {}", pid))?;

        if status.success() {
            unregister_pid(pid);
            Ok(())
        } else {
            bail!("kill: taskkill failed (pid {})", pid)
        }
    }

    /// Best-effort termination of all running background jobs.
    ///
    /// On Windows this uses `taskkill /T /F` (process tree kill). On other platforms this is a no-op.
    pub fn kill_all_running_best_effort(&mut self) -> usize {
        #[cfg(not(windows))]
        {
            0
        }

        #[cfg(windows)]
        {
            let ids: Vec<TaskId> = self
                .tasks
                .iter()
                .filter_map(|(&id, task)| {
                    let status = task.status.lock().unwrap().clone();
                    matches!(status, TaskStatus::Running).then_some(id)
                })
                .collect();

            let mut killed = 0usize;
            for id in ids {
                if self.kill(id).is_ok() {
                    killed += 1;
                }
            }
            killed
        }
    }

    /// Wait for a task to complete
    pub fn wait(&mut self, id: TaskId) -> Option<TaskStatus> {
        if let Some(task) = self.tasks.get_mut(&id) {
            if let Some(handle) = task.handle.take() {
                handle.join().ok();
            }
            Some(task.status.lock().unwrap().clone())
        } else {
            None
        }
    }

    pub fn wait_and_remove(&mut self, id: TaskId) -> Option<TaskStatus> {
        let mut task = self.tasks.remove(&id)?;
        if let Some(handle) = task.handle.take() {
            let _ = handle.join();
        }
        let status = task.status.lock().unwrap().clone();
        Some(status)
    }

    /// Clean up completed tasks
    pub fn cleanup(&mut self) {
        self.tasks.retain(|_, task| {
            matches!(*task.status.lock().unwrap(), TaskStatus::Running)
        });
    }

    /// Check for completed tasks, notify, and remove them
    pub fn check_completed(&mut self) -> Vec<(TaskId, i32, String)> {
        let mut completed = Vec::new();

        for (&id, task) in &self.tasks {
            let status = task.status.lock().unwrap();
            if let TaskStatus::Completed(code) = *status {
                completed.push((id, code, task.command.clone()));
            }
        }

        // Remove notified tasks to prevent duplicate notifications
        for (id, _, _) in &completed {
            self.tasks.remove(id);
        }

        completed
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TaskManager {
    fn drop(&mut self) {
        // When titanbash exits, ensure background jobs don't leak into the user's system.
        let _ = self.kill_all_running_best_effort();
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    fn pid_exists(pid: u32) -> bool {
        let script = format!(
            "try {{ Get-Process -Id {} -ErrorAction Stop | Out-Null; exit 0 }} catch {{ exit 1 }}",
            pid
        );
        Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn dropping_task_manager_terminates_running_jobs() {
        let mut tasks = TaskManager::new();

        let id = tasks
            .spawn("powershell Start-Sleep 30", move |pid| {
                let mut child = Command::new("powershell")
                    .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;

                *pid.lock().unwrap() = Some(child.id());
                let _ = child.wait();
                Ok((0, String::new()))
            })
            .expect("spawn should succeed");

        let start = Instant::now();
        let pid = loop {
            if let Some(pid) = tasks.pid(id) {
                break pid;
            }
            if start.elapsed() > Duration::from_secs(2) {
                panic!("pid was not set within timeout");
            }
            thread::sleep(Duration::from_millis(10));
        };

        assert!(pid_exists(pid), "expected child to be running before drop");

        drop(tasks);

        // Give taskkill a moment; avoid flaky timing.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            if !pid_exists(pid) {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }

        panic!("expected child to be terminated after drop");
    }
}
