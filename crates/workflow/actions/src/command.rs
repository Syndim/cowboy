use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use chrono::Utc;
use cowboy_workflow_core::{
    ActionResult, CommandAction, ExecutionContext, Result, StepDetail, StepInput, StepOutput,
    StepRecord, WorkflowError,
};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time;

const CAPTURE_LIMIT_BYTES: usize = 64 * 1024;

const COMMAND_ENV_ALLOW_LIST: [&str; 8] = [
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "TEMP",
    "TMP",
];

fn apply_command_environment(command: &mut Command) {
    for name in COMMAND_ENV_ALLOW_LIST {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandActionRunner {
    cwd: PathBuf,
    capture_limit_bytes: usize,
}

impl CommandActionRunner {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            capture_limit_bytes: CAPTURE_LIMIT_BYTES,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_capture_limit(cwd: impl Into<PathBuf>, capture_limit_bytes: usize) -> Self {
        Self {
            cwd: cwd.into(),
            capture_limit_bytes,
        }
    }

    pub async fn run(
        &self,
        action: CommandAction,
        context: ExecutionContext,
    ) -> Result<ActionResult> {
        let started_at = Utc::now();
        let timer = Instant::now();
        let mut command = Command::new(&action.program);
        command
            .args(&action.args)
            .current_dir(&self.cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        apply_command_environment(&mut command);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                return Ok(ActionResult::completed(
                    self.spawn_error_record(action, context, started_at, timer, err),
                ));
            }
        };

        let stdout = child.stdout.take().ok_or_else(|| {
            WorkflowError::InvalidAction("command stdout pipe was not captured".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            WorkflowError::InvalidAction("command stderr pipe was not captured".to_string())
        })?;
        let stdout_task = capture_stream(stdout, self.capture_limit_bytes);
        let stderr_task = capture_stream(stderr, self.capture_limit_bytes);

        let mut timed_out = false;
        let exit_status = if let Some(timeout_ms) = action.timeout_ms {
            match time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
                Ok(status) => Some(status.map_err(command_io_error)?),
                Err(_) => {
                    timed_out = true;
                    child.start_kill().map_err(command_io_error)?;
                    Some(child.wait().await.map_err(command_io_error)?)
                }
            }
        } else {
            Some(child.wait().await.map_err(command_io_error)?)
        };

        let stdout = join_capture(stdout_task).await?;
        let stderr = join_capture(stderr_task).await?;
        Ok(ActionResult::completed(self.record(
            action,
            context,
            started_at,
            timer,
            CommandOutcome {
                stdout,
                stderr,
                timed_out,
                exit_code: exit_status.and_then(|status| status.code()),
                spawn_error: None,
            },
        )))
    }

    fn spawn_error_record(
        &self,
        action: CommandAction,
        context: ExecutionContext,
        started_at: chrono::DateTime<Utc>,
        timer: Instant,
        err: std::io::Error,
    ) -> StepRecord {
        self.record(
            action,
            context,
            started_at,
            timer,
            CommandOutcome {
                stdout: CapturedStream::default(),
                stderr: CapturedStream::default(),
                timed_out: false,
                exit_code: None,
                spawn_error: Some(err.to_string()),
            },
        )
    }

    fn record(
        &self,
        action: CommandAction,
        context: ExecutionContext,
        started_at: chrono::DateTime<Utc>,
        timer: Instant,
        outcome: CommandOutcome,
    ) -> StepRecord {
        let completed_at = Utc::now();
        let success =
            !outcome.timed_out && outcome.spawn_error.is_none() && outcome.exit_code == Some(0);
        let status = if success {
            action.success_status.clone()
        } else {
            action.failure_status.clone()
        };
        let fields = command_fields(&action, &outcome, success);
        let body = command_body(success, &outcome);
        let raw = Value::Null;

        StepRecord {
            id: context.step_record_id,
            prev: context.prev,
            step: context.step_id,
            action: "command".to_string(),
            input: StepInput {
                prompt: None,
                context: Value::Null,
            },
            output: Some(StepOutput {
                status,
                fields,
                body,
                raw,
            }),
            detail: StepDetail {
                backend: None,
                session_id: None,
                duration_ms: elapsed_ms(timer),
                turn_count: 0,
                usage: None,
            },
            started_at,
            completed_at: Some(completed_at),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct CapturedStream {
    text: String,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct CommandOutcome {
    stdout: CapturedStream,
    stderr: CapturedStream,
    timed_out: bool,
    exit_code: Option<i32>,
    spawn_error: Option<String>,
}

fn capture_stream<R>(mut stream: R, limit: usize) -> JoinHandle<Result<CapturedStream>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut captured = Vec::new();
        let mut truncated = false;
        let mut buffer = [0_u8; 8192];

        loop {
            let read = stream.read(&mut buffer).await.map_err(command_io_error)?;
            if read == 0 {
                break;
            }

            if captured.len() < limit {
                let remaining = limit - captured.len();
                let retained = read.min(remaining);
                captured.extend_from_slice(&buffer[..retained]);
                if retained < read {
                    truncated = true;
                }
            } else {
                truncated = true;
            }
        }

        Ok(CapturedStream {
            text: String::from_utf8_lossy(&captured).into_owned(),
            truncated,
        })
    })
}

async fn join_capture(task: JoinHandle<Result<CapturedStream>>) -> Result<CapturedStream> {
    task.await.map_err(|err| {
        WorkflowError::InvalidAction(format!("command output capture task failed: {err}"))
    })?
}

fn command_fields(action: &CommandAction, outcome: &CommandOutcome, success: bool) -> Value {
    let mut fields = json!({
        "program": &action.program,
        "args": &action.args,
        "success": success,
        "exit_code": outcome.exit_code,
        "stdout": &outcome.stdout.text,
        "stderr": &outcome.stderr.text,
        "timed_out": outcome.timed_out,
        "stdout_truncated": outcome.stdout.truncated,
        "stderr_truncated": outcome.stderr.truncated,
    });

    if let Some(spawn_error) = &outcome.spawn_error {
        fields["spawn_error"] = Value::String(spawn_error.clone());
    }

    fields
}

fn command_body(success: bool, outcome: &CommandOutcome) -> String {
    if success {
        return outcome.stdout.text.clone();
    }

    if !outcome.stderr.text.is_empty() {
        return outcome.stderr.text.clone();
    }

    if !outcome.stdout.text.is_empty() {
        return outcome.stdout.text.clone();
    }

    if let Some(spawn_error) = &outcome.spawn_error {
        return spawn_error.clone();
    }

    if outcome.timed_out {
        return "command timed out".to_string();
    }

    String::new()
}

fn command_io_error(err: std::io::Error) -> WorkflowError {
    WorkflowError::InvalidAction(format!("command execution failed: {err}"))
}

fn elapsed_ms(timer: Instant) -> u64 {
    timer.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tokio::sync::{Mutex, MutexGuard};

    use cowboy_workflow_core::ExecutionContext;

    use super::*;

    static COMMAND_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    const EXPECTED_COMMAND_ENV_ALLOW_LIST: [&str; 8] = [
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
    ];
    const UNAPPROVED_COMMAND_ENV: &str = "COWBOY_COMMAND_UNAPPROVED";

    async fn command_test_lock() -> MutexGuard<'static, ()> {
        COMMAND_TEST_LOCK.lock().await
    }

    fn context() -> ExecutionContext {
        ExecutionContext {
            run_id: "run".to_string(),
            step_id: "step".to_string(),
            step_record_id: "record".to_string(),
            prev: None,
            role: None,
            attempt: 1,
            retry_reason: None,
            original_request: "request".to_string(),
            run_created_at: Utc::now(),
            user_prompts: Vec::new(),
        }
    }

    fn action(program: PathBuf, args: Vec<String>) -> CommandAction {
        CommandAction {
            program: program.to_string_lossy().to_string(),
            args,
            success_status: "ok".to_string(),
            failure_status: "bad".to_string(),
            timeout_ms: None,
        }
    }

    fn helper_args() -> Vec<String> {
        vec![
            "--exact".to_string(),
            "command::tests::command_test_helper".to_string(),
            "--ignored".to_string(),
            "--nocapture".to_string(),
        ]
    }

    fn helper_program(dir: &Path, mode: &str) -> PathBuf {
        let source = std::env::current_exe().unwrap();
        let helper = dir.join(format!("cowboy-command-helper-{mode}"));
        fs::copy(source, &helper).unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper, permissions).unwrap();
        helper
    }

    fn command_record(result: ActionResult) -> StepRecord {
        let ActionResult::Completed(record) = result else {
            panic!("expected completed command record")
        };
        assert_eq!(record.action, "command");
        *record
    }

    fn command_output(result: ActionResult) -> StepOutput {
        command_record(result).output.unwrap()
    }

    #[tokio::test]
    async fn command_runner_records_success_output() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let program = helper_program(dir.path(), "success");
        let runner = CommandActionRunner::new(dir.path());

        let output = command_output(
            runner
                .run(action(program, helper_args()), context())
                .await
                .unwrap(),
        );

        assert_eq!(output.fields["exit_code"], 0);
        assert_eq!(output.status, "ok");
        assert_eq!(output.fields["success"], true);
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains("helper stdout")
        );
        assert_eq!(output.fields["stderr"], "");
        assert!(output.body.contains("helper stdout"));
    }

    #[tokio::test]
    async fn command_runner_uses_sanitized_environment() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let program = helper_program(dir.path(), "env");
        let runner = CommandActionRunner::new(dir.path());

        let output = command_output(
            runner
                .run(action(program, helper_args()), context())
                .await
                .unwrap(),
        );

        assert_eq!(output.status, "ok");
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains("HOME=missing")
        );
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains("CARGO=missing")
        );
    }

    #[tokio::test]
    async fn command_runner_forwards_only_allowlisted_environment_variables() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::command_runner_environment_allow_list_probe",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .envs(EXPECTED_COMMAND_ENV_ALLOW_LIST.map(|name| (name, "test-marker")))
            .env("HOME", "test-marker")
            .env("CARGO", "test-marker")
            .env(UNAPPROVED_COMMAND_ENV, "test-marker")
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "environment allow-list probe failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore]
    async fn command_runner_environment_allow_list_probe() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let program = helper_program(dir.path(), "environment_allow_list");
        let runner = CommandActionRunner::new(dir.path());

        let output = command_output(
            runner
                .run(action(program, helper_args()), context())
                .await
                .unwrap(),
        );
        let stdout = output.fields["stdout"].as_str().unwrap();

        assert_eq!(output.status, "ok");
        for name in EXPECTED_COMMAND_ENV_ALLOW_LIST {
            assert!(
                stdout.contains(&format!("{name}=set")),
                "{name} was not forwarded: {stdout}"
            );
        }

        for name in ["HOME", "CARGO", UNAPPROVED_COMMAND_ENV] {
            assert!(
                stdout.contains(&format!("{name}=missing")),
                "{name} was unexpectedly forwarded: {stdout}"
            );
        }
    }

    #[tokio::test]
    async fn command_runner_preserves_system_root_for_child_runtime_initialization() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::command_runner_system_root_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("SystemRoot", r"C:\Windows")
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "SystemRoot probe failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore]
    async fn command_runner_system_root_probe() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let program = helper_program(dir.path(), "system_root");
        let runner = CommandActionRunner::new(dir.path());

        let output = command_output(
            runner
                .run(action(program, helper_args()), context())
                .await
                .unwrap(),
        );

        assert_eq!(output.status, "ok");
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains("SystemRoot=set"),
            "fields: {}",
            output.fields
        );
    }

    #[tokio::test]
    async fn command_runner_does_not_persist_cwd_or_duplicate_raw_streams() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let program = helper_program(dir.path(), "success");
        let runner = CommandActionRunner::new(dir.path());

        let record = command_record(
            runner
                .run(action(program, helper_args()), context())
                .await
                .unwrap(),
        );
        let output = record.output.unwrap();

        assert_eq!(record.input.context, Value::Null);
        assert_eq!(output.raw, Value::Null);
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains("helper stdout")
        );
    }

    #[tokio::test]
    async fn command_runner_records_failure_output() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let program = helper_program(dir.path(), "failure");
        let runner = CommandActionRunner::new(dir.path());

        let output = command_output(
            runner
                .run(action(program, helper_args()), context())
                .await
                .unwrap(),
        );

        assert_eq!(output.status, "bad");
        assert_eq!(output.fields["success"], false);
        assert_eq!(output.fields["exit_code"], 7);
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains("failure stdout")
        );
        assert!(
            output.fields["stderr"]
                .as_str()
                .unwrap()
                .contains("failure stderr")
        );
        assert!(output.body.contains("failure stderr"));
    }

    #[tokio::test]
    async fn command_runner_records_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let runner = CommandActionRunner::new(dir.path());

        let output = command_output(
            runner
                .run(
                    action(dir.path().join("missing-command"), Vec::new()),
                    context(),
                )
                .await
                .unwrap(),
        );

        assert_eq!(output.status, "bad");
        assert_eq!(output.fields["success"], false);
        assert_eq!(output.fields["exit_code"], Value::Null);
        assert!(
            output.fields["spawn_error"]
                .as_str()
                .unwrap()
                .contains("No such file")
        );
        assert!(output.body.contains("No such file"));
    }

    #[tokio::test]
    async fn command_runner_kills_timed_out_child() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let program = helper_program(dir.path(), "slow");
        let runner = CommandActionRunner::new(dir.path());
        let mut action = action(program, helper_args());
        action.timeout_ms = Some(50);

        let output = command_output(runner.run(action, context()).await.unwrap());

        assert_eq!(output.status, "bad");
        assert_eq!(output.fields["success"], false);
        assert_eq!(output.fields["timed_out"], true);
    }

    #[tokio::test]
    async fn command_runner_marks_truncated_streams() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let stdout_program = helper_program(dir.path(), "large_stdout");
        let runner = CommandActionRunner::with_capture_limit(dir.path(), 8);

        let stdout_output = command_output(
            runner
                .run(action(stdout_program, helper_args()), context())
                .await
                .unwrap(),
        );

        assert_eq!(stdout_output.status, "ok");
        assert_eq!(stdout_output.fields["stdout"].as_str().unwrap().len(), 8);
        assert_eq!(stdout_output.fields["stdout_truncated"], true);
        assert_eq!(stdout_output.fields["stderr_truncated"], false);

        let stderr_program = helper_program(dir.path(), "large_stderr");
        let stderr_runner = CommandActionRunner::with_capture_limit(dir.path(), 512);
        let stderr_output = command_output(
            stderr_runner
                .run(action(stderr_program, helper_args()), context())
                .await
                .unwrap(),
        );

        assert_eq!(stderr_output.status, "ok");
        assert_eq!(stderr_output.fields["stderr"].as_str().unwrap().len(), 512);
        assert_eq!(stderr_output.fields["stderr_truncated"], true);
        assert_eq!(stderr_output.fields["stdout_truncated"], false);
    }

    #[tokio::test]
    async fn command_runner_uses_configured_working_directory() {
        let _guard = command_test_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        fs::create_dir(&cwd).unwrap();
        let program = helper_program(dir.path(), "cwd");
        let runner = CommandActionRunner::new(&cwd);

        let output = command_output(
            runner
                .run(action(program, helper_args()), context())
                .await
                .unwrap(),
        );

        assert_eq!(output.status, "ok", "fields: {}", output.fields);
        assert!(
            output.fields["stdout"]
                .as_str()
                .unwrap()
                .contains(&format!("{}\n", cwd.display()))
        );
    }

    #[test]
    #[ignore]
    fn command_test_helper() {
        let mode = std::env::args()
            .next()
            .and_then(|path| {
                PathBuf::from(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .and_then(|name| {
                name.strip_prefix("cowboy-command-helper-")
                    .map(str::to_string)
            })
            .expect("helper mode is encoded in argv[0]");

        match mode.as_str() {
            "success" => {
                println!("helper stdout");
            }
            "failure" => {
                println!("failure stdout");
                eprintln!("failure stderr");
                std::process::exit(7);
            }
            "slow" => {
                std::thread::sleep(Duration::from_secs(5));
                println!("too late");
            }
            "large_stdout" => {
                println!("abcdefghijklmnopqrstuvwxyz");
            }
            "large_stderr" => {
                eprintln!("{}", "x".repeat(1024));
            }
            "env" => {
                let home = if std::env::var_os("HOME").is_some() {
                    "set"
                } else {
                    "missing"
                };
                let cargo = if std::env::var_os("CARGO").is_some() {
                    "set"
                } else {
                    "missing"
                };
                println!("HOME={home}");
                println!("CARGO={cargo}");
            }
            "environment_allow_list" => {
                for name in EXPECTED_COMMAND_ENV_ALLOW_LIST {
                    let state = if std::env::var_os(name).is_some() {
                        "set"
                    } else {
                        "missing"
                    };
                    println!("{name}={state}");
                }

                for name in ["HOME", "CARGO", UNAPPROVED_COMMAND_ENV] {
                    let state = if std::env::var_os(name).is_some() {
                        "set"
                    } else {
                        "missing"
                    };
                    println!("{name}={state}");
                }
            }
            "system_root" => {
                let system_root = if std::env::var_os("SystemRoot").is_some() {
                    "set"
                } else {
                    "missing"
                };
                println!("SystemRoot={system_root}");
            }
            "cwd" => {
                println!("{}", std::env::current_dir().unwrap().display());
            }
            other => panic!("unknown helper mode {other}"),
        }
    }
}
