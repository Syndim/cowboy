//! Workflow runtime orchestration adapters for Cowboy.
//!
//! This crate sits between the UI shell and the lower-level workflow crates. It
//! owns run orchestration, event projection, cwd session pointers, input routing,
//! and selector/summarizer adapters. The TUI crate should depend on these
//! interfaces rather than carrying workflow runtime logic itself.

mod agent_resolver;
pub mod events;
pub mod input;
mod run_lock;
pub mod runner;
pub mod runtime;
pub mod workflow;

pub use cowboy_workflow_actions::{
    AgentActionHandler, AgentActionRunner, AskUserActionRunner, EngineActionDispatcher,
    FailActionRunner, PendingAskUser, ResumeCallbackRegistry, StatusActionRunner,
};
pub use events::{EventBus, WorkflowEvent, WorkflowEventKind};
pub use input::ResumeRouter;
pub use runner::{LuaStepActionProvider, WorkflowRunner};
pub use runtime::{
    AgentRuntimeConfig, ResolutionOptions, ResolutionStatus, RunReport, RunSummaryLine,
    RunnerLimitsConfig, RuntimeConfig, WorkflowRuntime,
};
pub use workflow::{
    AgentRequestTopicGenerator, AgentWorkflowSelector, AgentWorkflowSummarizer, DeterministicSelector,
};
