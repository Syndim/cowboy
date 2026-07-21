use cowboy_workflow_core::ObjectHash;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors produced by the redb-backed workflow store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Opening or creating the redb database failed.
    #[error("redb database error: {0}")]
    Database(#[from] redb::DatabaseError),
    /// Starting a redb transaction failed.
    #[error("redb transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    /// Opening or accessing a redb table failed.
    #[error("redb table error: {0}")]
    Table(#[from] redb::TableError),
    /// Reading or writing redb table data failed.
    #[error("redb storage error: {0}")]
    Storage(#[from] redb::StorageError),
    /// Committing a redb write transaction failed.
    #[error("redb commit error: {0}")]
    Commit(#[from] redb::CommitError),
    /// Filesystem access for the workflow store failed.
    #[error("workflow store I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization/deserialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Waiting for another process to release the database was interrupted.
    #[error("workflow store wait cancelled")]
    WaitCancelled,
    /// Stored object envelope did not include a `payload` field.
    #[error("stored object envelope is missing payload")]
    MissingPayload,
    /// Immutable object was not present for the requested hash.
    #[error("object {0} not found")]
    ObjectNotFound(ObjectHash),
    /// Mutable run snapshot/head was not present for the requested run id.
    #[error("run {0:?} not found")]
    RunNotFound(String),
    /// Prompt-window metadata violated a sequencing invariant.
    #[error("invalid prompt-window state: {0}")]
    InvalidPromptState(String),
}

impl From<Error> for cowboy_workflow_core::WorkflowError {
    fn from(value: Error) -> Self {
        let message = value.to_string();

        cowboy_workflow_core::WorkflowError::InvalidAction(message)
    }
}

#[cfg(test)]
mod tests {
    use cowboy_workflow_core::WorkflowError;

    use super::*;

    #[test]
    fn non_temporary_store_error_maps_to_invalid_action_workflow_error() {
        let workflow_error: WorkflowError = Error::RunNotFound("run-1".to_string()).into();

        assert!(
            !workflow_error.recoverable(),
            "non-temporary workflow-store errors must remain terminal, got {workflow_error}"
        );

        assert!(
            matches!(workflow_error, WorkflowError::InvalidAction(_)),
            "non-temporary workflow-store errors must remain invalid actions"
        );
    }
}
