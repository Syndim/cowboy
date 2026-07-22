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
    #[error("agent reply did not contain a workflow result")]
    NoWorkflowResult,
    #[error("agent response has an opening `---` but is missing the closing `---` delimiter")]
    MissingClosingDelimiter,
    #[error("YAML frontmatter must be a mapping")]
    FrontmatterNotMapping,
    #[error("YAML frontmatter field {0:?} must be a string")]
    FrontmatterFieldNotString(String),
    #[error("YAML frontmatter is missing required status field")]
    MissingStatus,
    #[error("agent output status {status:?} is not allowed; expected one of: {allowed}")]
    DisallowedStatus { status: String, allowed: String },
    #[error("agent output is missing required frontmatter field(s): {0}")]
    MissingOutputFields(String),
    #[error("agent output field {field:?} must be {expected}, got {actual}")]
    InvalidOutputFieldType {
        field: String,
        expected: String,
        actual: String,
    },
    #[error("agent output field {field:?} uses unsupported descriptor {descriptor:?}")]
    UnsupportedOutputFieldDescriptor { field: String, descriptor: String },
    #[error("agent action missing output for step record")]
    MissingOutput,
}

impl Error {
    /// Whether the failure is worth retrying with the same intact session.
    ///
    /// Parse/frontmatter failures mean the agent's reply carried no parseable
    /// workflow result: either it finished its work but its final message was
    /// malformed, or the reply contained no workflow result at all (a
    /// stall/stream-close or no-result reply). A corrective nudge on the reused
    /// session usually recovers. Transient transport/ACP (`Client`) errors are
    /// also retryable. Missing client wiring and internal conversion errors are not.
    pub fn recoverable(&self) -> bool {
        match self {
            Error::Client(_)
            | Error::Yaml(_)
            | Error::MissingFrontmatter
            | Error::NoWorkflowResult
            | Error::MissingClosingDelimiter
            | Error::FrontmatterNotMapping
            | Error::FrontmatterFieldNotString(_)
            | Error::MissingStatus
            | Error::DisallowedStatus { .. }
            | Error::MissingOutputFields(_)
            | Error::InvalidOutputFieldType { .. }
            | Error::UnsupportedOutputFieldDescriptor { .. }
            | Error::MissingOutput => true,
            Error::MissingClient(_) | Error::Json(_) => false,
            Error::Workflow(err) => err.recoverable(),
        }
    }
}

impl From<Error> for cowboy_workflow_core::WorkflowError {
    fn from(value: Error) -> Self {
        if value.recoverable() {
            cowboy_workflow_core::WorkflowError::RecoverableAction(value.to_string())
        } else {
            cowboy_workflow_core::WorkflowError::InvalidAction(value.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_transient_errors_are_recoverable() {
        assert!(Error::MissingFrontmatter.recoverable());
        assert!(Error::NoWorkflowResult.recoverable());
        assert!(Error::MissingClosingDelimiter.recoverable());
        assert!(Error::FrontmatterNotMapping.recoverable());
        assert!(Error::FrontmatterFieldNotString("status".to_string()).recoverable());
        assert!(Error::MissingStatus.recoverable());
        assert!(
            Error::DisallowedStatus {
                status: "bad".to_string(),
                allowed: "ok".to_string()
            }
            .recoverable()
        );
        assert!(Error::MissingOutputFields("summary".to_string()).recoverable());
        assert!(
            Error::InvalidOutputFieldType {
                field: "files".to_string(),
                expected: "array".to_string(),
                actual: "string".to_string()
            }
            .recoverable()
        );
        assert!(
            Error::UnsupportedOutputFieldDescriptor {
                field: "files".to_string(),
                descriptor: "map".to_string()
            }
            .recoverable()
        );
        assert!(Error::MissingOutput.recoverable());
        assert!(Error::Client(anyhow::anyhow!("transport reset")).recoverable());
    }

    #[test]
    fn wiring_and_conversion_errors_are_not_recoverable() {
        assert!(!Error::MissingClient("developer".to_string()).recoverable());
    }

    #[test]
    fn conversion_preserves_recoverability() {
        let recoverable: cowboy_workflow_core::WorkflowError = Error::MissingFrontmatter.into();
        assert!(recoverable.recoverable());
        assert!(matches!(
            recoverable,
            cowboy_workflow_core::WorkflowError::RecoverableAction(_)
        ));

        let closing: cowboy_workflow_core::WorkflowError = Error::MissingClosingDelimiter.into();
        assert!(closing.recoverable());
        assert!(matches!(
            closing,
            cowboy_workflow_core::WorkflowError::RecoverableAction(_)
        ));

        let fatal: cowboy_workflow_core::WorkflowError =
            Error::MissingClient("developer".to_string()).into();
        assert!(!fatal.recoverable());
        assert!(matches!(
            fatal,
            cowboy_workflow_core::WorkflowError::InvalidAction(_)
        ));
    }
}
