//! JSON Lines transport for a single Claude subprocess.

use std::io::{self, Write};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

// A wire record may contain a large tool result, but cannot grow unbounded.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Wire {
    // Drop the tree before the child so the group leader has not been reaped.
    tree: Option<platform::ProcessTree>,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<FrameReader<ChildStdout>>,
}

impl Wire {
    pub async fn spawn(
        command: &str,
        args: &[String],
        allowed_env: &[String],
        cwd: &str,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for name in allowed_env {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }

        let mut tree = platform::ProcessTree::new(&mut cmd)?;
        let mut child = cmd.spawn().map_err(|error| io_error("spawn", error))?;
        tree.attach(&child)?;
        let stdin = child.stdin.take().context("Claude stdin unavailable")?;
        let stdout = child.stdout.take().context("Claude stdout unavailable")?;
        Ok(Self {
            tree: Some(tree),
            child,
            stdin: Some(stdin),
            stdout: Some(FrameReader::new(stdout)),
        })
    }

    /// Write one frame. Cancelling an in-progress write closes stdin so a later
    /// write cannot append to a partial frame. Callers may impose a deadline.
    pub async fn send(&mut self, frame: &Value) -> Result<()> {
        if self.stdin.is_none() {
            bail!("Claude stdin is closed");
        }
        let mut bytes = FrameBytes(Vec::new());
        serde_json::to_writer(&mut bytes, frame).map_err(|error| {
            if error.io_error_kind() == Some(io::ErrorKind::InvalidData) {
                anyhow!("Claude stdin frame exceeds the {MAX_FRAME_BYTES}-byte limit")
            } else {
                anyhow!("Claude stdin frame could not be encoded")
            }
        })?;
        bytes.0.push(b'\n');

        let mut stdin = self.stdin.take().context("Claude stdin is closed")?;
        stdin
            .write_all(&bytes.0)
            .await
            .map_err(|error| io_error("stdin write", error))?;
        // ChildStdin is unbuffered.
        self.stdin = Some(stdin);
        Ok(())
    }

    /// Read one frame. Cancelling this future preserves any partial frame.
    pub async fn recv(&mut self) -> Result<Value> {
        self.stdout
            .as_mut()
            .context("Claude stdout is closed")?
            .recv()
            .await
    }

    /// Close both pipes, terminate the process tree, and wait a bounded time
    /// to reap the direct child. Safe to retry if waiting was cancelled.
    pub async fn close(&mut self) -> Result<()> {
        self.stdin.take();
        self.stdout.take();
        let terminated = self.tree.take().map_or(Ok(()), |mut tree| tree.terminate());
        // Also cover targets without tree support or a failed tree kill.
        let _ = self.child.start_kill();
        timeout(CLOSE_TIMEOUT, self.child.wait())
            .await
            .context("Timed out waiting for Claude subprocess to exit")?
            .map_err(|error| io_error("wait", error))?;
        terminated
    }
}

fn io_error(operation: &str, error: io::Error) -> anyhow::Error {
    // Keep subprocess arguments, cwd, environment, and frame data out of errors.
    anyhow!("Claude subprocess {operation} failed ({})", error.kind())
}

struct FrameBytes(Vec<u8>);

impl Write for FrameBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES - self.0.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct FrameReader<R> {
    reader: Option<BufReader<R>>,
    partial: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: Some(BufReader::new(reader)),
            partial: Vec::new(),
        }
    }

    async fn recv(&mut self) -> Result<Value> {
        let reader = self.reader.as_mut().context("Claude stdout is closed")?;
        loop {
            let available = reader
                .fill_buf()
                .await
                .map_err(|error| io_error("stdout read", error))?;
            if available.is_empty() {
                if self.partial.is_empty() {
                    bail!("Claude stdout reached EOF");
                }
                // Like Lines::next_line, accept a final line without a newline.
                break;
            }
            let newline = available.iter().position(|&byte| byte == b'\n');
            let len = newline.unwrap_or(available.len());
            if len > MAX_FRAME_BYTES - self.partial.len() {
                // Do not drain an arbitrarily large line to find its delimiter.
                self.reader = None;
                self.partial = Vec::new();
                bail!("Claude stdout frame exceeds the {MAX_FRAME_BYTES}-byte limit");
            }
            // Store and consume together before the next await, so cancelled
            // reads leave the stream positioned at the remainder of this line.
            self.partial.extend_from_slice(&available[..len]);
            reader.consume(len + usize::from(newline.is_some()));
            if newline.is_some() {
                break;
            }
        }

        let parsed = serde_json::from_slice(&self.partial);
        self.partial.clear();
        parsed.map_err(|_| anyhow!("Claude stdout frame is not valid JSON"))
    }
}

#[cfg(unix)]
mod platform {
    use super::*;

    pub(super) struct ProcessTree(Option<libc::pid_t>);

    impl ProcessTree {
        pub(super) fn new(cmd: &mut Command) -> Result<Self> {
            cmd.process_group(0);
            Ok(Self(None))
        }

        pub(super) fn attach(&mut self, child: &Child) -> Result<()> {
            self.0 = Some(child.id().context("Claude subprocess PID unavailable")? as libc::pid_t);
            Ok(())
        }

        pub(super) fn terminate(&mut self) -> Result<()> {
            let Some(pgid) = self.0 else { return Ok(()) };
            // SAFETY: pgid identifies the group created for this child, whose
            // leader has not been reaped by Wire.
            if unsafe { libc::killpg(pgid, libc::SIGKILL) } == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(io_error("process group termination", error));
                }
            }
            self.0 = None;
            Ok(())
        }
    }

    impl Drop for ProcessTree {
        fn drop(&mut self) {
            let _ = self.terminate();
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::null;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    use super::*;

    pub(super) struct ProcessTree(OwnedHandle);

    impl ProcessTree {
        pub(super) fn new(_cmd: &mut Command) -> Result<Self> {
            // SAFETY: Null arguments request an unnamed job with default security.
            let handle = unsafe { CreateJobObjectW(null(), null()) };
            if handle.is_null() {
                return Err(io_error("job creation", io::Error::last_os_error()));
            }
            // SAFETY: The newly-created handle is valid and uniquely owned here.
            let job = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: The information class, struct and byte length match.
            if unsafe {
                SetInformationJobObject(
                    job.as_raw_handle().cast(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(io_error("job configuration", io::Error::last_os_error()));
            }
            Ok(Self(job))
        }

        pub(super) fn attach(&mut self, child: &Child) -> Result<()> {
            let handle = child
                .raw_handle()
                .context("Claude subprocess handle unavailable")?;
            // SAFETY: Both the job and process handles are live for the call.
            if unsafe { AssignProcessToJobObject(self.0.as_raw_handle().cast(), handle.cast()) }
                == 0
            {
                return Err(io_error("job assignment", io::Error::last_os_error()));
            }
            Ok(())
        }

        pub(super) fn terminate(&mut self) -> Result<()> {
            // SAFETY: The job handle is live and owned by self.
            if unsafe { TerminateJobObject(self.0.as_raw_handle().cast(), 1) } == 0 {
                return Err(io_error("job termination", io::Error::last_os_error()));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    use serde_json::json;
    use tokio::io::duplex;

    use super::*;

    #[tokio::test]
    async fn cancelled_receive_keeps_partial_frame() {
        let (mut writer, reader) = duplex(64);
        let mut reader = FrameReader::new(reader);
        writer.write_all(b"{\"key\":").await.unwrap();
        {
            let mut receive = std::pin::pin!(reader.recv());
            poll_fn(|cx| {
                assert!(receive.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        writer.write_all(b"42}\r\nnull").await.unwrap();
        drop(writer);
        assert_eq!(reader.recv().await.unwrap(), json!({"key": 42}));
        assert_eq!(reader.recv().await.unwrap(), Value::Null);
        assert!(reader.recv().await.unwrap_err().to_string().contains("EOF"));
    }

    #[tokio::test]
    async fn malformed_frame_errors_do_not_include_child_output() {
        let bytes = b"\"private\\invalid\"\nnull\n";
        let mut reader = FrameReader::new(bytes.as_slice());
        let error = reader.recv().await.unwrap_err();
        assert_eq!(
            format!("{error:?}"),
            "Claude stdout frame is not valid JSON"
        );
        assert_eq!(reader.recv().await.unwrap(), Value::Null);
    }

    #[tokio::test]
    async fn oversized_frame_closes_reader_and_encoder_is_bounded() {
        let bytes = vec![b'x'; MAX_FRAME_BYTES + 1];
        let mut reader = FrameReader::new(bytes.as_slice());
        assert!(
            reader
                .recv()
                .await
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
        assert!(
            reader
                .recv()
                .await
                .unwrap_err()
                .to_string()
                .contains("closed")
        );
        let mut encoded = FrameBytes(Vec::new());
        assert!(encoded.write_all(&bytes).is_err());
        assert!(encoded.0.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_round_trip_and_tree_shutdown() {
        let args = [
            "-c".into(),
            // The background child keeps stdout open even if the shell exits.
            "IFS= read -r line; /bin/sleep 30 & printf '%s\\n' \"$line\"; exit".into(),
        ];
        let mut wire = Wire::spawn("/bin/sh", &args, &[], "/").await.unwrap();
        let frame = json!({"text": "line\nbreak"});
        wire.send(&frame).await.unwrap();
        assert_eq!(
            timeout(CLOSE_TIMEOUT, wire.recv()).await.unwrap().unwrap(),
            frame
        );

        let mut stdout = wire.stdout.take().unwrap();
        wire.close().await.unwrap();
        wire.close().await.unwrap();
        let error = timeout(CLOSE_TIMEOUT, stdout.recv())
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("EOF"));
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;

    pub(super) struct ProcessTree;

    impl ProcessTree {
        pub(super) fn new(_cmd: &mut Command) -> Result<Self> {
            Ok(Self)
        }

        pub(super) fn attach(&mut self, _child: &Child) -> Result<()> {
            Ok(())
        }

        pub(super) fn terminate(&mut self) -> Result<()> {
            Ok(())
        }
    }
}
