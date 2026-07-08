use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use cowboy_workflow_core::{
    ObjectHash, ObjectKind, RoleSession, RunHead, RunId, RunStore, TurnRecord, WorkflowRun,
};
use redb::{Database, ReadableDatabase, ReadableTable};
use serde::{Serialize, de::DeserializeOwned};

use crate::hash::{canonical_object_bytes, object_hash};
use crate::tables::{OBJECTS, ROLE_SESSIONS, RUN_HEADS, RUN_TURNS, RUNS};
use crate::{Error, Result};

const OPEN_RETRY_ATTEMPTS: usize = 20;
const OPEN_RETRY_BACKOFF: Duration = Duration::from_millis(25);

/// redb-backed implementation of workflow run storage.
///
/// The store keeps immutable content-addressed objects in `OBJECTS` and mutable
/// run state in dedicated tables. The value is path-backed and opens redb only
/// inside each operation, so idle Cowboy processes do not retain redb's
/// exclusive writable database lock.
#[derive(Clone)]
pub struct RedbRunStore {
    /// Path to the redb workflow database.
    path: PathBuf,
}

impl RedbRunStore {
    /// Create or open a database at `path`, then drop the validation handle.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        ensure_parent_dir(&path)?;
        drop(open_database_with_retry(&path, |path| {
            Database::create(path)
        })?);
        Ok(Self { path })
    }

    /// Open an existing database at `path`, then drop the validation handle.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        drop(open_database_with_retry(&path, |path| {
            Database::open(path)
        })?);
        Ok(Self { path })
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
            let table = read.open_table(RUNS)?;
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

    /// Delete a run snapshot, run head, and indexed turn lists for the run.
    ///
    /// Immutable objects are intentionally left in `OBJECTS`; later garbage
    /// collection can remove unreferenced objects once we track reachability.
    pub fn delete_run(&self, run_id: &str) -> Result<()> {
        self.with_write(|write| {
            write.open_table(RUNS)?.remove(run_id)?;
            write.open_table(RUN_HEADS)?.remove(run_id)?;

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
        open_database_with_retry(&self.path, |path| Database::create(path))
    }
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn open_database_with_retry(
    path: &Path,
    mut open: impl FnMut(&Path) -> std::result::Result<Database, redb::DatabaseError>,
) -> Result<Database> {
    for attempt in 0..=OPEN_RETRY_ATTEMPTS {
        match open(path) {
            Ok(db) => return Ok(db),
            Err(redb::DatabaseError::DatabaseAlreadyOpen) if attempt < OPEN_RETRY_ATTEMPTS => {
                thread::sleep(OPEN_RETRY_BACKOFF);
            }
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => {
                return Err(Error::TemporarilyBusy(path.to_path_buf()));
            }
            Err(err) => return Err(err.into()),
        }
    }

    Err(Error::TemporarilyBusy(path.to_path_buf()))
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
            status: RunStatus::Running,
            current_step: "step-1".into(),
            head: None,
            resume: Value::Null,
            steps_executed: 0,
            step_visits: Default::default(),
            created_at: now,
            updated_at: now,
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
}
