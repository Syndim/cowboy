//! Workflow runtime orchestration adapters for Cowboy.
//!
//! This crate sits between the UI shell and the lower-level workflow crates. It
//! owns run orchestration, event projection, cwd session pointers, input routing,
//! and selector/summarizer adapters. The TUI crate should depend on these
//! interfaces rather than carrying workflow runtime logic itself.

mod agent_resolver;
pub mod events;
pub mod input;
pub mod runner;
pub mod runtime;
pub mod workflow;

pub use cowboy_workflow_actions::{
    AgentActionHandler, AgentActionRunner, AskUserActionRunner, EngineActionDispatcher,
    FailActionRunner, PendingAskUser, StatusActionRunner, SuspendActionRunner,
};
pub use events::{EventBus, WorkflowEvent, WorkflowEventKind};
pub use input::InputRouter;
pub use runner::{LuaStepActionProvider, WorkflowRunner};
pub use runtime::{
    AgentRuntimeConfig, RunReport, RunSummaryLine, RunnerLimitsConfig, RuntimeConfig,
    WorkflowRuntime,
};
pub use workflow::{AgentWorkflowSelector, AgentWorkflowSummarizer, DeterministicSelector};
