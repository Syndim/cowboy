//! Durable, private ownership ledger. A terminal run state is not a shutdown proof.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use cowboy_workflow_core::{ObjectKind, Run, RunStatus, StepRecord};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, Transaction};

use crate::{Error, Result, SqliteWorkflowStore};

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeWriterStart {
    pub generation: String,
    pub owner_token: String,
    pub run_id: String,
    pub workflow_hash: String,
    pub step_id: String,
    pub step_record_id: String,
    pub previous_head: Option<String>,
    pub visit: u32,
    pub attempt: u64,
    pub action_fingerprint: String,
    pub role_id: String,
    pub backend_identity: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeProcessExit {
    pub scope_id: String,
    /// Native group/Job empty after direct-child reap, or OS spawn failed before a child existed.
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeWriterExit {
    pub owner_token: String,
    pub backend_identity: String,
    pub session_id: Option<String>,
    pub scopes: Vec<NativeProcessExit>,
    pub verified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredStart {
    version: u32,
    start: NativeWriterStart,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredExit {
    version: u32,
    generation: String,
    session_at_seal: Option<String>,
    exit: NativeWriterExit,
}

/// Redacted read-only candidate; never authorizes resource release by itself.
#[derive(Debug, Clone, Serialize)]
pub struct NativeShutdownEvidence {
    pub version: u32,
    pub run_id: String,
    pub status: String,
    pub head: Option<String>,
    pub state: &'static str,
    pub attempt_count: usize,
    pub process_count: usize,
}
async fn committed_records(
    tx: &mut Transaction<'_, Sqlite>,
    run: &Run,
) -> Result<Option<BTreeMap<String, StepRecord>>> {
    let mut cursor = run.step.head.clone();
    let mut visited = BTreeSet::new();
    let mut records = BTreeMap::new();
    while let Some(hash) = cursor {
        if !visited.insert(hash.clone()) {
            return Ok(None);
        }

        let bytes: Option<Vec<u8>> = sqlx::query_scalar("SELECT data FROM objects WHERE hash = ?")
            .bind(&hash)
            .fetch_optional(&mut **tx)
            .await?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes)?;
        if envelope["kind"] != "step_record" {
            return Ok(None);
        }

        let payload = envelope
            .as_object_mut()
            .and_then(|fields| fields.remove("payload"))
            .ok_or(Error::MissingPayload)?;
        let record: StepRecord = serde_json::from_value(payload)?;
        if crate::object_hash(ObjectKind::StepRecord, &record)? != hash {
            return Ok(None);
        }

        cursor = record.prev.clone();
        if records.insert(record.id.clone(), record).is_some() {
            return Ok(None);
        }
    }

    Ok(Some(records))
}

impl SqliteWorkflowStore {
    /// Enroll *only* a newly created opted-in run, before its first agent dispatch.
    pub async fn enroll_native_run(&self, run: &Run) -> Result<()> {
        self.retry_write(|| async {
            let mut tx = self.pool().begin().await?;
            let data: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT data FROM runs WHERE run_id = ?")
                    .bind(&run.id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if data
                .is_none_or(|data| serde_json::from_slice::<Run>(&data).ok().as_ref() != Some(run))
            {
                return Err(Error::InvalidNativeWriter("run changed before enrollment"));
            }

            sqlx::query("INSERT INTO native_writer_runs(run_id, workflow_hash) VALUES(?, ?)")
                .bind(&run.id)
                .bind(&run.workflow.hash)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }
    pub async fn native_enrollment(&self, run_id: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT workflow_hash FROM native_writer_runs WHERE run_id = ?")
                .bind(run_id)
                .fetch_optional(self.pool())
                .await?,
        )
    }
    pub async fn native_pending_writers(&self, run_id: &str) -> Result<bool> {
        let pending: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM native_writer_attempts WHERE run_id = ? AND receipt IS NULL)",
        ).bind(run_id).fetch_one(self.pool()).await?;
        Ok(pending != 0)
    }

    pub async fn invalidate_native_run(&self, run_id: &str) -> Result<()> {
        self.retry_write(|| async {
            sqlx::query(
                "UPDATE native_writer_runs SET workflow_hash = 'invalidated' WHERE run_id = ?",
            )
            .bind(run_id)
            .execute(self.pool())
            .await?;
            Ok(())
        })
        .await
    }

    /// Register the generation before the factory can spawn an ACP writer.
    pub async fn begin_native_writer(&self, start: &NativeWriterStart) -> Result<()> {
        if start.generation.is_empty()
            || start.owner_token.is_empty()
            || start.backend_identity.is_empty()
            || start.action_fingerprint.is_empty()
            || start.role_id.is_empty()
            || start.visit == 0
            || start.attempt == 0
            || start.step_record_id.is_empty()
        {
            return Err(Error::InvalidNativeWriter("incomplete writer identity"));
        }

        self.retry_write(|| async {
            let mut tx = self.pool().begin().await?;
            let marker: Option<String> =
                sqlx::query_scalar("SELECT workflow_hash FROM native_writer_runs WHERE run_id = ?")
                    .bind(&start.run_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let data: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT data FROM runs WHERE run_id = ?")
                    .bind(&start.run_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let run: Run = serde_json::from_slice(
                &data.ok_or_else(|| Error::RunNotFound(start.run_id.clone()))?,
            )?;
            if marker.as_deref() != Some(&start.workflow_hash)
                || run.workflow.hash != start.workflow_hash
                || !matches!(run.status, RunStatus::Running)
                || run.agent_recovery_denied
                || run.step.next != start.step_id
                || run.step.head != start.previous_head
                || !matches!(
                    start
                        .visit
                        .checked_sub(run.step.visits.get(&start.step_id).copied().unwrap_or(0)),
                    Some(0 | 1)
                )
            {
                return Err(Error::InvalidNativeWriter(
                    "writer attempt does not match current run",
                ));
            }

            let data = serde_json::to_vec(&StoredStart {
                version: VERSION,
                start: start.clone(),
            })?;
            sqlx::query(
                "INSERT INTO native_writer_attempts(generation, run_id, data) VALUES(?, ?, ?)",
            )
            .bind(&start.generation)
            .bind(&start.run_id)
            .bind(data)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    /// Atomically seal every generation that belongs to one verified client instance.
    pub async fn seal_native_writer(&self, run_id: &str, exit: &NativeWriterExit) -> Result<()> {
        if exit.owner_token.is_empty()
            || exit.backend_identity.is_empty()
            || exit.scopes.len() != 1
            || exit.scopes[0].scope_id != exit.owner_token
        {
            return Err(Error::InvalidNativeWriter(
                "no verified owned process scope",
            ));
        }

        let mut scope_ids = BTreeSet::new();
        if exit.scopes.iter().any(|scope| {
            scope.scope_id.is_empty()
                || !matches!(
                    scope.method.as_str(),
                    "unix_group_absent_after_reap"
                        | "windows_job_empty_after_reap"
                        | "spawn_failed_no_child"
                )
                || (scope.method == "spawn_failed_no_child" && exit.session_id.is_some())
                || !scope_ids.insert(&scope.scope_id)
        }) {
            return Err(Error::InvalidNativeWriter("invalid owned process scope"));
        }

        self.retry_write(|| async {
            let mut tx = self.pool().begin().await?;
            let data: Vec<u8> = sqlx::query_scalar("SELECT data FROM runs WHERE run_id = ?")
                .bind(run_id).fetch_optional(&mut *tx).await?
                .ok_or_else(|| Error::RunNotFound(run_id.to_string()))?;
            let run: Run = serde_json::from_slice(&data)?;
            if run.agent_recovery_denied || run.agent_input_checkpoint.is_some() {
                return Err(Error::InvalidNativeWriter("unsafe run cannot attest writer shutdown"));
            }

            let rows = sqlx::query("SELECT generation, data, receipt FROM native_writer_attempts WHERE run_id = ?")
                .bind(run_id).fetch_all(&mut *tx).await?;
            let mut sealed = 0;
            for row in rows {
                let start: StoredStart = serde_json::from_slice(row.get("data"))?;
                if start.start.owner_token != exit.owner_token { continue; }
                if start.version != VERSION || start.start.run_id != run_id
                    || start.start.generation != row.get::<String, _>("generation")
                    || start.start.workflow_hash != run.workflow.hash
                    || start.start.backend_identity != exit.backend_identity
                    || exit.verified_at < start.start.started_at
                    || row.get::<Option<Vec<u8>>, _>("receipt").is_some()
                {
                    return Err(Error::InvalidNativeWriter("stale or foreign writer generation"));
                }
                let session_data: Option<Vec<u8>> = sqlx::query_scalar(
                    "SELECT data FROM role_sessions WHERE run_id = ? AND role_id = ?",
                ).bind(run_id).bind(&start.start.role_id).fetch_optional(&mut *tx).await?;
                let session = session_data.map(|data| serde_json::from_slice::<cowboy_workflow_core::RoleSession>(&data)).transpose()?;
                if exit.session_id.as_ref().is_some_and(|id| session.as_ref().map(|row| &row.session_id) != Some(id))
                    || session.as_ref().is_some_and(|row| row.backend_identity.as_deref() != Some(exit.backend_identity.as_str()))
                {
                    return Err(Error::InvalidNativeWriter("session does not belong to verified backend"));
                }


                let receipt = serde_json::to_vec(&StoredExit {
                    version: VERSION,
                    generation: start.start.generation.clone(),
                    session_at_seal: session.as_ref().map(|row| row.session_id.clone()),
                    exit: exit.clone(),
                })?;
                let changed = sqlx::query("UPDATE native_writer_attempts SET receipt = ? WHERE generation = ? AND receipt IS NULL")
                    .bind(receipt).bind(row.get::<String, _>("generation"))
                    .execute(&mut *tx).await?.rows_affected();
                if changed != 1 { return Err(Error::InvalidNativeWriter("writer generation changed")); }
                sealed += 1;
            }

            if sealed == 0 { return Err(Error::InvalidNativeWriter("writer owner has no pending generation")); }
            tx.commit().await?;
            Ok(())
        }).await
    }

    pub async fn native_shutdown_evidence(&self, run_id: &str) -> Result<NativeShutdownEvidence> {
        let mut tx = self.pool().begin().await?;
        let mut pending = vec![run_id.to_string()];
        let mut seen = BTreeSet::new();
        let mut scopes = BTreeMap::<String, (String, String)>::new();
        let mut expected_children = BTreeMap::<String, String>::new();
        let mut actual_parents = BTreeMap::<String, Option<String>>::new();
        let mut verified = true;
        let mut attempt_count = 0;
        let mut process_count = 0;
        let mut root = None;
        let mut requested_completion = false;
        while let Some(current_id) = pending.pop() {
            if !seen.insert(current_id.clone()) {
                continue;
            }
            let data: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT data FROM runs WHERE run_id = ?")
                    .bind(&current_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some(data) = data else {
                if current_id == run_id {
                    return Err(Error::RunNotFound(current_id));
                }

                verified = false;
                continue;
            };
            let run: Run = serde_json::from_slice(&data)?;
            actual_parents.insert(
                current_id.clone(),
                run.parent.as_ref().map(|parent| parent.run_id.clone()),
            );
            if current_id == run_id {
                root = Some((run.status.clone(), run.step.head.clone()));
                requested_completion = matches!(run.status, RunStatus::Completed);
            }
            let head_data: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT data FROM run_heads WHERE run_id = ?")
                    .bind(&current_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let head: Option<cowboy_workflow_core::RunHead> = head_data
                .map(|data| serde_json::from_slice(&data))
                .transpose()?;
            verified &= head
                .as_ref()
                .is_some_and(|head| head.head_step == run.step.head && head.status == run.status);
            if matches!(run.status, RunStatus::Completed) {
                verified &= run.step.head.is_some();
            }
            let incomplete_record_id = matches!(run.status, RunStatus::WaitingForInput { .. })
                .then(|| format!("{}-{}", run.id, run.step.executed + 1));

            let marker: Option<String> =
                sqlx::query_scalar("SELECT workflow_hash FROM native_writer_runs WHERE run_id = ?")
                    .bind(&current_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            verified &= marker.as_deref() == Some(run.workflow.hash.as_str())
                && !run.agent_recovery_denied
                && run.agent_input_checkpoint.is_none()
                && matches!(
                    run.status,
                    RunStatus::Completed | RunStatus::WaitingForInput { .. }
                );
            if requested_completion {
                verified &= matches!(run.status, RunStatus::Completed);
            }
            let committed = if marker.as_deref() == Some(run.workflow.hash.as_str()) {
                committed_records(&mut tx, &run).await?
            } else {
                None
            };
            verified &= committed.is_some();
            let committed = committed.unwrap_or_default();
            for record in committed
                .values()
                .filter(|record| record.action == "workflow")
            {
                if let Some(child_id) = record.input.context["child_run_id"].as_str() {
                    if let Some(other_parent) =
                        expected_children.insert(child_id.to_string(), current_id.clone())
                    {
                        verified &= other_parent == current_id;
                    }

                    pending.push(child_id.to_string());
                } else {
                    verified = false;
                }
            }

            if let RunStatus::WaitingForInput {
                resume_callback, ..
            } = &run.status
                && resume_callback.kind() == "workflow_child"
            {
                if let Some(child_id) = resume_callback.payload()["child_run_id"].as_str() {
                    expected_children.insert(child_id.to_string(), current_id.clone());
                    pending.push(child_id.to_string());
                } else {
                    verified = false;
                }
            }

            let session_rows =
                sqlx::query("SELECT role_id, data FROM role_sessions WHERE run_id = ?")
                    .bind(&current_id)
                    .fetch_all(&mut *tx)
                    .await?;
            let mut sessions = BTreeMap::new();
            for row in session_rows {
                let session: cowboy_workflow_core::RoleSession =
                    serde_json::from_slice(row.get("data"))?;
                sessions.insert(row.get::<String, _>("role_id"), session);
            }

            let rows = sqlx::query(
                "SELECT generation, data, receipt FROM native_writer_attempts WHERE run_id = ?",
            )
            .bind(&current_id)
            .fetch_all(&mut *tx)
            .await?;
            for row in rows {
                attempt_count += 1;
                let start: StoredStart = serde_json::from_slice(row.get("data"))?;
                let receipt: Option<Vec<u8>> = row.get("receipt");
                let Some(receipt) = receipt else {
                    verified = false;
                    continue;
                };
                let proof: StoredExit = serde_json::from_slice(&receipt)?;
                verified &= start.version == VERSION
                    && proof.version == VERSION
                    && start.start.generation == row.get::<String, _>("generation")
                    && proof.generation == start.start.generation
                    && start.start.run_id == current_id
                    && start.start.workflow_hash == run.workflow.hash
                    && start.start.owner_token == proof.exit.owner_token
                    && start.start.backend_identity == proof.exit.backend_identity
                    && proof.exit.verified_at >= start.start.started_at
                    && proof.exit.scopes.len() == 1
                    && proof.exit.scopes[0].scope_id == start.start.owner_token;
                let session = sessions.get(&start.start.role_id);
                verified &= proof.exit.session_id.is_none()
                    || proof.session_at_seal == proof.exit.session_id;
                if let Some(saved_id) = &proof.session_at_seal {
                    verified &= session.map(|row| &row.session_id) == Some(saved_id)
                        && session.is_some_and(|row| {
                            row.backend_identity.as_deref()
                                == Some(start.start.backend_identity.as_str())
                        });
                }
                if let Some(record) = committed.get(&start.start.step_record_id) {
                    verified &= record.action == "agent"
                        && record.step == start.start.step_id
                        && record.prev == start.start.previous_head
                        && record.input.context["role"] == start.start.role_id
                        && record.input.context["native_action_fingerprint"]
                            == start.start.action_fingerprint
                        && proof.exit.session_id.as_ref().is_none_or(|session| {
                            record.detail.session_id.as_ref() == Some(session)
                        });
                } else if let RunStatus::WaitingForInput {
                    step,
                    resume_callback,
                    ..
                } = &run.status
                    && resume_callback.kind() == "agent_human_input"
                    && start.start.step_id == *step
                    && incomplete_record_id.as_deref() == Some(start.start.step_record_id.as_str())
                {
                    let callback = resume_callback.payload();
                    let original = callback.get("action").map(serde_json::to_vec).transpose()?;
                    verified &= original.as_ref().is_some_and(|bytes| {
                        blake3::hash(bytes).to_hex().as_str() == start.start.action_fingerprint
                    }) && callback.get("role").and_then(serde_json::Value::as_str)
                        == Some(start.start.role_id.as_str())
                        && callback.get("visit").and_then(serde_json::Value::as_u64)
                            == Some(u64::from(start.start.visit))
                        && start.start.previous_head == run.step.head;
                } else {
                    verified = false;
                }

                for scope in &proof.exit.scopes {
                    verified &= scope.scope_id == start.start.owner_token
                        && matches!(
                            scope.method.as_str(),
                            "unix_group_absent_after_reap"
                                | "windows_job_empty_after_reap"
                                | "spawn_failed_no_child"
                        )
                        && (scope.method != "spawn_failed_no_child"
                            || proof.exit.session_id.is_none());
                    if let Some((other_owner, other_method)) = scopes.get(&scope.scope_id) {
                        verified &=
                            other_owner == &proof.exit.owner_token && other_method == &scope.method;
                    } else {
                        if scope.method != "spawn_failed_no_child" {
                            process_count += 1;
                        }

                        scopes.insert(
                            scope.scope_id.clone(),
                            (proof.exit.owner_token.clone(), scope.method.clone()),
                        );
                    }
                }
            }
            if let Some(parent) = &run.parent {
                if parent.run_id == current_id {
                    verified = false;
                } else {
                    pending.push(parent.run_id.clone());
                }
            }

            if let Some(source_id) = &run.restart_source_run_id {
                pending.push(source_id.clone());
            }

            let children = sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT data FROM runs WHERE json_extract(data, '$.parent.run_id') = ?",
            )
            .bind(&current_id)
            .fetch_all(&mut *tx)
            .await?;
            for child_data in children {
                let child: Run = serde_json::from_slice(&child_data)?;
                pending.push(child.id);
            }
        }

        for (child_id, parent_id) in expected_children {
            verified &= actual_parents.get(&child_id) == Some(&Some(parent_id));
        }

        tx.commit().await?;
        let (root_status, root_head) =
            root.ok_or_else(|| Error::RunNotFound(run_id.to_string()))?;
        let status = match root_status {
            RunStatus::Running => "running",
            RunStatus::WaitingForInput { .. } => "waiting_for_input",
            RunStatus::Completed => "completed",
            RunStatus::Failed { .. } => "failed",
            RunStatus::Cancelled => "cancelled",
        };
        Ok(NativeShutdownEvidence {
            version: VERSION,
            run_id: run_id.to_string(),
            status: status.to_string(),
            head: root_head,
            state: if verified { "verified" } else { "unknown" },
            attempt_count,
            process_count,
        })
    }
}
