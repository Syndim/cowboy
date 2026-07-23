use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use cowboy_workflow_core::{
    AbortAgentPromptWindowOutcome, AgentPromptWindow, AppendUserPromptOutcome,
    CompareAndSealPromptWindowOutcome, ObjectHash, ObjectKind, OpenAgentPromptWindowOutcome,
    RoleSession, RunHead, RunId, RunStatus, RunStore, RunUserPrompt, TurnRecord, WorkflowRun,
};
use redb::{Database, ReadableDatabase, ReadableTable};
use serde::{Serialize, de::DeserializeOwned};

use crate::hash::{canonical_object_bytes, object_hash};
use crate::tables::{
    AGENT_PROMPT_WINDOWS, OBJECTS, ROLE_SESSIONS, RUN_HEADS, RUN_TURNS, RUN_USER_PROMPTS, RUNS,
};
use crate::{Error, Result};

const OPEN_RETRY_BACKOFF: Duration = Duration::from_millis(25);

pub type StoreWaitObserver = Arc<dyn Fn(&Path) + Send + Sync + 'static>;
/// Snapshot used to interrupt an in-progress database availability wait.
#[derive(Clone)]
pub struct StoreWaitCancellation {
    generation: Arc<AtomicU64>,
    expected_generation: u64,
}

impl StoreWaitCancellation {
    pub fn new(generation: Arc<AtomicU64>, expected_generation: u64) -> Self {
        Self {
            generation,
            expected_generation,
        }
    }

    fn is_cancelled(&self) -> bool {
        self.generation.load(Ordering::Acquire) != self.expected_generation
    }
}

/// redb-backed implementation of workflow run storage.
///
/// The store keeps immutable content-addressed objects in `OBJECTS` and mutable
/// run state in dedicated tables. It opens redb only inside each operation, so
/// idle Cowboy processes do not retain redb's exclusive writable database lock.
#[derive(Clone)]
pub struct RedbRunStore {
    /// Path to the redb workflow database.
    path: PathBuf,
    /// Optional notification hook fired once when a database-open wait begins.
    wait_observer: Option<StoreWaitObserver>,
    /// Optional cancellation check evaluated while the database remains busy.
    wait_cancellation: Option<StoreWaitCancellation>,
}

impl RedbRunStore {
    /// Build a cloneable store value without opening redb until an operation runs.
    pub fn lazy(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            wait_observer: None,
            wait_cancellation: None,
        }
    }

    /// Return a store clone for the same path with a per-operation wait observer.
    pub fn with_wait_observer(&self, wait_observer: StoreWaitObserver) -> Self {
        Self {
            path: self.path.clone(),
            wait_observer: Some(wait_observer),
            wait_cancellation: self.wait_cancellation.clone(),
        }
    }

    /// Return a store clone whose database-open wait can be interrupted.
    pub fn with_wait_cancellation(&self, wait_cancellation: StoreWaitCancellation) -> Self {
        Self {
            path: self.path.clone(),
            wait_observer: self.wait_observer.clone(),
            wait_cancellation: Some(wait_cancellation),
        }
    }

    /// Create or open a database at `path`, then drop the validation handle.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_inner(path, None)
    }

    /// Create or open a database at `path` with a wait-start observer.
    pub fn create_with_wait_observer(
        path: impl AsRef<Path>,
        wait_observer: StoreWaitObserver,
    ) -> Result<Self> {
        Self::create_inner(path, Some(wait_observer))
    }

    fn create_inner(
        path: impl AsRef<Path>,
        wait_observer: Option<StoreWaitObserver>,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        ensure_parent_dir(&path)?;
        drop(open_database_when_available(
            &path,
            |path| Database::create(path),
            wait_observer.as_ref(),
            None,
        )?);
        Ok(Self {
            path,
            wait_observer,
            wait_cancellation: None,
        })
    }

    /// Open an existing database at `path`, then drop the validation handle.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        drop(open_database_when_available(
            &path,
            |path| Database::open(path),
            None,
            None,
        )?);
        Ok(Self {
            path,
            wait_observer: None,
            wait_cancellation: None,
        })
    }

    /// Save the mutable workflow run snapshot by run id.
    pub fn save_run(&self, run: &WorkflowRun) -> Result<()> {
        self.with_write(|write| {
            let bytes = serde_json::to_vec(run)?;
            let mut runs = write.open_table(RUNS)?;
            runs.insert(run.id.as_str(), bytes.as_slice())?;
            Ok(())
        })
    }

    /// Load a workflow run snapshot by run id.
    pub fn load_run(&self, run_id: &RunId) -> Result<WorkflowRun> {
        self.with_read(|read| {
            let table = match read.open_table(RUNS) {
                Ok(table) => table,
                Err(redb::TableError::TableDoesNotExist(_)) => {
                    return Err(Error::RunNotFound(run_id.clone()));
                }
                Err(err) => return Err(err.into()),
            };
            let Some(bytes) = table.get(run_id.as_str())? else {
                return Err(Error::RunNotFound(run_id.clone()));
            };
            decode_json(bytes.value())
        })
    }

    /// List all run heads known to the store.
    pub fn list_runs(&self) -> Result<Vec<RunHead>> {
        self.with_read(|read| {
            let table = match read.open_table(RUN_HEADS) {
                Ok(table) => table,
                Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
                Err(err) => return Err(err.into()),
            };
            let mut out = Vec::new();
            for item in table.iter()? {
                let (_, value) = item?;
                out.push(decode_json(value.value())?);
            }
            Ok(out)
        })
    }

    /// Store an immutable object and return its content hash.
    pub fn put_object<T: Serialize>(&self, kind: ObjectKind, value: &T) -> Result<ObjectHash> {
        self.with_write(|write| Self::put_object_in_tx(write, kind, value))
    }

    /// Load an immutable object by content hash.
    pub fn get_object<T: DeserializeOwned>(&self, hash: &ObjectHash) -> Result<T> {
        self.with_read(|read| {
            let table = read.open_table(OBJECTS)?;
            let Some(bytes) = table.get(hash.as_str())? else {
                return Err(Error::ObjectNotFound(hash.clone()));
            };
            decode_object(bytes.value())
        })
    }

    /// Save or replace the mutable head for a run.
    pub fn update_run_head(&self, run_id: &str, head: RunHead) -> Result<()> {
        self.with_write(|write| {
            let bytes = serde_json::to_vec(&head)?;
            let mut table = write.open_table(RUN_HEADS)?;
            table.insert(run_id, bytes.as_slice())?;
            Ok(())
        })
    }

    /// Load the mutable head for a run.
    pub fn load_run_head(&self, run_id: &str) -> Result<RunHead> {
        self.with_read(|read| {
            let table = read.open_table(RUN_HEADS)?;
            let Some(bytes) = table.get(run_id)? else {
                return Err(Error::RunNotFound(run_id.to_string()));
            };
            decode_json(bytes.value())
        })
    }

    /// Save or replace a backend session for one `(run_id, role_id)` pair.
    pub fn save_role_session(&self, session: RoleSession) -> Result<()> {
        self.with_write(|write| {
            let key = role_session_key(&session.run_id, &session.role_id);
            let bytes = serde_json::to_vec(&session)?;
            let mut table = write.open_table(ROLE_SESSIONS)?;
            table.insert(key.as_str(), bytes.as_slice())?;
            Ok(())
        })
    }

    /// Load a backend session for one `(run_id, role_id)` pair.
    pub fn load_role_session(&self, run_id: &str, role_id: &str) -> Result<Option<RoleSession>> {
        self.with_read(|read| {
            let table = match read.open_table(ROLE_SESSIONS) {
                Ok(table) => table,
                Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            let key = role_session_key(run_id, role_id);
            table
                .get(key.as_str())?
                .map(|value| decode_json(value.value()))
                .transpose()
        })
    }

    /// Delete all backend sessions associated with a run.
    pub fn delete_role_sessions(&self, run_id: &str) -> Result<()> {
        self.with_write(|write| {
            let mut table = write.open_table(ROLE_SESSIONS)?;
            let prefix = format!("{run_id}:");
            let keys = table
                .range::<&str>(run_id..)?
                .filter_map(|item| item.ok().map(|(key, _)| key.value().to_string()))
                .take_while(|key| key.starts_with(&prefix))
                .collect::<Vec<_>>();
            for key in keys {
                table.remove(key.as_str())?;
            }
            Ok(())
        })
    }

    /// Store a turn record and append its hash to the run/step turn index.
    pub fn append_turn(&self, run_id: &str, turn: TurnRecord) -> Result<ObjectHash> {
        self.with_write(|write| {
            let hash = Self::put_object_in_tx(write, ObjectKind::TurnRecord, &turn)?;
            let key = format!("{run_id}:{}", turn.step_id);
            let mut table = write.open_table(RUN_TURNS)?;
            let mut turns = table
                .get(key.as_str())?
                .map(|bytes| serde_json::from_slice::<Vec<ObjectHash>>(bytes.value()))
                .transpose()?
                .unwrap_or_default();
            turns.push(hash.clone());
            let bytes = serde_json::to_vec(&turns)?;
            table.insert(key.as_str(), bytes.as_slice())?;
            Ok(hash)
        })
    }

    /// Load durable follow-up prompts in sequence order.
    pub fn load_user_prompts(&self, run_id: &str) -> Result<Vec<RunUserPrompt>> {
        self.with_read(|read| {
            let table = match read.open_table(RUN_USER_PROMPTS) {
                Ok(table) => table,
                Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
                Err(err) => return Err(err.into()),
            };
            table
                .get(run_id)?
                .map(|value| decode_json(value.value()))
                .transpose()
                .map(Option::unwrap_or_default)
        })
    }

    /// Open a new prompt window and bind it to the current durable prompt baseline.
    pub fn open_agent_prompt_window(
        &self,
        mut window: AgentPromptWindow,
    ) -> Result<OpenAgentPromptWindowOutcome> {
        self.with_write(|write| {
            let run = {
                let runs = write.open_table(RUNS)?;
                let Some(value) = runs.get(window.run_id.as_str())? else {
                    return Ok(OpenAgentPromptWindowOutcome::MissingRun);
                };
                decode_json::<WorkflowRun>(value.value())?
            };
            if !matches!(run.status, RunStatus::Running) {
                return Ok(OpenAgentPromptWindowOutcome::TerminalRun);
            }

            let baseline = {
                let prompts = write.open_table(RUN_USER_PROMPTS)?;
                prompts
                    .get(window.run_id.as_str())?
                    .map(|value| decode_json::<Vec<RunUserPrompt>>(value.value()))
                    .transpose()?
                    .and_then(|prompts| prompts.last().map(|prompt| prompt.sequence))
                    .unwrap_or(0)
            };
            window.baseline_sequence = baseline;
            window.applied_sequence = baseline;
            window.sealed_at = None;
            let bytes = serde_json::to_vec(&window)?;
            write
                .open_table(AGENT_PROMPT_WINDOWS)?
                .insert(window.run_id.as_str(), bytes.as_slice())?;
            Ok(OpenAgentPromptWindowOutcome::Opened(window))
        })
    }

    /// Append a prompt only through the matching open token, assigning its sequence atomically.
    pub fn append_user_prompt(
        &self,
        run_id: &str,
        window_id: &str,
        content: String,
    ) -> Result<AppendUserPromptOutcome> {
        self.with_write(|write| {
            let run = {
                let runs = write.open_table(RUNS)?;
                let Some(value) = runs.get(run_id)? else {
                    return Ok(AppendUserPromptOutcome::MissingRun);
                };
                decode_json::<WorkflowRun>(value.value())?
            };
            if !matches!(run.status, RunStatus::Running) {
                return Ok(AppendUserPromptOutcome::TerminalRun);
            }

            let window = {
                let windows = write.open_table(AGENT_PROMPT_WINDOWS)?;
                let Some(value) = windows.get(run_id)? else {
                    return Ok(AppendUserPromptOutcome::NoWindow);
                };
                decode_json::<AgentPromptWindow>(value.value())?
            };
            if window.window_id != window_id {
                return Ok(AppendUserPromptOutcome::StaleWindow);
            }
            if !window.is_open() {
                return Ok(AppendUserPromptOutcome::SealedWindow);
            }

            let mut table = write.open_table(RUN_USER_PROMPTS)?;
            let mut prompts = table
                .get(run_id)?
                .map(|value| decode_json::<Vec<RunUserPrompt>>(value.value()))
                .transpose()?
                .unwrap_or_default();
            let sequence = prompts
                .last()
                .map(|prompt| prompt.sequence + 1)
                .unwrap_or(1);
            let submitted_at = Utc::now();
            let prompt = RunUserPrompt {
                sequence,
                content,
                submitted_at: DateTime::from_timestamp_millis(submitted_at.timestamp_millis())
                    .expect("valid UTC timestamp milliseconds"),
            };
            prompts.push(prompt.clone());
            let bytes = serde_json::to_vec(&prompts)?;
            table.insert(run_id, bytes.as_slice())?;
            Ok(AppendUserPromptOutcome::Accepted(prompt))
        })
    }

    /// Return prompts newer than `applied_sequence`, or seal when none remain.
    pub fn compare_and_seal_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        applied_sequence: u64,
        sealed_at: DateTime<Utc>,
    ) -> Result<CompareAndSealPromptWindowOutcome> {
        self.with_write(|write| {
            let run = {
                let runs = write.open_table(RUNS)?;
                let Some(value) = runs.get(run_id)? else {
                    return Ok(CompareAndSealPromptWindowOutcome::MissingRun);
                };
                decode_json::<WorkflowRun>(value.value())?
            };
            if !matches!(run.status, RunStatus::Running) {
                return Ok(CompareAndSealPromptWindowOutcome::TerminalRun);
            }

            let mut window = {
                let windows = write.open_table(AGENT_PROMPT_WINDOWS)?;
                let Some(value) = windows.get(run_id)? else {
                    return Ok(CompareAndSealPromptWindowOutcome::NoWindow);
                };
                decode_json::<AgentPromptWindow>(value.value())?
            };
            if window.window_id != window_id {
                return Ok(CompareAndSealPromptWindowOutcome::StaleWindow);
            }
            if !window.is_open() {
                return Ok(CompareAndSealPromptWindowOutcome::Sealed(window));
            }
            if applied_sequence < window.applied_sequence {
                return Err(Error::InvalidPromptState(format!(
                    "applied sequence moved backwards from {} to {applied_sequence}",
                    window.applied_sequence
                )));
            }

            let prompts = {
                let table = write.open_table(RUN_USER_PROMPTS)?;
                table
                    .get(run_id)?
                    .map(|value| decode_json::<Vec<RunUserPrompt>>(value.value()))
                    .transpose()?
                    .unwrap_or_default()
            };
            let latest = prompts.last().map(|prompt| prompt.sequence).unwrap_or(0);
            if applied_sequence > latest {
                return Err(Error::InvalidPromptState(format!(
                    "applied sequence {applied_sequence} exceeds latest sequence {latest}"
                )));
            }

            window.applied_sequence = applied_sequence;
            let pending = prompts
                .into_iter()
                .filter(|prompt| prompt.sequence > applied_sequence)
                .collect::<Vec<_>>();
            if pending.is_empty() {
                window.sealed_at = Some(sealed_at);
            }
            let bytes = serde_json::to_vec(&window)?;
            write
                .open_table(AGENT_PROMPT_WINDOWS)?
                .insert(run_id, bytes.as_slice())?;
            if pending.is_empty() {
                Ok(CompareAndSealPromptWindowOutcome::Sealed(window))
            } else {
                Ok(CompareAndSealPromptWindowOutcome::Pending {
                    window,
                    prompts: pending,
                })
            }
        })
    }

    /// Seal the matching window without accepting further prompts.
    pub fn abort_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        aborted_at: DateTime<Utc>,
    ) -> Result<AbortAgentPromptWindowOutcome> {
        self.with_write(|write| {
            {
                let runs = write.open_table(RUNS)?;
                if runs.get(run_id)?.is_none() {
                    return Ok(AbortAgentPromptWindowOutcome::MissingRun);
                }
            }
            let mut window = {
                let windows = write.open_table(AGENT_PROMPT_WINDOWS)?;
                let Some(value) = windows.get(run_id)? else {
                    return Ok(AbortAgentPromptWindowOutcome::NoWindow);
                };
                decode_json::<AgentPromptWindow>(value.value())?
            };
            if window.window_id != window_id {
                return Ok(AbortAgentPromptWindowOutcome::StaleWindow);
            }
            window.sealed_at.get_or_insert(aborted_at);
            let bytes = serde_json::to_vec(&window)?;
            write
                .open_table(AGENT_PROMPT_WINDOWS)?
                .insert(run_id, bytes.as_slice())?;
            Ok(AbortAgentPromptWindowOutcome::Aborted(window))
        })
    }

    /// Remove process-stale prompt-window metadata under the caller's execution guard.
    pub fn clear_agent_prompt_window(&self, run_id: &str) -> Result<Option<AgentPromptWindow>> {
        self.with_write(|write| {
            let mut windows = write.open_table(AGENT_PROMPT_WINDOWS)?;
            let window = windows
                .get(run_id)?
                .map(|value| decode_json::<AgentPromptWindow>(value.value()))
                .transpose()?;
            windows.remove(run_id)?;
            Ok(window)
        })
    }

    /// Delete a run snapshot, run head, and indexed turn lists for the run.
    ///
    /// Immutable objects are intentionally left in `OBJECTS`; later garbage
    /// collection can remove unreferenced objects once we track reachability.
    pub fn delete_run(&self, run_id: &str) -> Result<()> {
        self.with_write(|write| {
            write.open_table(RUNS)?.remove(run_id)?;
            write.open_table(RUN_HEADS)?.remove(run_id)?;
            write.open_table(RUN_USER_PROMPTS)?.remove(run_id)?;
            write.open_table(AGENT_PROMPT_WINDOWS)?.remove(run_id)?;

            let mut role_sessions = write.open_table(ROLE_SESSIONS)?;
            let session_prefix = format!("{run_id}:");
            let session_keys = role_sessions
                .range::<&str>(run_id..)?
                .filter_map(|item| item.ok().map(|(key, _)| key.value().to_string()))
                .take_while(|key| key.starts_with(&session_prefix))
                .collect::<Vec<_>>();
            for key in session_keys {
                role_sessions.remove(key.as_str())?;
            }

            let mut turns = write.open_table(RUN_TURNS)?;
            let turn_prefix = format!("{run_id}:");
            let keys = turns
                .range::<&str>(run_id..)?
                .filter_map(|item| item.ok().map(|(key, _)| key.value().to_string()))
                .take_while(|key| key.starts_with(&turn_prefix))
                .collect::<Vec<_>>();
            for key in keys {
                turns.remove(key.as_str())?;
            }
            Ok(())
        })
    }

    /// Delete an immutable object by hash.
    ///
    /// This is a low-level cleanup API. Callers must ensure no run head, step,
    /// turn list, or future index still references the object.
    pub fn delete_object(&self, hash: &ObjectHash) -> Result<()> {
        self.with_write(|write| {
            write.open_table(OBJECTS)?.remove(hash.as_str())?;
            Ok(())
        })
    }

    /// Store an immutable object inside an existing write transaction.
    fn put_object_in_tx<T: Serialize>(
        write: &redb::WriteTransaction,
        kind: ObjectKind,
        value: &T,
    ) -> Result<ObjectHash> {
        let hash = object_hash(kind, value)?;
        let bytes = canonical_object_bytes(kind, value)?;
        let mut table = write.open_table(OBJECTS)?;
        table.insert(hash.as_str(), bytes.as_slice())?;
        Ok(hash)
    }

    fn with_read<T>(&self, f: impl FnOnce(&redb::ReadTransaction) -> Result<T>) -> Result<T> {
        let db = self.open_database()?;
        let read = db.begin_read()?;
        f(&read)
    }

    fn with_write<T>(&self, f: impl FnOnce(&redb::WriteTransaction) -> Result<T>) -> Result<T> {
        let db = self.open_database()?;
        let write = db.begin_write()?;
        let value = f(&write)?;
        write.commit()?;
        Ok(value)
    }

    fn open_database(&self) -> Result<Database> {
        ensure_parent_dir(&self.path)?;
        open_database_when_available(
            &self.path,
            |path| Database::create(path),
            self.wait_observer.as_ref(),
            self.wait_cancellation.as_ref(),
        )
    }
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn open_database_when_available(
    path: &Path,
    mut open: impl FnMut(&Path) -> std::result::Result<Database, redb::DatabaseError>,
    wait_observer: Option<&StoreWaitObserver>,
    wait_cancellation: Option<&StoreWaitCancellation>,
) -> Result<Database> {
    let mut waiting = false;
    loop {
        match open(path) {
            Ok(db) => {
                if waiting {
                    tracing::info!(workflow_store = %path.display(), "workflow store available; continuing");
                }

                return Ok(db);
            }
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => {
                if wait_cancellation.is_some_and(StoreWaitCancellation::is_cancelled) {
                    tracing::debug!(workflow_store = %path.display(), "workflow store wait cancelled");
                    return Err(Error::WaitCancelled);
                }

                if !waiting {
                    tracing::info!(workflow_store = %path.display(), "workflow store busy; waiting for availability");
                    if let Some(observer) = wait_observer {
                        observer(path);
                    }

                    waiting = true;
                }

                thread::sleep(OPEN_RETRY_BACKOFF);
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// Decode a plain JSON value from table bytes.
fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Decode the payload of a content-addressed object envelope.
fn decode_object<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let envelope: serde_json::Value = serde_json::from_slice(bytes)?;
    let payload = envelope
        .get("payload")
        .cloned()
        .ok_or(Error::MissingPayload)?;
    Ok(serde_json::from_value(payload)?)
}

fn role_session_key(run_id: &str, role_id: &str) -> String {
    format!("{run_id}:{role_id}")
}

impl RunStore for RedbRunStore {
    fn save_run(&self, run: &WorkflowRun) -> cowboy_workflow_core::Result<()> {
        RedbRunStore::save_run(self, run).map_err(Into::into)
    }

    fn load_run(&self, run_id: &RunId) -> cowboy_workflow_core::Result<WorkflowRun> {
        RedbRunStore::load_run(self, run_id).map_err(Into::into)
    }

    fn list_runs(&self) -> cowboy_workflow_core::Result<Vec<RunHead>> {
        RedbRunStore::list_runs(self).map_err(Into::into)
    }

    fn put_object<T: Serialize>(
        &self,
        kind: ObjectKind,
        value: &T,
    ) -> cowboy_workflow_core::Result<ObjectHash> {
        RedbRunStore::put_object(self, kind, value).map_err(Into::into)
    }

    fn get_object<T: DeserializeOwned>(
        &self,
        hash: &ObjectHash,
    ) -> cowboy_workflow_core::Result<T> {
        RedbRunStore::get_object(self, hash).map_err(Into::into)
    }

    fn update_run_head(&self, run_id: &str, head: RunHead) -> cowboy_workflow_core::Result<()> {
        RedbRunStore::update_run_head(self, run_id, head).map_err(Into::into)
    }

    fn load_run_head(&self, run_id: &str) -> cowboy_workflow_core::Result<RunHead> {
        RedbRunStore::load_run_head(self, run_id).map_err(Into::into)
    }

    fn save_role_session(&self, session: RoleSession) -> cowboy_workflow_core::Result<()> {
        RedbRunStore::save_role_session(self, session).map_err(Into::into)
    }

    fn load_role_session(
        &self,
        run_id: &str,
        role_id: &str,
    ) -> cowboy_workflow_core::Result<Option<RoleSession>> {
        RedbRunStore::load_role_session(self, run_id, role_id).map_err(Into::into)
    }

    fn delete_role_sessions(&self, run_id: &str) -> cowboy_workflow_core::Result<()> {
        RedbRunStore::delete_role_sessions(self, run_id).map_err(Into::into)
    }

    fn append_turn(
        &self,
        run_id: &str,
        turn: TurnRecord,
    ) -> cowboy_workflow_core::Result<ObjectHash> {
        RedbRunStore::append_turn(self, run_id, turn).map_err(Into::into)
    }

    fn load_user_prompts(&self, run_id: &str) -> cowboy_workflow_core::Result<Vec<RunUserPrompt>> {
        RedbRunStore::load_user_prompts(self, run_id).map_err(Into::into)
    }

    fn open_agent_prompt_window(
        &self,
        window: AgentPromptWindow,
    ) -> cowboy_workflow_core::Result<OpenAgentPromptWindowOutcome> {
        RedbRunStore::open_agent_prompt_window(self, window).map_err(Into::into)
    }

    fn append_user_prompt(
        &self,
        run_id: &str,
        window_id: &str,
        content: String,
    ) -> cowboy_workflow_core::Result<AppendUserPromptOutcome> {
        RedbRunStore::append_user_prompt(self, run_id, window_id, content).map_err(Into::into)
    }

    fn compare_and_seal_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        applied_sequence: u64,
        sealed_at: DateTime<Utc>,
    ) -> cowboy_workflow_core::Result<CompareAndSealPromptWindowOutcome> {
        RedbRunStore::compare_and_seal_agent_prompt_window(
            self,
            run_id,
            window_id,
            applied_sequence,
            sealed_at,
        )
        .map_err(Into::into)
    }

    fn abort_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        aborted_at: DateTime<Utc>,
    ) -> cowboy_workflow_core::Result<AbortAgentPromptWindowOutcome> {
        RedbRunStore::abort_agent_prompt_window(self, run_id, window_id, aborted_at)
            .map_err(Into::into)
    }

    fn clear_agent_prompt_window(
        &self,
        run_id: &str,
    ) -> cowboy_workflow_core::Result<Option<AgentPromptWindow>> {
        RedbRunStore::clear_agent_prompt_window(self, run_id).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use cowboy_workflow_core::{
        RoleSession, RunStatus, StepDetail, StepInput, StepOutput, StepRecord,
        WorkflowSourceSnapshot,
    };
    use serde_json::Value;

    use super::*;
    use cowboy_workflow_core::RunHeadSummary;

    fn store() -> (tempfile::NamedTempFile, RedbRunStore) {
        let file = tempfile::NamedTempFile::new().unwrap();
        let store = RedbRunStore::create(file.path()).unwrap();
        (file, store)
    }

    fn run() -> WorkflowRun {
        let now = Utc::now();
        WorkflowRun {
            id: "run-1".into(),
            workflow_name: "wf".into(),
            workflow_api_version: 1,
            workflow_hash: "hash".into(),
            workflow_sources: Default::default(),
            original_request: "do it".into(),
            request_topic: None,
            config_set: Default::default(),
            status: RunStatus::Running,
            retries_used: 0,
            step_retries_used: Default::default(),
            current_step: "step-1".into(),
            head: None,
            resume: Value::Null,
            steps_executed: 0,
            step_visits: Default::default(),
            active_duration_ms: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn prompt_window(run_id: &str, window_id: &str) -> AgentPromptWindow {
        AgentPromptWindow {
            window_id: window_id.to_string(),
            run_id: run_id.to_string(),
            step_record_id: "record-1".to_string(),
            step_id: "step-1".to_string(),
            role_id: "developer".to_string(),
            baseline_sequence: 0,
            applied_sequence: 0,
            opened_at: Utc::now(),
            sealed_at: None,
        }
    }

    fn step_record() -> StepRecord {
        let now = Utc::now();
        StepRecord {
            id: "record-1".into(),
            prev: None,
            step: "step-1".into(),
            action: "status".into(),
            input: StepInput {
                prompt: None,
                context: Value::Null,
            },
            output: Some(StepOutput {
                status: "success".into(),
                fields: Value::Null,
                body: String::new(),
                raw: Value::Null,
            }),
            detail: StepDetail {
                backend: None,
                session_id: None,
                duration_ms: 0,
                turn_count: 0,
                usage: None,
            },
            started_at: now,
            completed_at: Some(now),
        }
    }

    #[test]
    fn stores_and_loads_run() {
        let (_file, store) = store();
        let run = run();
        store.save_run(&run).unwrap();
        assert_eq!(store.load_run(&run.id).unwrap(), run);
    }

    #[test]
    fn stores_and_loads_run_with_named_config_set_ref() {
        let (_file, store) = store();
        let mut run = run();
        run.config_set = cowboy_workflow_core::ConfigSetRef {
            name: "careful".into(),
        };
        store.save_run(&run).unwrap();
        let loaded = store.load_run(&run.id).unwrap();
        assert_eq!(loaded, run);
        assert_eq!(loaded.config_set.name, "careful");
    }

    #[test]
    fn stores_and_loads_objects_by_hash() {
        let (_file, store) = store();
        let record = step_record();
        let hash = store.put_object(ObjectKind::StepRecord, &record).unwrap();
        assert_eq!(store.get_object::<StepRecord>(&hash).unwrap(), record);
    }

    #[test]
    fn persists_run_heads() {
        let (_file, store) = store();
        let now = Utc::now();
        let head = RunHead {
            run_id: "run-1".into(),
            workflow_hash: "wf-hash".into(),
            head_step: Some("step-hash".into()),
            status: RunStatus::Completed,
            updated_at: now,
            summary: Some(RunHeadSummary {
                workflow_name: "wf".into(),
                request_topic: Some("topic".into()),
                current_step: "step-1".into(),
            }),
        };
        store.update_run_head("run-1", head.clone()).unwrap();
        assert_eq!(store.load_run_head("run-1").unwrap(), head);
        assert_eq!(store.list_runs().unwrap(), vec![head]);
    }

    #[test]
    fn persists_role_sessions_by_run_and_role() {
        let (_file, store) = store();
        let now = Utc::now();
        let session = RoleSession {
            run_id: "run-1".into(),
            role_id: "developer".into(),
            backend: "acp".into(),
            session_id: "session-1".into(),
            updated_at: now,
        };

        store.save_role_session(session.clone()).unwrap();

        assert_eq!(
            store.load_role_session("run-1", "developer").unwrap(),
            Some(session)
        );
        assert_eq!(store.load_role_session("run-1", "reviewer").unwrap(), None);
    }

    #[test]
    fn deletes_role_sessions_for_run() {
        let (_file, store) = store();
        let now = Utc::now();
        let session = RoleSession {
            run_id: "run-1".into(),
            role_id: "developer".into(),
            backend: "acp".into(),
            session_id: "session-1".into(),
            updated_at: now,
        };
        store.save_role_session(session).unwrap();

        store.delete_role_sessions("run-1").unwrap();

        assert_eq!(store.load_role_session("run-1", "developer").unwrap(), None);
    }

    #[test]
    fn appends_turns_and_stores_turn_objects() {
        let (_file, store) = store();
        let now = Utc::now();
        let turn = TurnRecord {
            id: "turn-1".into(),
            step_id: "record-1".into(),
            role: "assistant".into(),
            content: "hello".into(),
            timestamp: now,
            prev: None,
        };
        let hash = store.append_turn("run-1", turn.clone()).unwrap();
        assert_eq!(store.get_object::<TurnRecord>(&hash).unwrap(), turn);
    }

    #[test]
    fn stores_workflow_source_snapshot() {
        let (_file, store) = store();
        let bundle = WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".into(),
            files: [("main.lua".into(), "return workflow('x', step('s'))".into())]
                .into_iter()
                .collect(),
        };
        let hash = store
            .put_object(ObjectKind::WorkflowSourceSnapshot, &bundle)
            .unwrap();
        assert_eq!(
            store.get_object::<WorkflowSourceSnapshot>(&hash).unwrap(),
            bundle
        );
    }

    #[test]
    fn committed_data_survives_reopen() {
        let (file, store) = store();
        let run = run();
        let record = step_record();
        let record_hash = store.put_object(ObjectKind::StepRecord, &record).unwrap();
        let head = RunHead {
            run_id: run.id.clone(),
            workflow_hash: run.workflow_hash.clone(),
            head_step: Some(record_hash.clone()),
            status: RunStatus::Completed,
            updated_at: Utc::now(),
            summary: Some(RunHeadSummary::from_run(&run)),
        };
        store.save_run(&run).unwrap();
        store.update_run_head(&run.id, head.clone()).unwrap();
        drop(store);

        let reopened = RedbRunStore::open(file.path()).unwrap();
        assert_eq!(reopened.load_run(&run.id).unwrap(), run);
        assert_eq!(reopened.load_run_head(&head.run_id).unwrap(), head);
        assert_eq!(
            reopened.get_object::<StepRecord>(&record_hash).unwrap(),
            record
        );
    }

    #[test]
    fn live_store_value_does_not_keep_database_locked() {
        let (file, store_a) = store();
        let run_a = run();
        let head_a = RunHead {
            run_id: run_a.id.clone(),
            workflow_hash: run_a.workflow_hash.clone(),
            head_step: None,
            status: RunStatus::Running,
            updated_at: Utc::now(),
            summary: Some(RunHeadSummary::from_run(&run_a)),
        };
        store_a.save_run(&run_a).unwrap();
        store_a.update_run_head(&run_a.id, head_a.clone()).unwrap();

        let store_b = RedbRunStore::create(file.path()).unwrap();
        assert_eq!(store_b.load_run(&run_a.id).unwrap(), run_a);

        let mut run_b = run();
        run_b.id = "run-2".into();
        let head_b = RunHead {
            run_id: run_b.id.clone(),
            workflow_hash: run_b.workflow_hash.clone(),
            head_step: None,
            status: RunStatus::Running,
            updated_at: Utc::now(),
            summary: Some(RunHeadSummary::from_run(&run_b)),
        };
        store_b.save_run(&run_b).unwrap();
        store_b.update_run_head(&run_b.id, head_b.clone()).unwrap();

        assert_eq!(store_a.load_run(&run_b.id).unwrap(), run_b);

        let mut run_ids = store_b
            .list_runs()
            .unwrap()
            .into_iter()
            .map(|head| head.run_id)
            .collect::<Vec<_>>();
        run_ids.sort();
        assert_eq!(run_ids, vec!["run-1", "run-2"]);
    }

    #[test]
    fn transient_database_contention_outlasting_retry_window_does_not_fail() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let holder_path = path.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let holder = thread::spawn(move || {
            let database = Database::create(holder_path).unwrap();
            locked_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(750));
            drop(database);
        });
        locked_rx.recv().unwrap();

        let store = RedbRunStore::create(&path);

        holder.join().unwrap();
        store.expect("transient database contention should wait for the lock to be released");
    }

    #[test]
    fn store_wait_cancellation_interrupts_contention() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let holder_path = path.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let holder = thread::spawn(move || {
            let database = Database::create(holder_path).unwrap();
            locked_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(250));
            drop(database);
        });
        locked_rx.recv().unwrap();
        let generation = Arc::new(AtomicU64::new(0));
        let cancellation = StoreWaitCancellation::new(generation.clone(), 0);
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let observer: StoreWaitObserver = Arc::new(move |_| {
            waiting_tx.send(()).unwrap();
        });
        let store = RedbRunStore::lazy(&path)
            .with_wait_observer(observer)
            .with_wait_cancellation(cancellation);
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let waiter = thread::spawn(move || {
            result_tx.send(store.list_runs()).unwrap();
        });
        waiting_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        generation.fetch_add(1, Ordering::AcqRel);
        let result = result_rx.recv_timeout(Duration::from_millis(100));

        holder.join().unwrap();
        waiter.join().unwrap();
        let result = result.expect("cancelled store wait should finish promptly");
        assert!(matches!(result, Err(Error::WaitCancelled)), "{result:?}");
    }

    #[test]
    fn store_wait_observer_fires_once_when_contention_starts() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let holder_path = path.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let holder = thread::spawn(move || {
            let database = Database::create(holder_path).unwrap();
            locked_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(200));
            drop(database);
        });
        locked_rx.recv().unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observer_calls = calls.clone();
        let observer_paths = observed_paths.clone();
        let observer: StoreWaitObserver = Arc::new(move |path| {
            observer_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            observer_paths.lock().unwrap().push(path.to_path_buf());
        });

        let store = RedbRunStore::create_with_wait_observer(&path, observer);

        holder.join().unwrap();
        store.expect("observer-backed store should wait for contention to clear");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(*observed_paths.lock().unwrap(), vec![path]);
    }

    #[test]
    fn create_accepts_existing_valid_database_path() {
        let (file, store) = store();
        let run = run();
        store.save_run(&run).unwrap();

        let reopened_with_create = RedbRunStore::create(file.path()).unwrap();

        assert_eq!(reopened_with_create.load_run(&run.id).unwrap(), run);
    }

    #[test]
    fn deletes_run_but_leaves_immutable_objects() {
        let (_file, store) = store();
        let run = run();
        let record = step_record();
        let record_hash = store.put_object(ObjectKind::StepRecord, &record).unwrap();
        let head = RunHead {
            run_id: run.id.clone(),
            workflow_hash: run.workflow_hash.clone(),
            head_step: Some(record_hash.clone()),
            status: RunStatus::Completed,
            updated_at: Utc::now(),
            summary: Some(RunHeadSummary::from_run(&run)),
        };
        store.save_run(&run).unwrap();
        store.update_run_head(&run.id, head).unwrap();

        store.delete_run(&run.id).unwrap();

        assert!(matches!(
            store.load_run(&run.id),
            Err(Error::RunNotFound(_))
        ));
        assert!(matches!(
            store.load_run_head(&run.id),
            Err(Error::RunNotFound(_))
        ));
        assert_eq!(
            store.get_object::<StepRecord>(&record_hash).unwrap(),
            record
        );
    }

    #[test]
    fn deletes_object() {
        let (_file, store) = store();
        let record = step_record();
        let hash = store.put_object(ObjectKind::StepRecord, &record).unwrap();

        store.delete_object(&hash).unwrap();

        assert!(matches!(
            store.get_object::<StepRecord>(&hash),
            Err(Error::ObjectNotFound(_))
        ));
    }

    #[test]
    fn prompt_append_and_compare_and_seal_are_totally_ordered() {
        let (file, store) = store();
        let run = run();
        store.save_run(&run).unwrap();
        assert_eq!(store.load_user_prompts(&run.id).unwrap(), Vec::new());

        let opened = store
            .open_agent_prompt_window(prompt_window(&run.id, "window-1"))
            .unwrap();
        assert!(matches!(opened, OpenAgentPromptWindowOutcome::Opened(_)));
        let content = "  correction\nwith spacing  ".to_string();
        let accepted_not_before = Utc::now().timestamp_millis();
        let accepted = store
            .append_user_prompt(&run.id, "window-1", content.clone())
            .unwrap();
        let AppendUserPromptOutcome::Accepted(prompt) = accepted else {
            panic!("expected accepted prompt")
        };
        assert_eq!(prompt.sequence, 1);
        assert_eq!(prompt.content, content);
        assert!(prompt.submitted_at.timestamp_millis() >= accepted_not_before);
        assert!(prompt.submitted_at.timestamp_millis() <= Utc::now().timestamp_millis());

        let pending = store
            .compare_and_seal_agent_prompt_window(&run.id, "window-1", 0, Utc::now())
            .unwrap();
        assert!(matches!(
            pending,
            CompareAndSealPromptWindowOutcome::Pending { prompts, .. }
                if prompts == vec![prompt.clone()]
        ));
        let sealed = store
            .compare_and_seal_agent_prompt_window(&run.id, "window-1", 1, Utc::now())
            .unwrap();
        assert!(matches!(
            sealed,
            CompareAndSealPromptWindowOutcome::Sealed(_)
        ));
        assert_eq!(
            store
                .append_user_prompt(&run.id, "window-1", "too late".to_string())
                .unwrap(),
            AppendUserPromptOutcome::SealedWindow
        );

        let reopened = RedbRunStore::open(file.path()).unwrap();
        assert_eq!(reopened.load_user_prompts(&run.id).unwrap(), vec![prompt]);
    }

    #[test]
    fn prompt_submission_rejects_missing_terminal_stale_and_absent_windows_without_writing() {
        let (_file, store) = store();
        assert_eq!(
            store
                .append_user_prompt("missing", "window", "text".to_string())
                .unwrap(),
            AppendUserPromptOutcome::MissingRun
        );
        let mut run = run();
        store.save_run(&run).unwrap();
        assert_eq!(
            store
                .append_user_prompt(&run.id, "window", "text".to_string())
                .unwrap(),
            AppendUserPromptOutcome::NoWindow
        );
        store
            .open_agent_prompt_window(prompt_window(&run.id, "current"))
            .unwrap();
        assert_eq!(
            store
                .append_user_prompt(&run.id, "stale", "text".to_string())
                .unwrap(),
            AppendUserPromptOutcome::StaleWindow
        );
        run.status = RunStatus::Completed;
        store.save_run(&run).unwrap();
        assert_eq!(
            store
                .append_user_prompt(&run.id, "current", "text".to_string())
                .unwrap(),
            AppendUserPromptOutcome::TerminalRun
        );
        assert!(store.load_user_prompts(&run.id).unwrap().is_empty());
    }

    #[test]
    fn replacing_and_clearing_windows_invalidates_old_tokens_and_delete_removes_prompt_indexes() {
        let (_file, store) = store();
        let run = run();
        store.save_run(&run).unwrap();
        store
            .open_agent_prompt_window(prompt_window(&run.id, "old"))
            .unwrap();
        store
            .open_agent_prompt_window(prompt_window(&run.id, "new"))
            .unwrap();
        assert_eq!(
            store
                .append_user_prompt(&run.id, "old", "stale".to_string())
                .unwrap(),
            AppendUserPromptOutcome::StaleWindow
        );
        assert_eq!(
            store
                .clear_agent_prompt_window(&run.id)
                .unwrap()
                .unwrap()
                .window_id,
            "new"
        );
        assert_eq!(
            store
                .append_user_prompt(&run.id, "new", "closed".to_string())
                .unwrap(),
            AppendUserPromptOutcome::NoWindow
        );

        store
            .open_agent_prompt_window(prompt_window(&run.id, "final"))
            .unwrap();
        let accepted = store
            .append_user_prompt(&run.id, "final", "saved".to_string())
            .unwrap();
        assert!(matches!(accepted, AppendUserPromptOutcome::Accepted(_)));
        store.delete_run(&run.id).unwrap();
        assert!(store.load_user_prompts(&run.id).unwrap().is_empty());
        assert!(store.clear_agent_prompt_window(&run.id).unwrap().is_none());
    }
}
