use cowboy_workflow_core::WorkflowId;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("workflow id must not be empty")]
    EmptyWorkflowId,
    #[error("invalid relative workflow path {0:?}")]
    InvalidRelativePath(String),
    #[error("workflow entry must be a .lua file: {0:?}")]
    NonLuaEntry(String),
    #[error("workflow {workflow_id:?} does not have a filesystem root")]
    MissingRoot { workflow_id: WorkflowId },
    #[error("unknown built-in workflow {workflow_id:?}")]
    UnknownBuiltin { workflow_id: WorkflowId },
    #[error("unknown workflow {workflow_id:?}")]
    UnknownWorkflow { workflow_id: WorkflowId },
    #[error("workflow {workflow_id:?} is missing replacement source")]
    MissingReplacementSource { workflow_id: WorkflowId },
    #[error("workflow {workflow_id:?} already exists at {path}")]
    AlreadyExists {
        workflow_id: WorkflowId,
        path: String,
    },
    #[error("workflow {workflow_id:?} is invalid: {message}")]
    InvalidWorkflowSource {
        workflow_id: WorkflowId,
        message: String,
    },
    #[error("workflow catalog root is not a directory: {0}")]
    NotDirectory(String),
    #[error("workflow path is not valid UTF-8: {0}")]
    NonUtf8Path(String),
    #[error("I/O error at {path}: {message}")]
    Io { path: String, message: String },
}
