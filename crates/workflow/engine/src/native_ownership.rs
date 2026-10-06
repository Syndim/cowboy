use async_trait::async_trait;
use chrono::Utc;
use cowboy_agent_client::VerifiedClientShutdown;
use cowboy_workflow_agent::NativeWriterTracker;
use cowboy_workflow_core::{AgentAction, ExecutionContext, Result, WorkflowError};
use cowboy_workflow_store::{
    NativeProcessExit, NativeWriterExit, NativeWriterStart, SqliteWorkflowStore,
};

pub(crate) struct SqliteNativeWriterTracker(pub(crate) SqliteWorkflowStore);

#[async_trait]
impl NativeWriterTracker for SqliteNativeWriterTracker {
    async fn begin(
        &self,
        context: &ExecutionContext,
        action: &AgentAction,
        owner_token: &str,
        backend_identity: &str,
    ) -> Result<String> {
        let role = context.role.as_ref().ok_or(WorkflowError::AgentUnsafe)?;
        if role.id != action.role || owner_token.is_empty() || backend_identity.is_empty() {
            return Err(WorkflowError::AgentUnsafe);
        }

        let run = self.0.load_run(&context.run_id).await?;
        let action_value = serde_json::to_value(action).map_err(|_| WorkflowError::AgentUnsafe)?;
        let action_bytes =
            serde_json::to_vec(&action_value).map_err(|_| WorkflowError::AgentUnsafe)?;
        let start = NativeWriterStart {
            generation: uuid::Uuid::new_v4().to_string(),
            owner_token: owner_token.to_string(),
            run_id: context.run_id.clone(),
            workflow_hash: run.workflow.hash,
            step_id: context.step_id.clone(),
            step_record_id: context.step_record_id.clone(),
            previous_head: context.prev.clone(),
            visit: context.step_visit,
            attempt: context.attempt,
            action_fingerprint: blake3::hash(&action_bytes).to_hex().to_string(),
            role_id: role.id.clone(),
            backend_identity: backend_identity.to_string(),
            started_at: Utc::now(),
        };
        self.0.begin_native_writer(&start).await?;
        Ok(start.action_fingerprint)
    }

    async fn seal(
        &self,
        run_id: &str,
        backend_identity: &str,
        proof: VerifiedClientShutdown,
    ) -> Result<()> {
        let exit = NativeWriterExit {
            owner_token: proof.owner_token,
            backend_identity: backend_identity.to_string(),
            session_id: proof.session_id,
            scopes: proof
                .scopes
                .into_iter()
                .map(|scope| NativeProcessExit {
                    scope_id: scope.scope_id,
                    method: scope.method.to_string(),
                })
                .collect(),
            verified_at: Utc::now(),
        };
        self.0
            .seal_native_writer(run_id, &exit)
            .await
            .map_err(Into::into)
    }
}
