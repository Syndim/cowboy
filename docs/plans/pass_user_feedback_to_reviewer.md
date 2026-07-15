# Plan

Preserve cumulative, stage-labeled user feedback across the example feature, bug-fix, and dev-loop workflows so the next plan, RCA, result-feedback, or implementation reviewer evaluates revised work against the user's direction as well as repository rules, TODO state, tests, and validation evidence. Keep `user_feedback` separate from the immediate planner/reviewer `feedback` field, preserve entry order across repeated confirmation and blocked-recovery loops, and carry `Goal`, `Validation`, `Work dir`, `Plan doc`, `RCA doc`, and `Repro test` values unchanged when present.

The reusable Lua workflow modules already express this handoff through structured output fields and persisted step records. Retain that workflow-level design and its persisted end-to-end coverage. The replan changes the engine test architecture: the current persisted scenarios inject scripted agents and request topics through `WorkflowRuntime` members and branches compiled only under `#[cfg(test)]`. That gives production and tests different `WorkflowRuntime` layouts and different control flow.

Introduce one normal internal dependency seam, used by every build. Add an object-safe async `RuntimeDependencies` interface in the engine crate for the two variable behaviors: constructing the `ClientFactory` used by `AgentExecutor`, and generating an optional request topic. `WorkflowRuntime` will always hold `Arc<dyn RuntimeDependencies>`; `WorkflowRuntime::new` will install a production ACP-backed implementation, while a non-conditional internal constructor accepts another implementation. The runtime's agent execution and topic paths will always delegate through this interface—no test-only fields, constructors, early returns, factory overloads, or enum dispatch.

Use `mockall = "0.15"` as an engine-crate dev dependency. Mockall 0.15 supports `#[automock]` with `#[async_trait]` and its Rust 1.77 MSRV is below this workspace's Rust 1.89 requirement. Apply `#[cfg_attr(test, mockall::automock)]` to the normal dependency interface so tests use generated `MockRuntimeDependencies` expectations without changing the production interface or `WorkflowRuntime` shape. Keep scripted response state only as a test fixture behind the mocked seam; do not add another runtime override abstraction.

# Changes

- Preserve the existing feedback behavior in `examples/workflows/utils/context.lua`, confirmation steps, agent steps, blocked/clarification recovery, commit handling, reviewer prompts, and shared roles. These remain the source of truth for appending labeled user entries, rendering complete history, preserving artifact paths, and distinguishing user feedback from reviewer-generated feedback.
- Add `mockall = "0.15"` to `[dev-dependencies]` in `crates/workflow/engine/Cargo.toml`; do not make it a production or workspace-wide dependency.
- Add a private engine module, such as `crates/workflow/engine/src/runtime_dependencies.rs`, containing:
  - the object-safe async `RuntimeDependencies` trait;
  - a production implementation that preserves the existing ACP resolver/client-factory construction, deterministic-selector topic behavior, best-effort topic failure logging, cwd, and configured model behavior;
  - a cloneable adapter over `Arc<dyn ClientFactory>` if required by `AgentExecutor`'s generic factory parameter;
  - test mock generation on the trait via Mockall, without `#[cfg(test)]` behavior in the production implementation.
- Update `crates/workflow/engine/src/lib.rs` to register the private dependency module.
- Refactor `crates/workflow/engine/src/runtime.rs` so `WorkflowRuntime` always stores the dependency object and all constructors initialize it through the same field. `WorkflowRuntime::new` must retain its public API and production behavior; the internal injection constructor must be ordinary compiled code, not a test-only method.
- Route both `generate_request_topic` and `AgentExecutor` factory creation through `RuntimeDependencies`. Preserve current semantics: deterministic production selection does not contact an agent for a topic, topic failures remain non-fatal, request topics are generated only for new runs, invalid agent configuration fails at the same workflow boundary, and existing runs reuse their persisted topic.
- Remove `request_topic_override`, `agent_factory_override`, `with_request_topic_for_tests`, `with_agent_factory_for_tests`, the paired `#[cfg(test)]`/`#[cfg(not(test))]` `agent_factory` implementations, and `TestClientFactory`. After the cutover, the test module itself may remain conditional, but `WorkflowRuntime` fields and runtime branches must not depend on `cfg(test)`.
- Migrate every runtime test that uses either override to generated dependency mocks. This includes the shared topic-workflow helper and all start, stepwise start, named-workflow start, resume, resolve, answer, and run-summary topic tests, plus both persisted user-feedback scenarios that currently use `ScriptedAgentFactory` through `with_agent_factory_for_tests`.

# Tests to be added/updated

- Add focused unit coverage for the dependency seam and production adapter: the runtime delegates factory creation and topic generation to the configured dependency object, the client-factory adapter forwards the requested role and result, and dependency errors preserve existing workflow error/fallback behavior.
- Convert all request-topic tests in `crates/workflow/engine/src/runtime.rs` to `MockRuntimeDependencies`. Keep their observable assertions unchanged: generated topics appear on new-run events and persisted summaries, stepwise and named starts behave consistently, resumed runs do not generate a second topic, and resolve/answer paths retain the original persisted topic.
- Convert `workflow_runtime_plan_reviewer_receives_persisted_user_feedback` to the generated dependency mock while preserving its full persisted run path. Assert the revised `review_plan` prompt and persisted review record contain the exact plan-confirmation feedback and `plan_doc`.
- Convert `workflow_runtime_preserves_result_feedback_through_commit_recovery` to the generated dependency mock while preserving its full persisted run and recovery path. Assert result feedback, `work_dir`, `plan_doc`, `rca_doc`, `repro_test`, commands/evidence, and reviewer prompts survive implementation, test, review, commit failure, blocked recovery, and the second review.
- Keep and run the Lua workflow tests for confirmation capture, cumulative ordering, exact field preservation, reviewer guidance, result-feedback routing, and clarification/blocked/commit detours. Fixtures may be adapted to the seam, but the workflow behavior and `changes_requested` versus `replan_requested` routing must not change.
- Do not add source-text tests for absence of `cfg(test)`. Prove the invariant through the single normal constructor/dependency path, behavior tests using generated mocks, a production-library `cargo check` that excludes test-only compilation, and review of the final `WorkflowRuntime` definition and execution paths.

# How to verify

- Run `cargo fmt --all -- --check`.
- Run the focused Mockall-backed engine tests for request-topic persistence and both persisted user-feedback scenarios.
- Run `cargo test -p cowboy-workflow-lua examples_workflows`.
- Run `cargo test -p cowboy-workflow-engine` and `cargo test -p cowboy-workflow-lua`.
- Run `cargo check -p cowboy-workflow-engine --lib` to compile the production runtime without test-only code or dev-dependency use.
- Run `cargo clippy -p cowboy-workflow-engine -p cowboy-workflow-lua --all-targets -- -D warnings`.
- Inspect the two persisted workflow scenarios' captured reviewer prompts: the plan reviewer must receive the exact plan feedback and path; the implementation reviewer must receive the exact result feedback, artifact paths, and test/validation evidence before and after blocked recovery.

# TODO

- [x] Add Mockall to the engine dev dependencies and define the normal async `RuntimeDependencies` seam.
- [x] Implement production ACP agent-factory and request-topic behavior behind the seam, including any shared factory adapter.
- [x] Wire every `WorkflowRuntime` construction and execution path through the same dependency field and control flow.
- [x] Remove all test-only runtime override fields, constructors, factory variants, enums, and behavior branches.
- [x] Migrate every request-topic override test to generated `MockRuntimeDependencies` expectations without weakening assertions.
- [x] Migrate both persisted user-feedback runtime scenarios to generated dependency mocks and preserve prompt inspection.
- [x] Preserve and update Lua/engine regression coverage for cumulative feedback, artifact paths, reviewer evidence, routing, and blocked recovery.
- [x] Run formatting, focused tests, full affected-crate tests, production `cargo check`, and Clippy; fix every failure and warning.
