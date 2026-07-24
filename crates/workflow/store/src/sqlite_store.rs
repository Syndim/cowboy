use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cowboy_workflow_core::{
    AbortAgentPromptWindowOutcome, AgentPromptWindow, AgentSessionStore, AppendUserPromptOutcome,
    CompareAndSealPromptWindowOutcome, ObjectHash, ObjectKind, OpenAgentPromptWindowOutcome,
    PromptWindowStore, RoleSession, RunHead, RunId, RunStatus, RunUserPrompt, StepRecord,
    TurnRecord, TurnStore, UserPromptStore, WorkflowObjectStore, WorkflowRun,
    WorkflowSourceSnapshot, WorkflowStateStore,
};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use tokio::sync::watch;

use crate::hash::{canonical_object_bytes, object_hash};
use crate::{Error, Result, schema};

const RETRY_BACKOFF: Duration = Duration::from_millis(25);

pub type StoreWaitObserver = Arc<dyn Fn(&Path) + Send + Sync + 'static>;

#[derive(Clone)]
pub struct StoreWaitCancellation {
    receiver: watch::Receiver<u64>,
    expected_generation: u64,
}

impl StoreWaitCancellation {
    pub fn new(receiver: watch::Receiver<u64>) -> Self {
        let expected_generation = *receiver.borrow();
        Self {
            receiver,
            expected_generation,
        }
    }

    async fn cancelled(&mut self) {
        loop {
            if *self.receiver.borrow_and_update() != self.expected_generation {
                return;
            }
            if self.receiver.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[derive(Clone)]
pub struct SqliteWorkflowStore {
    path: PathBuf,
    pool: SqlitePool,
    wait_observer: Option<StoreWaitObserver>,
    wait_cancellation: Option<StoreWaitCancellation>,
    #[cfg(test)]
    fail_completed_step_before_commit: Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for SqliteWorkflowStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteWorkflowStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SqliteWorkflowStore {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let pool = schema::connect(&path).await?;
        Ok(Self {
            path,
            pool,
            wait_observer: None,
            wait_cancellation: None,
            #[cfg(test)]
            fail_completed_step_before_commit: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    pub fn with_wait_observer(&self, observer: StoreWaitObserver) -> Self {
        let mut store = self.clone();
        store.wait_observer = Some(observer);
        store
    }

    pub fn with_wait_cancellation(&self, cancellation: StoreWaitCancellation) -> Self {
        let mut store = self.clone();
        store.wait_cancellation = Some(cancellation);
        store
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    async fn retry_write<T, F, Fut>(&self, mut operation: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let mut notified = false;
        loop {
            match operation().await {
                Ok(value) => return Ok(value),
                Err(Error::Sqlx(error)) if is_retryable_sqlx_error(&error) => {
                    if !notified {
                        tracing::info!("workflow store busy; waiting for availability");
                        if let Some(observer) = &self.wait_observer {
                            observer(&self.path);
                        }
                        notified = true;
                    }
                    if let Some(cancellation) = &self.wait_cancellation {
                        let mut cancellation = cancellation.clone();
                        tokio::select! {
                            () = tokio::time::sleep(RETRY_BACKOFF) => {}
                            () = cancellation.cancelled() => return Err(Error::WaitCancelled),
                        }
                    } else {
                        tokio::time::sleep(RETRY_BACKOFF).await;
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn save_run(&self, run: &WorkflowRun) -> Result<()> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            upsert_run_and_head(&mut tx, run).await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    pub async fn load_run(&self, run_id: &str) -> Result<WorkflowRun> {
        let data = sqlx::query_scalar::<_, Vec<u8>>("SELECT data FROM runs WHERE run_id = ?")
            .bind(run_id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::RunNotFound(run_id.to_string()))?;
        decode_json(&data)
    }

    pub async fn list_runs(&self) -> Result<Vec<RunHead>> {
        let rows = sqlx::query("SELECT data FROM run_heads ORDER BY run_id")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| decode_json(&row.get::<Vec<u8>, _>(0)))
            .collect()
    }

    pub async fn load_run_head(&self, run_id: &str) -> Result<RunHead> {
        let data = sqlx::query_scalar::<_, Vec<u8>>("SELECT data FROM run_heads WHERE run_id = ?")
            .bind(run_id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::RunNotFound(run_id.to_string()))?;
        decode_json(&data)
    }

    pub async fn commit_completed_step(
        &self,
        run: &WorkflowRun,
        record: &StepRecord,
    ) -> Result<ObjectHash> {
        let hash = object_hash(ObjectKind::StepRecord, record)?;
        self.retry_write(|| {
            let hash = hash.clone();
            async move {
                let mut tx = self.pool.begin().await?;
                put_object_in_tx(&mut tx, ObjectKind::StepRecord, record).await?;
                let mut stored_run = run.clone();
                stored_run.head = Some(hash.clone());
                upsert_run_and_head(&mut tx, &stored_run).await?;
                #[cfg(test)]
                if self
                    .fail_completed_step_before_commit
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(Error::InjectedFailure);
                }
                tx.commit().await?;
                Ok(hash)
            }
        })
        .await
    }

    pub async fn store_workflow_source_snapshot(
        &self,
        snapshot: &WorkflowSourceSnapshot,
    ) -> Result<ObjectHash> {
        self.store_object(ObjectKind::WorkflowSourceSnapshot, snapshot)
            .await
    }

    pub async fn load_workflow_source_snapshot(
        &self,
        hash: &ObjectHash,
    ) -> Result<WorkflowSourceSnapshot> {
        self.load_object(hash).await
    }

    pub async fn store_step_record(&self, record: &StepRecord) -> Result<ObjectHash> {
        self.store_object(ObjectKind::StepRecord, record).await
    }

    pub async fn load_step_record(&self, hash: &ObjectHash) -> Result<StepRecord> {
        self.load_object(hash).await
    }

    async fn store_object<T: Serialize>(&self, kind: ObjectKind, value: &T) -> Result<ObjectHash> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            let hash = put_object_in_tx(&mut tx, kind, value).await?;
            tx.commit().await?;
            Ok(hash)
        })
        .await
    }

    async fn load_object<T: DeserializeOwned>(&self, hash: &ObjectHash) -> Result<T> {
        let data = sqlx::query_scalar::<_, Vec<u8>>("SELECT data FROM objects WHERE hash = ?")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::ObjectNotFound(hash.clone()))?;
        decode_object(&data)
    }

    pub async fn save_role_session(&self, session: RoleSession) -> Result<()> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            sqlx::query(
                "INSERT INTO role_sessions(run_id, role_id, data) VALUES(?, ?, ?) \
                 ON CONFLICT(run_id, role_id) DO UPDATE SET data=excluded.data",
            )
            .bind(&session.run_id)
            .bind(&session.role_id)
            .bind(serde_json::to_vec(&session)?)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    pub async fn load_role_session(
        &self,
        run_id: &str,
        role_id: &str,
    ) -> Result<Option<RoleSession>> {
        sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT data FROM role_sessions WHERE run_id = ? AND role_id = ?",
        )
        .bind(run_id)
        .bind(role_id)
        .fetch_optional(&self.pool)
        .await?
        .map(|data| decode_json(&data))
        .transpose()
    }

    pub async fn delete_role_sessions(&self, run_id: &str) -> Result<()> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            sqlx::query("DELETE FROM role_sessions WHERE run_id = ?")
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    pub async fn append_turn(&self, run_id: &str, turn: TurnRecord) -> Result<ObjectHash> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            let hash = put_object_in_tx(&mut tx, ObjectKind::TurnRecord, &turn).await?;
            let position: i64 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(position) + 1, 0) FROM run_turns WHERE run_id = ? AND step_record_id = ?",
            )
            .bind(run_id)
            .bind(&turn.step_id)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO run_turns(run_id, step_record_id, position, object_hash) VALUES(?, ?, ?, ?)",
            )
            .bind(run_id)
            .bind(&turn.step_id)
            .bind(position)
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(hash)
        })
        .await
    }

    pub async fn load_turn(&self, hash: &ObjectHash) -> Result<TurnRecord> {
        self.load_object(hash).await
    }

    pub async fn load_turns(&self, run_id: &str, step_record_id: &str) -> Result<Vec<TurnRecord>> {
        let rows = sqlx::query(
            "SELECT o.data FROM run_turns t JOIN objects o ON o.hash=t.object_hash \
             WHERE t.run_id=? AND t.step_record_id=? ORDER BY t.position",
        )
        .bind(run_id)
        .bind(step_record_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_object(&row.get::<Vec<u8>, _>(0)))
            .collect()
    }

    pub async fn load_user_prompts(&self, run_id: &str) -> Result<Vec<RunUserPrompt>> {
        let rows =
            sqlx::query("SELECT data FROM run_user_prompts WHERE run_id = ? ORDER BY sequence")
                .bind(run_id)
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|row| decode_json(&row.get::<Vec<u8>, _>(0)))
            .collect()
    }

    pub async fn open_agent_prompt_window(
        &self,
        window: AgentPromptWindow,
    ) -> Result<OpenAgentPromptWindowOutcome> {
        self.retry_write(|| {
            let mut window = window.clone();
            async move {
                let mut tx = self.pool.begin().await?;
                let Some(run) = load_run_in_tx(&mut tx, &window.run_id).await? else {
                    return Ok(OpenAgentPromptWindowOutcome::MissingRun);
                };
                if !matches!(run.status, RunStatus::Running) {
                    return Ok(OpenAgentPromptWindowOutcome::TerminalRun);
                }
                let baseline: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(MAX(sequence), 0) FROM run_user_prompts WHERE run_id = ?",
                )
                .bind(&window.run_id)
                .fetch_one(&mut *tx)
                .await?;
                window.baseline_sequence = baseline as u64;
                window.applied_sequence = baseline as u64;
                window.sealed_at = None;
                sqlx::query(
                    "INSERT INTO agent_prompt_windows(run_id, window_id, data) VALUES(?, ?, ?) \
                     ON CONFLICT(run_id) DO UPDATE SET window_id=excluded.window_id, data=excluded.data",
                )
                .bind(&window.run_id)
                .bind(&window.window_id)
                .bind(serde_json::to_vec(&window)?)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(OpenAgentPromptWindowOutcome::Opened(window))
            }
        })
        .await
    }

    pub async fn append_user_prompt(
        &self,
        run_id: &str,
        window_id: &str,
        content: String,
    ) -> Result<AppendUserPromptOutcome> {
        self.retry_write(|| {
            let content = content.clone();
            async move {
                let mut tx = self.pool.begin().await?;
                let Some(run) = load_run_in_tx(&mut tx, run_id).await? else {
                    return Ok(AppendUserPromptOutcome::MissingRun);
                };
                if !matches!(run.status, RunStatus::Running) {
                    return Ok(AppendUserPromptOutcome::TerminalRun);
                }
                let Some(window) = load_window_in_tx(&mut tx, run_id).await? else {
                    return Ok(AppendUserPromptOutcome::NoWindow);
                };
                if window.window_id != window_id {
                    return Ok(AppendUserPromptOutcome::StaleWindow);
                }
                if !window.is_open() {
                    return Ok(AppendUserPromptOutcome::SealedWindow);
                }
                let sequence: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(MAX(sequence) + 1, 1) FROM run_user_prompts WHERE run_id = ?",
                )
                .bind(run_id)
                .fetch_one(&mut *tx)
                .await?;
                let now = Utc::now();
                let prompt = RunUserPrompt {
                    sequence: sequence as u64,
                    content,
                    submitted_at: DateTime::from_timestamp_millis(now.timestamp_millis())
                        .expect("valid UTC timestamp milliseconds"),
                };
                sqlx::query("INSERT INTO run_user_prompts(run_id, sequence, data) VALUES(?, ?, ?)")
                    .bind(run_id)
                    .bind(sequence)
                    .bind(serde_json::to_vec(&prompt)?)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(AppendUserPromptOutcome::Accepted(prompt))
            }
        })
        .await
    }

    pub async fn compare_and_seal_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        applied_sequence: u64,
        sealed_at: DateTime<Utc>,
    ) -> Result<CompareAndSealPromptWindowOutcome> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            let Some(run) = load_run_in_tx(&mut tx, run_id).await? else {
                return Ok(CompareAndSealPromptWindowOutcome::MissingRun);
            };
            if !matches!(run.status, RunStatus::Running) {
                return Ok(CompareAndSealPromptWindowOutcome::TerminalRun);
            }
            let Some(mut window) = load_window_in_tx(&mut tx, run_id).await? else {
                return Ok(CompareAndSealPromptWindowOutcome::NoWindow);
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
            let prompts = load_prompts_in_tx(&mut tx, run_id).await?;
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
            save_window_in_tx(&mut tx, &window).await?;
            tx.commit().await?;
            if pending.is_empty() {
                Ok(CompareAndSealPromptWindowOutcome::Sealed(window))
            } else {
                Ok(CompareAndSealPromptWindowOutcome::Pending {
                    window,
                    prompts: pending,
                })
            }
        })
        .await
    }

    pub async fn abort_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        aborted_at: DateTime<Utc>,
    ) -> Result<AbortAgentPromptWindowOutcome> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            if load_run_in_tx(&mut tx, run_id).await?.is_none() {
                return Ok(AbortAgentPromptWindowOutcome::MissingRun);
            }
            let Some(mut window) = load_window_in_tx(&mut tx, run_id).await? else {
                return Ok(AbortAgentPromptWindowOutcome::NoWindow);
            };
            if window.window_id != window_id {
                return Ok(AbortAgentPromptWindowOutcome::StaleWindow);
            }
            window.sealed_at.get_or_insert(aborted_at);
            save_window_in_tx(&mut tx, &window).await?;
            tx.commit().await?;
            Ok(AbortAgentPromptWindowOutcome::Aborted(window))
        })
        .await
    }

    pub async fn clear_agent_prompt_window(
        &self,
        run_id: &str,
    ) -> Result<Option<AgentPromptWindow>> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            let window = load_window_in_tx(&mut tx, run_id).await?;
            sqlx::query("DELETE FROM agent_prompt_windows WHERE run_id = ?")
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(window)
        })
        .await
    }

    pub async fn delete_run(&self, run_id: &str) -> Result<()> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            for table in [
                "runs",
                "run_heads",
                "role_sessions",
                "run_turns",
                "run_user_prompts",
                "agent_prompt_windows",
            ] {
                let query = format!("DELETE FROM {table} WHERE run_id = ?");
                sqlx::query(&query).bind(run_id).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    pub async fn delete_object(&self, hash: &ObjectHash) -> Result<()> {
        self.retry_write(|| async {
            let mut tx = self.pool.begin().await?;
            sqlx::query("DELETE FROM objects WHERE hash = ?")
                .bind(hash)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }
}

async fn upsert_run_and_head(tx: &mut Transaction<'_, Sqlite>, run: &WorkflowRun) -> Result<()> {
    sqlx::query(
        "INSERT INTO runs(run_id, data) VALUES(?, ?) \
         ON CONFLICT(run_id) DO UPDATE SET data=excluded.data",
    )
    .bind(&run.id)
    .bind(serde_json::to_vec(run)?)
    .execute(&mut **tx)
    .await?;
    let head = RunHead::from_run(run);
    sqlx::query(
        "INSERT INTO run_heads(run_id, data) VALUES(?, ?) \
         ON CONFLICT(run_id) DO UPDATE SET data=excluded.data",
    )
    .bind(&run.id)
    .bind(serde_json::to_vec(&head)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn put_object_in_tx<T: Serialize>(
    tx: &mut Transaction<'_, Sqlite>,
    kind: ObjectKind,
    value: &T,
) -> Result<ObjectHash> {
    let hash = object_hash(kind, value)?;
    let data = canonical_object_bytes(kind, value)?;
    if let Some(existing) =
        sqlx::query_scalar::<_, Vec<u8>>("SELECT data FROM objects WHERE hash = ?")
            .bind(&hash)
            .fetch_optional(&mut **tx)
            .await?
    {
        if existing != data {
            return Err(Error::HashCollision(hash));
        }
        return Ok(hash);
    }
    sqlx::query("INSERT OR IGNORE INTO objects(hash, kind, data) VALUES(?, ?, ?)")
        .bind(&hash)
        .bind(serde_json::to_string(&kind)?)
        .bind(data)
        .execute(&mut **tx)
        .await?;
    Ok(hash)
}

async fn load_run_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> Result<Option<WorkflowRun>> {
    sqlx::query_scalar::<_, Vec<u8>>("SELECT data FROM runs WHERE run_id = ?")
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|data| decode_json(&data))
        .transpose()
}

async fn load_window_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> Result<Option<AgentPromptWindow>> {
    sqlx::query_scalar::<_, Vec<u8>>("SELECT data FROM agent_prompt_windows WHERE run_id = ?")
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|data| decode_json(&data))
        .transpose()
}

async fn save_window_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    window: &AgentPromptWindow,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO agent_prompt_windows(run_id, window_id, data) VALUES(?, ?, ?) \
         ON CONFLICT(run_id) DO UPDATE SET window_id=excluded.window_id, data=excluded.data",
    )
    .bind(&window.run_id)
    .bind(&window.window_id)
    .bind(serde_json::to_vec(window)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn load_prompts_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> Result<Vec<RunUserPrompt>> {
    let rows = sqlx::query("SELECT data FROM run_user_prompts WHERE run_id = ? ORDER BY sequence")
        .bind(run_id)
        .fetch_all(&mut **tx)
        .await?;
    rows.into_iter()
        .map(|row| decode_json(&row.get::<Vec<u8>, _>(0)))
        .collect()
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(bytes)?)
}

fn decode_object<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let envelope: serde_json::Value = serde_json::from_slice(bytes)?;
    let payload = envelope
        .get("payload")
        .cloned()
        .ok_or(Error::MissingPayload)?;
    Ok(serde_json::from_value(payload)?)
}

fn is_retryable_sqlx_error(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(error) => is_retryable_sqlite_code(error.code().as_deref()),
        _ => false,
    }
}

pub fn is_retryable_sqlite_code(code: Option<&str>) -> bool {
    code.and_then(|code| code.parse::<i32>().ok())
        .is_some_and(|code| matches!(code & 0xff, 5 | 6))
}

#[async_trait]
impl WorkflowStateStore for SqliteWorkflowStore {
    async fn save_run(&self, run: &WorkflowRun) -> cowboy_workflow_core::Result<()> {
        SqliteWorkflowStore::save_run(self, run)
            .await
            .map_err(Into::into)
    }
    async fn load_run(&self, run_id: &RunId) -> cowboy_workflow_core::Result<WorkflowRun> {
        SqliteWorkflowStore::load_run(self, run_id)
            .await
            .map_err(Into::into)
    }
    async fn list_runs(&self) -> cowboy_workflow_core::Result<Vec<RunHead>> {
        SqliteWorkflowStore::list_runs(self)
            .await
            .map_err(Into::into)
    }
    async fn load_run_head(&self, run_id: &str) -> cowboy_workflow_core::Result<RunHead> {
        SqliteWorkflowStore::load_run_head(self, run_id)
            .await
            .map_err(Into::into)
    }
    async fn commit_completed_step(
        &self,
        run: &WorkflowRun,
        record: &StepRecord,
    ) -> cowboy_workflow_core::Result<ObjectHash> {
        SqliteWorkflowStore::commit_completed_step(self, run, record)
            .await
            .map_err(Into::into)
    }
    async fn delete_run(&self, run_id: &str) -> cowboy_workflow_core::Result<()> {
        SqliteWorkflowStore::delete_run(self, run_id)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl WorkflowObjectStore for SqliteWorkflowStore {
    async fn store_workflow_source_snapshot(
        &self,
        snapshot: &WorkflowSourceSnapshot,
    ) -> cowboy_workflow_core::Result<ObjectHash> {
        SqliteWorkflowStore::store_workflow_source_snapshot(self, snapshot)
            .await
            .map_err(Into::into)
    }
    async fn load_workflow_source_snapshot(
        &self,
        hash: &ObjectHash,
    ) -> cowboy_workflow_core::Result<WorkflowSourceSnapshot> {
        SqliteWorkflowStore::load_workflow_source_snapshot(self, hash)
            .await
            .map_err(Into::into)
    }
    async fn store_step_record(
        &self,
        record: &StepRecord,
    ) -> cowboy_workflow_core::Result<ObjectHash> {
        SqliteWorkflowStore::store_step_record(self, record)
            .await
            .map_err(Into::into)
    }
    async fn load_step_record(
        &self,
        hash: &ObjectHash,
    ) -> cowboy_workflow_core::Result<StepRecord> {
        SqliteWorkflowStore::load_step_record(self, hash)
            .await
            .map_err(Into::into)
    }
    async fn delete_object(&self, hash: &ObjectHash) -> cowboy_workflow_core::Result<()> {
        SqliteWorkflowStore::delete_object(self, hash)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl AgentSessionStore for SqliteWorkflowStore {
    async fn save_role_session(&self, session: RoleSession) -> cowboy_workflow_core::Result<()> {
        SqliteWorkflowStore::save_role_session(self, session)
            .await
            .map_err(Into::into)
    }
    async fn load_role_session(
        &self,
        run_id: &str,
        role_id: &str,
    ) -> cowboy_workflow_core::Result<Option<RoleSession>> {
        SqliteWorkflowStore::load_role_session(self, run_id, role_id)
            .await
            .map_err(Into::into)
    }
    async fn delete_role_sessions(&self, run_id: &str) -> cowboy_workflow_core::Result<()> {
        SqliteWorkflowStore::delete_role_sessions(self, run_id)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl TurnStore for SqliteWorkflowStore {
    async fn append_turn(
        &self,
        run_id: &str,
        turn: TurnRecord,
    ) -> cowboy_workflow_core::Result<ObjectHash> {
        SqliteWorkflowStore::append_turn(self, run_id, turn)
            .await
            .map_err(Into::into)
    }
    async fn load_turn(&self, hash: &ObjectHash) -> cowboy_workflow_core::Result<TurnRecord> {
        SqliteWorkflowStore::load_turn(self, hash)
            .await
            .map_err(Into::into)
    }
    async fn load_turns(
        &self,
        run_id: &str,
        step_record_id: &str,
    ) -> cowboy_workflow_core::Result<Vec<TurnRecord>> {
        SqliteWorkflowStore::load_turns(self, run_id, step_record_id)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl UserPromptStore for SqliteWorkflowStore {
    async fn load_user_prompts(
        &self,
        run_id: &str,
    ) -> cowboy_workflow_core::Result<Vec<RunUserPrompt>> {
        SqliteWorkflowStore::load_user_prompts(self, run_id)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl PromptWindowStore for SqliteWorkflowStore {
    async fn open_agent_prompt_window(
        &self,
        window: AgentPromptWindow,
    ) -> cowboy_workflow_core::Result<OpenAgentPromptWindowOutcome> {
        SqliteWorkflowStore::open_agent_prompt_window(self, window)
            .await
            .map_err(Into::into)
    }
    async fn append_user_prompt(
        &self,
        run_id: &str,
        window_id: &str,
        content: String,
    ) -> cowboy_workflow_core::Result<AppendUserPromptOutcome> {
        SqliteWorkflowStore::append_user_prompt(self, run_id, window_id, content)
            .await
            .map_err(Into::into)
    }
    async fn compare_and_seal_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        applied_sequence: u64,
        sealed_at: DateTime<Utc>,
    ) -> cowboy_workflow_core::Result<CompareAndSealPromptWindowOutcome> {
        SqliteWorkflowStore::compare_and_seal_agent_prompt_window(
            self,
            run_id,
            window_id,
            applied_sequence,
            sealed_at,
        )
        .await
        .map_err(Into::into)
    }
    async fn abort_agent_prompt_window(
        &self,
        run_id: &str,
        window_id: &str,
        aborted_at: DateTime<Utc>,
    ) -> cowboy_workflow_core::Result<AbortAgentPromptWindowOutcome> {
        SqliteWorkflowStore::abort_agent_prompt_window(self, run_id, window_id, aborted_at)
            .await
            .map_err(Into::into)
    }
    async fn clear_agent_prompt_window(
        &self,
        run_id: &str,
    ) -> cowboy_workflow_core::Result<Option<AgentPromptWindow>> {
        SqliteWorkflowStore::clear_agent_prompt_window(self, run_id)
            .await
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use cowboy_workflow_core::{
        AgentPromptWindow, AppendUserPromptOutcome, CompareAndSealPromptWindowOutcome,
    };
    use sqlx::Executor;

    use super::*;
    use crate::contract::tests::{record, run};

    #[test]
    fn extended_busy_code_is_retryable() {
        assert!(is_retryable_sqlite_code(Some("517")));
        assert!(is_retryable_sqlite_code(Some("5")));
        println!("EVIDENCE sqlite-code value=517 primary=5 retryable=true");
    }

    #[test]
    fn extended_locked_code_is_retryable() {
        assert!(is_retryable_sqlite_code(Some("262")));
        assert!(is_retryable_sqlite_code(Some("6")));
        println!("EVIDENCE sqlite-code value=262 primary=6 retryable=true");
    }

    #[tokio::test]
    async fn non_locking_sqlx_errors_are_not_retried() {
        assert!(!is_retryable_sqlite_code(Some("19")));
        assert!(!is_retryable_sqlite_code(Some("bad")));
        assert!(!is_retryable_sqlite_code(None));

        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWorkflowStore::connect(dir.path().join("data.db"))
            .await
            .unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempt_counter = attempts.clone();
        let result = store
            .retry_write(|| {
                attempt_counter.fetch_add(1, Ordering::SeqCst);
                async {
                    let mut tx = store.pool.begin().await?;
                    sqlx::query("INSERT INTO run_heads(run_id, data) VALUES(NULL, ?)")
                        .bind(b"invalid".as_slice())
                        .execute(&mut *tx)
                        .await?;
                    tx.commit().await?;
                    Ok(())
                }
            })
            .await;
        assert!(matches!(result, Err(Error::Sqlx(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        println!("EVIDENCE sqlite-code non_locking=true attempts=1");
    }

    fn prompt_window(run_id: &str, window_id: &str) -> AgentPromptWindow {
        AgentPromptWindow {
            window_id: window_id.into(),
            run_id: run_id.into(),
            step_record_id: "record-1".into(),
            step_id: "start".into(),
            role_id: "developer".into(),
            baseline_sequence: 0,
            applied_sequence: 0,
            opened_at: Utc::now(),
            sealed_at: None,
        }
    }

    #[tokio::test]
    async fn save_run_updates_run_and_head_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWorkflowStore::connect(dir.path().join("data.db"))
            .await
            .unwrap();
        let run = run("run-1");
        store.save_run(&run).await.unwrap();
        assert_eq!(store.load_run(&run.id).await.unwrap(), run);
        assert_eq!(
            store.load_run_head(&run.id).await.unwrap(),
            RunHead::from_run(&run)
        );
        println!("EVIDENCE transaction-run-head committed=true snapshots_match=true");
    }

    #[tokio::test]
    async fn completed_step_transaction_rolls_back_on_injected_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWorkflowStore::connect(dir.path().join("data.db"))
            .await
            .unwrap();
        let baseline = run("run-1");
        store.save_run(&baseline).await.unwrap();
        let mut changed = baseline.clone();
        changed.status = RunStatus::Completed;
        changed.updated_at = Utc::now();
        let record = record("record-1");
        let hash = object_hash(ObjectKind::StepRecord, &record).unwrap();
        store
            .fail_completed_step_before_commit
            .store(true, Ordering::SeqCst);

        assert!(matches!(
            store.commit_completed_step(&changed, &record).await,
            Err(Error::InjectedFailure)
        ));
        assert_eq!(store.load_run(&baseline.id).await.unwrap(), baseline);
        assert_eq!(
            store.load_run_head(&baseline.id).await.unwrap(),
            RunHead::from_run(&baseline)
        );
        assert!(matches!(
            store.load_step_record(&hash).await,
            Err(Error::ObjectNotFound(_))
        ));
        let mut reusable = baseline.clone();
        reusable.active_duration_ms = 100;
        store.save_run(&reusable).await.unwrap();
        assert_eq!(store.load_run(&reusable.id).await.unwrap(), reusable);
        println!("EVIDENCE transaction-step rollback=true pool_reusable=true");
    }

    #[tokio::test]
    async fn immutable_object_collision_is_rejected_without_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWorkflowStore::connect(dir.path().join("data.db"))
            .await
            .unwrap();
        let record = record("record-1");
        let hash = object_hash(ObjectKind::StepRecord, &record).unwrap();
        let conflicting = b"conflicting canonical bytes".to_vec();
        sqlx::query("INSERT INTO objects(hash, kind, data) VALUES(?, ?, ?)")
            .bind(&hash)
            .bind(serde_json::to_string(&ObjectKind::StepRecord).unwrap())
            .bind(&conflicting)
            .execute(store.pool())
            .await
            .unwrap();

        assert!(matches!(
            store.store_step_record(&record).await,
            Err(Error::HashCollision(collision)) if collision == hash
        ));
        let stored: Vec<u8> = sqlx::query_scalar("SELECT data FROM objects WHERE hash = ?")
            .bind(&hash)
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(stored, conflicting);
        println!("EVIDENCE object-collision rejected=true bytes_unchanged=true");
    }

    #[tokio::test]
    async fn prompt_append_and_compare_and_seal_are_totally_ordered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let store_a = SqliteWorkflowStore::connect(&path).await.unwrap();
        let store_b = SqliteWorkflowStore::connect(&path).await.unwrap();
        let run = run("run-1");
        store_a.save_run(&run).await.unwrap();
        store_a
            .open_agent_prompt_window(prompt_window(&run.id, "window-1"))
            .await
            .unwrap();
        let accepted = store_a
            .append_user_prompt(&run.id, "window-1", "  correction\n  ".into())
            .await
            .unwrap();
        let AppendUserPromptOutcome::Accepted(prompt) = accepted else {
            panic!("expected accepted prompt");
        };
        let pending = store_b
            .compare_and_seal_agent_prompt_window(&run.id, "window-1", 0, Utc::now())
            .await
            .unwrap();
        assert!(matches!(
            pending,
            CompareAndSealPromptWindowOutcome::Pending { prompts, .. }
                if prompts == vec![prompt.clone()]
        ));
        assert!(matches!(
            store_b
                .compare_and_seal_agent_prompt_window(&run.id, "window-1", 1, Utc::now())
                .await
                .unwrap(),
            CompareAndSealPromptWindowOutcome::Sealed(_)
        ));
        assert_eq!(
            store_a
                .append_user_prompt(&run.id, "window-1", "late".into())
                .await
                .unwrap(),
            AppendUserPromptOutcome::SealedWindow
        );

        for index in 0..8 {
            let baseline = store_a
                .load_user_prompts(&run.id)
                .await
                .unwrap()
                .last()
                .map(|prompt| prompt.sequence)
                .unwrap_or(0);
            let window_id = format!("race-window-{index}");
            store_a
                .open_agent_prompt_window(prompt_window(&run.id, &window_id))
                .await
                .unwrap();
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let append = {
                let barrier = barrier.clone();
                let store = store_a.clone();
                let run_id = run.id.clone();
                let window_id = window_id.clone();
                async move {
                    barrier.wait().await;
                    store
                        .append_user_prompt(&run_id, &window_id, format!("race-{index}"))
                        .await
                        .unwrap()
                }
            };
            let seal = {
                let barrier = barrier.clone();
                let store = store_b.clone();
                let run_id = run.id.clone();
                let window_id = window_id.clone();
                async move {
                    barrier.wait().await;
                    store
                        .compare_and_seal_agent_prompt_window(
                            &run_id,
                            &window_id,
                            baseline,
                            Utc::now(),
                        )
                        .await
                        .unwrap()
                }
            };
            let (append, seal) = tokio::join!(append, seal);
            match (append, seal) {
                (
                    AppendUserPromptOutcome::Accepted(prompt),
                    CompareAndSealPromptWindowOutcome::Pending { prompts, .. },
                ) => {
                    assert_eq!(prompts, vec![prompt.clone()]);
                    assert!(matches!(
                        store_b
                            .compare_and_seal_agent_prompt_window(
                                &run.id,
                                &window_id,
                                prompt.sequence,
                                Utc::now(),
                            )
                            .await
                            .unwrap(),
                        CompareAndSealPromptWindowOutcome::Sealed(_)
                    ));
                }
                (
                    AppendUserPromptOutcome::SealedWindow,
                    CompareAndSealPromptWindowOutcome::Sealed(_),
                ) => {}
                outcomes => panic!("append and seal were not totally ordered: {outcomes:?}"),
            }
        }
        println!("EVIDENCE prompt-order legal_outcomes=2 partial_state=false");
    }

    #[tokio::test]
    async fn store_wait_observer_fires_once_for_contended_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let base = SqliteWorkflowStore::connect(&path).await.unwrap();
        let mut holder = base.pool().acquire().await.unwrap();
        holder.execute("BEGIN IMMEDIATE").await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observer_calls = calls.clone();
        let store = base.with_wait_observer(Arc::new(move |_| {
            observer_calls.fetch_add(1, Ordering::SeqCst);
        }));
        let task = tokio::spawn({
            let store = store.clone();
            async move { store.save_run(&run("run-1")).await }
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        holder.execute("ROLLBACK").await.unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        println!("EVIDENCE store-wait observer_count=1 retries_at_least=1 path_leaked=false");
    }

    #[tokio::test]
    async fn store_wait_cancellation_interrupts_contended_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let base = SqliteWorkflowStore::connect(&path).await.unwrap();
        let mut holder = base.pool().acquire().await.unwrap();
        holder.execute("BEGIN IMMEDIATE").await.unwrap();
        let (sender, receiver) = watch::channel(0_u64);
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
        let observed_tx = Arc::new(std::sync::Mutex::new(Some(observed_tx)));
        let store = base
            .with_wait_observer(Arc::new(move |_| {
                if let Some(sender) = observed_tx.lock().unwrap().take() {
                    let _ = sender.send(());
                }
            }))
            .with_wait_cancellation(StoreWaitCancellation::new(receiver));
        let task = tokio::spawn({
            let store = store.clone();
            async move { store.save_run(&run("run-1")).await }
        });
        observed_rx.await.unwrap();
        sender.send_replace(1);
        let result = tokio::time::timeout(Duration::from_millis(200), task)
            .await
            .expect("cancelled store wait should finish promptly")
            .unwrap();
        assert!(matches!(result, Err(Error::WaitCancelled)));
        holder.execute("ROLLBACK").await.unwrap();
        base.save_run(&run("run-2")).await.unwrap();
        println!("EVIDENCE store-cancel cancelled=true pool_reusable=true");
    }

    #[tokio::test]
    async fn two_pools_share_wal_reads_writes_and_reuse_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let store_a = SqliteWorkflowStore::connect(&path).await.unwrap();
        let store_b = SqliteWorkflowStore::connect(&path).await.unwrap();
        store_a.save_run(&run("run-a")).await.unwrap();

        let (read_a, read_b) = tokio::join!(store_a.list_runs(), store_b.list_runs());
        assert_eq!(read_a.unwrap().len(), 1);
        assert_eq!(read_b.unwrap().len(), 1);

        let run_b = run("run-b");
        let run_c = run("run-c");
        let (write_a, write_b) = tokio::join!(store_a.save_run(&run_b), store_b.save_run(&run_c));
        write_a.unwrap();
        write_b.unwrap();
        assert_eq!(store_a.list_runs().await.unwrap().len(), 3);
        assert_eq!(store_b.list_runs().await.unwrap().len(), 3);

        let mut connection = store_a.pool().acquire().await.unwrap();
        connection.execute("BEGIN IMMEDIATE").await.unwrap();
        connection.execute("ROLLBACK").await.unwrap();
        drop(connection);
        store_a.save_run(&run("run-d")).await.unwrap();
        assert_eq!(store_b.load_run("run-d").await.unwrap().id, "run-d");
        println!(
            "EVIDENCE wal-concurrency pools=2 readers=concurrent writes=visible reusable=true"
        );
    }

    #[tokio::test]
    async fn prompt_windows_preserve_content_sequences_rejections_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWorkflowStore::connect(dir.path().join("data.db"))
            .await
            .unwrap();
        let mut active = run("run-1");
        store.save_run(&active).await.unwrap();
        store
            .open_agent_prompt_window(prompt_window(&active.id, "window-1"))
            .await
            .unwrap();
        let content = "  correction\nwith spacing  ".to_string();
        let accepted_not_before = Utc::now().timestamp_millis();
        let first = store
            .append_user_prompt(&active.id, "window-1", content.clone())
            .await
            .unwrap();
        let AppendUserPromptOutcome::Accepted(first) = first else {
            panic!("expected accepted prompt")
        };
        let second = store
            .append_user_prompt(&active.id, "window-1", "second".into())
            .await
            .unwrap();
        let AppendUserPromptOutcome::Accepted(second) = second else {
            panic!("expected second prompt")
        };
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(first.content, content);
        assert_eq!(first.submitted_at.timestamp_subsec_nanos() % 1_000_000, 0);
        assert!(first.submitted_at.timestamp_millis() >= accepted_not_before);
        assert_eq!(
            store
                .append_user_prompt(&active.id, "stale", "ignored".into())
                .await
                .unwrap(),
            AppendUserPromptOutcome::StaleWindow
        );
        assert!(matches!(
            store
                .abort_agent_prompt_window(&active.id, "window-1", Utc::now())
                .await
                .unwrap(),
            AbortAgentPromptWindowOutcome::Aborted(_)
        ));
        assert_eq!(
            store
                .append_user_prompt(&active.id, "window-1", "late".into())
                .await
                .unwrap(),
            AppendUserPromptOutcome::SealedWindow
        );
        assert!(
            store
                .clear_agent_prompt_window(&active.id)
                .await
                .unwrap()
                .is_some()
        );
        active.status = RunStatus::Completed;
        store.save_run(&active).await.unwrap();
        assert_eq!(
            store
                .open_agent_prompt_window(prompt_window(&active.id, "window-2"))
                .await
                .unwrap(),
            OpenAgentPromptWindowOutcome::TerminalRun
        );
        store.delete_run(&active.id).await.unwrap();
        assert!(
            store
                .load_user_prompts(&active.id)
                .await
                .unwrap()
                .is_empty()
        );
        println!("EVIDENCE prompt-window content=exact sequence=monotonic cleanup=true");
    }
}
