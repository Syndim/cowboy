//! Ownership of the *process tree* an agent subprocess creates.
//!
//! Killing only the directly spawned agent process is not enough: the agent's
//! descendants inherit its stdio pipes, so the transport's pending stdout read
//! never observes EOF and process shutdown blocks forever. A
//! [`ProcessTreeScope`] is configured on the [`Command`] before spawn, attached
//! to the spawned child, and terminates the whole tree on demand or on drop.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::process::{Child, Command};

/// A platform handle on the process tree rooted at one spawned child.
pub(crate) struct ProcessTreeScope {
    inner: platform::Scope,
    terminated: AtomicBool,
}

impl ProcessTreeScope {
    /// Create an empty scope that no process belongs to yet.
    pub(crate) fn new() -> anyhow::Result<Self> {
        Ok(Self {
            inner: platform::Scope::new()?,
            terminated: AtomicBool::new(false),
        })
    }

    /// Apply spawn-time configuration required for tree ownership.
    /// Must be called before `Command::spawn`.
    pub(crate) fn configure(&self, command: &mut Command) {
        self.inner.configure(command);
    }

    /// Take ownership of the freshly spawned child and its future descendants.
    pub(crate) fn attach(&self, child: &Child) -> anyhow::Result<()> {
        self.inner.attach(child)
    }

    /// Terminate the whole tree. Idempotent: repeated calls succeed.
    pub(crate) fn terminate(&self) -> anyhow::Result<()> {
        if self.terminated.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        self.inner.terminate()
    }
}

impl Drop for ProcessTreeScope {
    fn drop(&mut self) {
        if let Err(err) = self.terminate() {
            tracing::warn!(error = %err, "agent process tree termination on drop failed");
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

    /// A Windows Job Object owning the agent's process tree. Job handles are
    /// process-wide kernel handles and are safe to use from any thread.
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
                    "failed to create agent job object: {}",
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
                    "failed to configure agent job object: {error}"
                ));
            }

            Ok(Self { job })
        }

        pub(super) fn configure(&self, _command: &mut Command) {}

        pub(super) fn attach(&self, child: &Child) -> anyhow::Result<()> {
            let handle = child
                .raw_handle()
                .ok_or_else(|| anyhow::anyhow!("agent process handle unavailable"))?;
            // SAFETY: `handle` is the live process handle owned by `child`,
            // and `self.job` is a live job handle.
            let ok = unsafe { AssignProcessToJobObject(self.job, handle.cast()) };
            if ok == 0 {
                return Err(anyhow::anyhow!(
                    "failed to assign agent process to job object: {}",
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
                    "failed to terminate agent job object: {}",
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

    /// A Unix process group owning the agent's process tree.
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
                .ok_or_else(|| anyhow::anyhow!("agent process id unavailable"))?;
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
                    "failed to terminate agent process group {pgid}: {error}"
                ));
            }

            Ok(())
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use tokio::process::{Child, Command};

    /// Unsupported platform: tree ownership is a documented no-op and callers
    /// fall back to killing the directly spawned child only.
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

#[cfg(test)]
mod tests {
    use std::process::Stdio;
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::*;

    /// Command that spawns a longer-lived descendant inheriting stdout and
    /// then stays alive itself.
    fn command_leaving_a_descendant() -> Command {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("powershell");
            command.args([
                "-NoProfile",
                "-Command",
                "Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -NoNewWindow; Write-Output ready; Start-Sleep -Seconds 30",
            ]);
            command
        };

        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 30 & echo ready; sleep 30"]);
            command
        };

        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::null());
        command.kill_on_drop(true);
        command
    }

    async fn spawn_tree(scope: &ProcessTreeScope) -> (Child, tokio::process::ChildStdout) {
        let mut command = command_leaving_a_descendant();
        scope.configure(&mut command);
        let mut child = command.spawn().expect("spawn tree root");
        scope.attach(&child).expect("attach tree root");
        let stdout = child.stdout.take().expect("stdout piped");
        (child, stdout)
    }

    async fn wait_for_ready(reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) {
        let ready = tokio::time::timeout(Duration::from_secs(30), reader.next_line())
            .await
            .expect("readiness line within 30s")
            .expect("read readiness line");
        assert_eq!(ready.as_deref(), Some("ready"));
    }

    #[tokio::test]
    async fn terminate_kills_descendants() {
        let scope = ProcessTreeScope::new().unwrap();
        let (mut child, stdout) = spawn_tree(&scope).await;
        let mut reader = BufReader::new(stdout).lines();
        wait_for_ready(&mut reader).await;

        scope.terminate().unwrap();

        let eof = tokio::time::timeout(Duration::from_secs(10), reader.next_line())
            .await
            .expect("stdout reached EOF within 10s")
            .expect("read stdout after termination");
        assert_eq!(eof, None);
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn terminate_is_idempotent() {
        let scope = ProcessTreeScope::new().unwrap();
        let (mut child, _stdout) = spawn_tree(&scope).await;

        assert!(scope.terminate().is_ok());
        assert!(scope.terminate().is_ok());
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn drop_terminates_tree() {
        let scope = ProcessTreeScope::new().unwrap();
        let (mut child, stdout) = spawn_tree(&scope).await;
        let mut reader = BufReader::new(stdout).lines();
        wait_for_ready(&mut reader).await;

        drop(scope);

        let eof = tokio::time::timeout(Duration::from_secs(10), reader.next_line())
            .await
            .expect("stdout reached EOF within 10s")
            .expect("read stdout after scope drop");
        assert_eq!(eof, None);
        let _ = child.wait().await;
    }
}
