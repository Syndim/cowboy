//! Agent action execution for workflow steps.

mod error;
mod executor;
mod frontmatter;
mod prompt;

pub use error::{Error, Result};
pub use executor::{
    AgentExecution, AgentExecutionConfig, AgentExecutor, AgentProgress, AgentProgressKind,
    ClientFactory, ProgressSink,
};
pub use frontmatter::{FrontmatterOutput, parse_frontmatter_output};
pub use prompt::build_agent_prompt;
