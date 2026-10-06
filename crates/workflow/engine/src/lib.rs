//! Workflow runtime orchestration adapters for Cowboy.
//!
//! This crate sits between the UI shell and the lower-level workflow crates. It
//! owns run orchestration, event projection, cwd session pointers, input routing,
//! and selector/summarizer adapters. The TUI crate should depend on these
//! interfaces rather than carrying workflow runtime logic itself.

mod active_clock;
mod agent_input;
mod agent_resolver;
pub mod events;
pub mod input;
mod native_ownership;
mod preflight;
mod run_lock;
pub mod runner;
pub mod runtime;
mod runtime_dependencies;
mod system;
pub mod workflow;

pub use cowboy_agent_acp::AgentAccess;
pub use cowboy_workflow_actions::{
    AgentActionHandler, AgentActionRunner, CommandActionRunner, EngineActionDispatcher,
    FailActionRunner, PendingWaitForInput, ResumeCallbackRegistry, StatusActionRunner,
    WaitForInputActionRunner, WorkflowActionHandler,
};
pub use cowboy_workflow_core::{Choice, RunStatus};
pub use cowboy_workflow_store::NativeShutdownEvidence;
pub use events::{EventBus, WorkflowEvent, WorkflowEventKind};
pub use input::ResumeRouter;
pub use runner::{LuaStepActionProvider, ResolvedRuntimePolicy, WorkflowRunner};
pub use runtime::{
    AgentRuntimeConfig, AgentWatchdogRuntimeConfig, DEFAULT_SHUTDOWN_TIMEOUT, ResolutionOptions,
    ResolutionStatus, RunReport, RunStartOptions, RunStatusDetail, RunStatusState, RunSummaryLine,
    RunnerLimitsConfig, RuntimeConfig, UserPromptRejection, UserPromptSubmission, WorkflowRuntime,
};
pub use workflow::{
    AgentRequestTopicGenerator, AgentWorkflowSelector, AgentWorkflowSummarizer,
    DeterministicSelector,
};
