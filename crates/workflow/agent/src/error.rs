pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("agent client error: {0}")]
    Client(#[from] anyhow::Error),
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("json conversion error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("workflow error: {0}")]
    Workflow(#[from] cowboy_workflow_core::WorkflowError),
    #[error("missing client for role {0:?}")]
    MissingClient(String),
    #[error("agent response is missing YAML frontmatter")]
    MissingFrontmatter,
    #[error("YAML frontmatter must be a mapping")]
    FrontmatterNotMapping,
    #[error("YAML frontmatter field {0:?} must be a string")]
    FrontmatterFieldNotString(String),
    #[error("YAML frontmatter is missing required status field")]
    MissingStatus,
    #[error("agent action missing output for step record")]
    MissingOutput,
}

impl From<Error> for cowboy_workflow_core::WorkflowError {
    fn from(value: Error) -> Self {
        cowboy_workflow_core::WorkflowError::InvalidAction(value.to_string())
    }
}
