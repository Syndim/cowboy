//! JSONL transport for the pi RPC backend: a local subprocess speaking
//! one-JSON-object-per-line records over stdin/stdout.
//!
//! Mirrors the ACP stdio transport's process-tree ownership (descendants
//! inherit the stdout pipe, so only tree termination lets the pending read
//! observe EOF) and stderr diagnostics logging.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::Instant;

/// A platform handle on the process tree rooted at one spawned child.
struct ProcessTreeScope {
    inner: platform::Scope,
    terminated: AtomicBool,
}

impl ProcessTreeScope {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            inner: platform::Scope::new()?,
            terminated: AtomicBool::new(false),
        })
    }

    fn configure(&self, command: &mut Command) {
        self.inner.configure(command);
    }

    fn attach(&self, child: &Child) -> anyhow::Result<()> {
        self.inner.attach(child)
    }

    fn terminate(&self) -> anyhow::Result<()> {
        if self.terminated.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.terminate()
    }
}

impl Drop for ProcessTreeScope {
    fn drop(&mut self) {
        if let Err(err) = self.terminate() {
            tracing::warn!(error = %err, "pi process tree termination on drop failed");
        }
    }
}

#[cfg(windows)]
mod platform {
    use tokio::process::{Child, Command};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    pub(super) struct Scope {
        job: HANDLE,
    }

    unsafe impl Send for Scope {}
    unsafe impl Sync for Scope {}

    impl Scope {
        pub(super) fn new() -> anyhow::Result<Self> {
            // SAFETY: `CreateJobObjectW` with null arguments creates an
            // unnamed job object with default security and returns null on
            // failure.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(anyhow::anyhow!(
                    "failed to create pi job object: {}",
                    std::io::Error::last_os_error()
                ));
            }

            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is a correctly sized, initialized
            // `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` matching the requested
            // information class, and `job` is a live job handle.
            let ok = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                let error = std::io::Error::last_os_error();
                // SAFETY: `job` is a live handle owned here and not used again.
                unsafe { CloseHandle(job) };
                return Err(anyhow::anyhow!(
                    "failed to configure pi job object: {error}"
                ));
            }

            Ok(Self { job })
        }

        pub(super) fn configure(&self, _command: &mut Command) {}

        pub(super) fn attach(&self, child: &Child) -> anyhow::Result<()> {
            let handle = child
                .raw_handle()
                .ok_or_else(|| anyhow::anyhow!("pi process handle unavailable"))?;
            // SAFETY: `handle` is the live process handle owned by `child`,
            // and `self.job` is a live job handle.
            let ok = unsafe { AssignProcessToJobObject(self.job, handle.cast()) };
            if ok == 0 {
                return Err(anyhow::anyhow!(
                    "failed to assign pi process to job object: {}",
                    std::io::Error::last_os_error()
                ));
            }

            Ok(())
        }

        pub(super) fn terminate(&self) -> anyhow::Result<()> {
            // SAFETY: `self.job` is a live job handle; terminating an empty or
            // already terminated job is a no-op.
            let ok = unsafe { TerminateJobObject(self.job, 1) };
            if ok == 0 {
                return Err(anyhow::anyhow!(
                    "failed to terminate pi job object: {}",
                    std::io::Error::last_os_error()
                ));
            }

            Ok(())
        }
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            // SAFETY: `self.job` is a live handle owned here and dropped once.
            unsafe { CloseHandle(self.job) };
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::sync::atomic::{AtomicI32, Ordering};

    use tokio::process::{Child, Command};

    /// A Unix process group owning the pi process tree.
    pub(super) struct Scope {
        pgid: AtomicI32,
    }

    impl Scope {
        pub(super) fn new() -> anyhow::Result<Self> {
            Ok(Self {
                pgid: AtomicI32::new(0),
            })
        }

        pub(super) fn configure(&self, command: &mut Command) {
            command.process_group(0);
        }

        pub(super) fn attach(&self, child: &Child) -> anyhow::Result<()> {
            let pid = child
                .id()
                .ok_or_else(|| anyhow::anyhow!("pi process id unavailable"))?;
            self.pgid.store(pid as i32, Ordering::SeqCst);
            Ok(())
        }

        pub(super) fn terminate(&self) -> anyhow::Result<()> {
            let pgid = self.pgid.load(Ordering::SeqCst);
            if pgid <= 0 {
                return Ok(());
            }

            // SAFETY: `killpg` is a plain libc call with an owned process
            // group id; a missing group yields `ESRCH`, handled below.
            let result = unsafe { libc::killpg(pgid, libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(());
                }

                return Err(anyhow::anyhow!(
                    "failed to terminate pi process group {pgid}: {error}"
                ));
            }

            Ok(())
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use tokio::process::{Child, Command};

    /// Unsupported platform: tree ownership is a no-op and callers fall back
    /// to killing the directly spawned child only.
    pub(super) struct Scope;

    impl Scope {
        pub(super) fn new() -> anyhow::Result<Self> {
            Ok(Self)
        }

        pub(super) fn configure(&self, _command: &mut Command) {}

        pub(super) fn attach(&self, _child: &Child) -> anyhow::Result<()> {
            Ok(())
        }

        pub(super) fn terminate(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }
}

/// Spawn options for one pi RPC subprocess.
#[derive(Debug, Clone)]
pub struct PiProcessConfig {
    /// Executable to spawn (typically `pi`).
    pub command: String,
    /// Arguments putting the executable into RPC mode.
    pub args: Vec<String>,
    /// Restrict inherited environment to these variable names.
    pub clear_env: bool,
    pub allowed_env: Vec<String>,
    /// Environment overrides applied on top of the inherited environment.
    pub env: Vec<(String, String)>,
}

/// A running pi subprocess with JSONL framing over stdio.
pub struct PiProcess {
    writer: Option<BufWriter<ChildStdin>>,
    reader: BufReader<ChildStdout>,
    read_buffer: Vec<u8>,
    line_started: Option<Instant>,
    child: Child,
    command: String,
    pid: Option<u32>,
    tree: Arc<ProcessTreeScope>,
}

impl PiProcess {
    /// Spawn the pi subprocess and return a connected transport.
    pub async fn spawn(config: &PiProcessConfig) -> anyhow::Result<Self> {
        let mut cmd = Command::new(&config.command);
        for arg in &config.args {
            cmd.arg(arg);
        }
        if config.clear_env {
            cmd.env_clear();
            for name in &config.allowed_env {
                if let Some(value) = std::env::var_os(name) {
                    cmd.env(name, value);
                }
            }
        }
        for (key, val) in &config.env {
            cmd.env(key, val);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let tree = Arc::new(ProcessTreeScope::new()?);
        tree.configure(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn pi process '{}': {}", config.command, e)
        })?;

        if let Err(err) = tree.attach(&child) {
            let _ = child.kill().await;
            return Err(anyhow::anyhow!(
                "Failed to take ownership of pi process tree for '{}': {}",
                config.command,
                err
            ));
        }

        let pid = child.id();
        let command = Path::new(&config.command)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_logger(command.clone(), child.id(), stderr);
        }

        tracing::debug!(
            event = "pi_process_spawned",
            command = %command,
            pid = child.id(),
            clear_env = config.clear_env,
            argument_count = config.args.len(),
            allowed_env_count = config.allowed_env.len(),
            configured_env_count = config.env.len(),
            "pi subprocess spawned"
        );

        Ok(Self {
            writer: Some(BufWriter::new(stdin)),
            reader: BufReader::new(stdout),
            read_buffer: Vec::new(),
            line_started: None,
            child,
            command,
            pid,
            tree,
        })
    }

    /// Write one JSONL record (a single line) and flush it.
    pub async fn send_line(&mut self, line: &str) -> anyhow::Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            anyhow::bail!("pi stdin is closed");
        };
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    /// Close stdin, asking pi for an orderly exit.
    pub fn close_stdin(&mut self) {
        self.writer.take();
    }

    /// Wait for the subprocess to exit, bounded by `timeout`.
    /// Returns whether it exited in time.
    pub async fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        match tokio::time::timeout(timeout, self.child.wait()).await {
            Ok(Ok(_)) | Ok(Err(_)) => true,
            Err(_) => false,
        }
    }

    /// Read the next JSONL record, blocking until one full line is available.
    ///
    /// Returns `None` on clean EOF with no partial line pending. Diagnostics
    /// on stderr are logged, never parsed as protocol data.
    pub async fn recv_line(&mut self) -> anyhow::Result<Option<String>> {
        loop {
            let started = *self.line_started.get_or_insert_with(Instant::now);
            let available = match self.reader.fill_buf().await {
                Ok(available) => available,
                Err(error) => {
                    tracing::warn!(event = "pi_stdout_read_failed", command = %self.command, pid = self.pid, error = %error, "pi stdout read failed");
                    return Err(error.into());
                }
            };
            let eof = available.is_empty();
            // Clean EOF with no partial line pending: signal orderly exit.
            // Without this guard, EOF loops back into fill_buf() forever.
            if eof && self.read_buffer.is_empty() {
                self.line_started = None;
                tracing::debug!(event = "pi_stdout_closed", command = %self.command, pid = self.pid, "pi stdout closed");
                return Ok(None);
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let count = newline.map_or(available.len(), |index| index + 1);
            self.read_buffer.extend_from_slice(&available[..count]);
            self.reader.consume(count);
            if newline.is_none() && !eof {
                continue;
            }
            self.line_started = None;
            let bytes = self.read_buffer.len();
            let frame = std::mem::take(&mut self.read_buffer);
            let mut line = match String::from_utf8(frame) {
                Ok(line) => line,
                Err(_) => {
                    tracing::warn!(event = "pi_stdout_frame_failed", command = %self.command, pid = self.pid, bytes, "pi stdout frame is not valid UTF-8");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "pi stdout frame is invalid UTF-8",
                    )
                    .into());
                }
            };
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            tracing::debug!(
                event = "pi_stdout_frame_complete",
                command = %self.command,
                pid = self.pid,
                bytes,
                elapsed_ms = started.elapsed().as_millis() as u64,
                empty = line.trim().is_empty(),
                "pi stdout frame complete"
            );
            if !line.trim().is_empty() {
                return Ok(Some(line));
            }
        }
    }

    /// Force an immediate tree kill after an orderly exit was not observed.
    pub async fn kill(&mut self) {
        let status = self.child.try_wait().ok().flatten();
        let tree_terminated = self.tree.terminate().is_ok();
        tracing::debug!(
            command = %self.command,
            pid = self.pid,
            status = ?status,
            tree_terminated,
            "Killing pi subprocess"
        );
        if status.is_none() {
            let _ = self.child.kill().await;
        }
    }
}

fn spawn_stderr_logger(command: String, pid: Option<u32>, mut stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut buffer = [0; 8192];
        let mut line = Vec::with_capacity(4096);
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) => {
                    log_stderr_diagnostic(&command, pid, &line);
                    tracing::debug!(event = "pi_stderr_eof", command = %command, pid, "pi stderr closed");
                    break;
                }
                Ok(count) => {
                    for byte in &buffer[..count] {
                        if *byte == b'\n' {
                            log_stderr_diagnostic(&command, pid, &line);
                            line.clear();
                        } else if line.len() < 4096 {
                            line.push(*byte);
                        } else {
                            // Over-long diagnostics lines are dropped rather
                            // than flooding logs.
                            line.clear();
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(event = "pi_stderr_read_failed", command = %command, pid, error = %error, "pi stderr read failed");
                    break;
                }
            }
        }
    });
}

fn log_stderr_diagnostic(command: &str, pid: Option<u32>, line: &[u8]) {
    let Ok(text) = std::str::from_utf8(line) else {
        return;
    };
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    tracing::debug!(event = "pi_stderr", command = %command, pid, diagnostic = %cowboy_agent_client::diagnostic_error_text(text), "pi stderr diagnostic");
}
