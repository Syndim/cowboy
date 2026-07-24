use cowboy_workflow_core::ObjectHash;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQLite workflow store error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("workflow store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "configured workflow store is not a SQLite database; preserve the old file and choose or clear a SQLite store path: {0}"
    )]
    NonSqliteFile(String),
    #[error("workflow store schema version {found} is newer than supported version {supported}")]
    FutureSchema { found: i64, supported: i64 },
    #[error("workflow store schema initialization timed out while waiting for another writer")]
    BootstrapTimeout,
    #[error("workflow store wait cancelled")]
    WaitCancelled,
    #[error("stored object envelope is missing payload")]
    MissingPayload,
    #[error("object {0} not found")]
    ObjectNotFound(ObjectHash),
    #[error("run {0:?} not found")]
    RunNotFound(String),
    #[error("immutable object hash collision for {0}")]
    HashCollision(ObjectHash),
    #[error("invalid prompt-window state: {0}")]
    InvalidPromptState(String),
    #[cfg(test)]
    #[error("injected transaction failure")]
    InjectedFailure,
}

impl From<Error> for cowboy_workflow_core::WorkflowError {
    fn from(value: Error) -> Self {
        cowboy_workflow_core::WorkflowError::InvalidAction(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use cowboy_workflow_core::WorkflowError;

    use super::*;

    #[test]
    fn non_temporary_store_error_maps_to_invalid_action_workflow_error() {
        let workflow_error: WorkflowError = Error::RunNotFound("run-1".to_string()).into();
        assert!(!workflow_error.recoverable());
        assert!(matches!(workflow_error, WorkflowError::InvalidAction(_)));
    }
}
