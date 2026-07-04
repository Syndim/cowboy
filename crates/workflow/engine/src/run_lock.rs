use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use cowboy_workflow_core::{Result, WorkflowError};
use fs2::FileExt;
use uuid::Uuid;

static ACTIVE_RUN_LOCKS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Clone, Debug)]
pub(crate) struct RunExecutionLocks {
    lock_dir: PathBuf,
}

#[derive(Debug)]
pub(crate) struct RunExecutionGuard {
    active_key: String,
    file: File,
}

impl RunExecutionLocks {
    pub(crate) fn new(workflow_store: PathBuf) -> Self {
        Self {
            lock_dir: workflow_store_lock_dir(&workflow_store),
        }
    }

    pub(crate) fn acquire(&self, run_id: &str) -> Result<RunExecutionGuard> {
        let lock_id = parse_run_lock_id(run_id)?;
        let lock_key = format!("run-{lock_id}");
        let active_key = self.active_key(&lock_key);

        {
            let mut active = ACTIVE_RUN_LOCKS
                .lock()
                .map_err(|_| WorkflowError::InvalidAction("run lock set poisoned".to_string()))?;
            if !active.insert(active_key.clone()) {
                return Err(active_run_error(run_id));
            }
        }

        match self.acquire_file_lock(&lock_key) {
            Ok(file) => Ok(RunExecutionGuard { active_key, file }),
            Err(err) => {
                release_in_process(&active_key)?;
                Err(err)
            }
        }
    }

    pub(crate) fn lock_dir(&self) -> &Path {
        &self.lock_dir
    }

    fn acquire_file_lock(&self, lock_key: &str) -> Result<File> {
        fs::create_dir_all(&self.lock_dir)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        let lock_path = self.lock_dir.join(format!("{lock_key}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
        file.try_lock_exclusive().map_err(|err| match err.kind() {
            std::io::ErrorKind::WouldBlock => active_run_error(lock_key),
            _ => WorkflowError::InvalidAction(err.to_string()),
        })?;
        Ok(file)
    }

    fn active_key(&self, lock_key: &str) -> String {
        format!("{}:{lock_key}", self.lock_dir.display())
    }
}

impl Drop for RunExecutionGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = release_in_process(&self.active_key);
    }
}

pub(crate) fn parse_run_lock_id(run_id: &str) -> Result<Uuid> {
    let Some(raw_uuid) = run_id.strip_prefix("run-") else {
        return Err(invalid_run_id_error(run_id));
    };

    let uuid = Uuid::parse_str(raw_uuid).map_err(|_| invalid_run_id_error(run_id))?;
    if raw_uuid != uuid.to_string() {
        return Err(invalid_run_id_error(run_id));
    }

    Ok(uuid)
}

fn workflow_store_lock_dir(workflow_store: &Path) -> PathBuf {
    let file_name = workflow_store
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workflow.redb");
    workflow_store.with_file_name(format!("{file_name}.locks"))
}

fn release_in_process(active_key: &str) -> Result<()> {
    let mut active = ACTIVE_RUN_LOCKS
        .lock()
        .map_err(|_| WorkflowError::InvalidAction("run lock set poisoned".to_string()))?;
    active.remove(active_key);
    Ok(())
}

fn invalid_run_id_error(run_id: &str) -> WorkflowError {
    WorkflowError::InvalidAction(format!("invalid run id {run_id:?}; expected run-<uuid>"))
}

fn active_run_error(run_id: &str) -> WorkflowError {
    WorkflowError::InvalidAction(format!(
        "run {run_id} is already active in another Cowboy instance"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_lock_rejects_same_run_in_process() {
        let dir = tempfile::tempdir().unwrap();
        let locks = RunExecutionLocks::new(dir.path().join("state/workflow.redb"));
        let run_id = "run-00000000-0000-0000-0000-000000000001";
        let _first = locks.acquire(run_id).unwrap();

        let err = locks.acquire(run_id).unwrap_err();

        assert!(err.to_string().contains("already active"));
        assert!(!err.to_string().contains("redb"));
    }

    #[test]
    fn run_lock_allows_different_runs() {
        let dir = tempfile::tempdir().unwrap();
        let locks = RunExecutionLocks::new(dir.path().join("state/workflow.redb"));
        let first = "run-00000000-0000-0000-0000-000000000001";
        let second = "run-00000000-0000-0000-0000-000000000002";

        let _first = locks.acquire(first).unwrap();
        let _second = locks.acquire(second).unwrap();
    }

    #[test]
    fn run_lock_rejects_invalid_ids_before_creating_lock_dir() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_store = dir.path().join("state/workflow.redb");
        let locks = RunExecutionLocks::new(workflow_store);

        for run_id in [
            "../run-00000000-0000-0000-0000-000000000000",
            "/tmp/run-00000000-0000-0000-0000-000000000000",
            "run-../../00000000-0000-0000-0000-000000000000",
            "run-not-a-uuid",
        ] {
            let err = locks.acquire(run_id).unwrap_err();
            assert!(err.to_string().contains("invalid run id"));
        }

        assert!(!locks.lock_dir().exists());
    }

    #[test]
    fn run_lock_uses_canonical_uuid_filename_next_to_workflow_store() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_store = dir.path().join("shared/workflow.redb");
        let locks = RunExecutionLocks::new(workflow_store);
        let run_id = "run-00000000-0000-0000-0000-000000000001";

        let _guard = locks.acquire(run_id).unwrap();

        assert_eq!(
            locks.lock_dir(),
            dir.path().join("shared/workflow.redb.locks")
        );
        assert!(locks.lock_dir().join(format!("{run_id}.lock")).exists());
    }

    #[test]
    fn run_lock_namespace_follows_workflow_store_not_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let shared_store = dir.path().join("shared/workflow.redb");
        let locks_a = RunExecutionLocks::new(shared_store.clone());
        let locks_b = RunExecutionLocks::new(shared_store);
        let run_id = "run-00000000-0000-0000-0000-000000000001";
        let _first = locks_a.acquire(run_id).unwrap();

        let err = locks_b.acquire(run_id).unwrap_err();

        assert!(err.to_string().contains("already active"));
    }
}
