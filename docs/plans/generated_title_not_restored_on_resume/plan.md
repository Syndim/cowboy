# Plan

Use the confirmed RCA in `docs/plans/generated_title_not_restored_on_resume/rca.md` as the source of truth. The durable `WorkflowRun.request_topic` field already stores the generated title for new runs; the bug is that existing-run execution paths pass `None` into the runner, so resumed `RunStarted` events omit the title.

Fix the runtime by carrying the persisted topic from the loaded `WorkflowRun` into every existing-run continuation that emits a fresh `RunStarted` event. Do not generate a new topic for resume, step, prompt-answer continuation, or manual-resolution continuation. Preserve legacy runs whose `request_topic` is `None`.

Keep the investigator-added repro test unchanged as the primary regression input: `crates/workflow/engine/src/runtime.rs::runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event`.

# Changes

- Update `crates/workflow/engine/src/runtime.rs` in `WorkflowRuntime::resume_with` to clone `run.request_topic` from the loaded run before moving `run`, then pass that value to `run_existing` instead of `None`. This covers both `resume_run` and `step_run` because both route through `resume_with`.
- Update `WorkflowRuntime::answer_run` when an answer advances the run back to `Running`: pass `run.request_topic.clone()` in `ActiveRunExecution.request_topic` instead of `None` before calling `run_existing_with_events`.
- Update `WorkflowRuntime::resolve_run` when manual resolution advances the run back to `Running`: pass `run.request_topic.clone()` in `ActiveRunExecution.request_topic` instead of `None` before calling `run_existing_with_events`.
- Do not change `WorkflowRuntime::start_catalog_workflow` title generation semantics: it should generate once for a new run, persist `WorkflowRun.request_topic`, and pass the freshly generated topic to the first runner invocation.
- Do not change storage schema or TUI rendering unless implementation evidence shows an additional break. The RCA confirms persistence and list summaries already use `WorkflowRun.request_topic` with a legacy event fallback.

# Tests to be added/updated

- Preserve the investigator-added failing regression test exactly: `resume_run_restores_persisted_request_topic_in_run_started_event` in `crates/workflow/engine/src/runtime.rs`. The implementation must make this test pass by changing production code, not by weakening the assertion.
- Add focused runtime coverage for `step_run` proving that a persisted `request_topic` reaches the resumed `RunStarted` event through the `RunMode::SingleStep` path.
- Add focused runtime coverage for `answer_run` continuation proving that a persisted `request_topic` reaches the continued `RunStarted` event after an ask-user answer routes to another step.
- Add focused runtime coverage for `resolve_run` continuation proving that a persisted `request_topic` reaches the continued `RunStarted` event after manual resolution routes to another step.

# How to verify

1. Run the investigator-provided regression command:

   ```bash
   cargo test -p cowboy-workflow-engine runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event -- --exact
   ```

   Expected result: the test passes, and the resumed report's first `RunStarted` event contains `request_topic: Some("Initial topic")`.

2. Run the narrow engine runtime tests that cover generated-topic behavior and any newly added continuation coverage:

   ```bash
   cargo test -p cowboy-workflow-engine runtime::tests:: -- request_topic
   ```

   Expected result: all selected request-topic tests pass; no test shows a regenerated title on resume or a lost persisted title on continuation.

3. Run the crate-level engine tests after the focused tests pass:

   ```bash
   cargo test -p cowboy-workflow-engine
   ```

   Expected result: all engine tests pass with no Rust compiler warnings introduced by the change.

# TODO

- [x] TODO-01: Propagate the persisted topic through `resume_with`.
  - Procedure: Edit `crates/workflow/engine/src/runtime.rs` so `WorkflowRuntime::resume_with` captures `run.request_topic.clone()` after loading or status-normalizing the run and passes it to `run_existing` instead of `None`.
  - Expected result: `resume_run` and `step_run` invoke `WorkflowRunner::with_request_topic` with the loaded durable topic for existing runs, and still pass `None` for legacy runs whose durable topic is absent.
  - Observed result: `WorkflowRuntime::resume_with` now captures `let request_topic = run.request_topic.clone();` before moving `run` into `run_existing`, and the focused request-topic tests passed with `resume_run_restores_persisted_request_topic_in_run_started_event` and `step_run_restores_persisted_request_topic_in_run_started_event` both observing persisted topics.

- [x] TODO-02: Propagate the persisted topic through prompt-answer continuation.
  - Procedure: Edit `WorkflowRuntime::answer_run` so the `status == RunStatus::Running` continuation passes `run.request_topic.clone()` in `ActiveRunExecution.request_topic` before calling `run_existing_with_events`.
  - Expected result: answering an ask-user prompt that routes to another step emits the prefix answer events and then a continued `RunStarted` event whose `request_topic` matches the stored `WorkflowRun.request_topic`.
  - Observed result: `WorkflowRuntime::answer_run` now captures the loaded run's `request_topic` before the continued `run_existing_with_events` call, and `answer_run_restores_persisted_request_topic_before_resumed_events` passed while asserting `Some("Initial prompt topic")` in the continued `RunStarted` event.

- [x] TODO-03: Propagate the persisted topic through manual-resolution continuation.
  - Procedure: Edit `WorkflowRuntime::resolve_run` so the `status_result == RunStatus::Running` continuation passes `run.request_topic.clone()` in `ActiveRunExecution.request_topic` before calling `run_existing_with_events`.
  - Expected result: resolving a failed step to a status that routes to another step emits the manual-resolution events and then a continued `RunStarted` event whose `request_topic` matches the stored `WorkflowRun.request_topic`.
  - Observed result: `WorkflowRuntime::resolve_run` now captures the loaded run's `request_topic` before the continued `run_existing_with_events` call, and `resolve_run_restores_persisted_request_topic_and_exposes_fields_to_next_step` passed while asserting `Some("Original topic")` in the continued `RunStarted` event.

- [x] TODO-04: Keep the existing repro test unchanged and make it pass.
  - Procedure: Run `cargo test -p cowboy-workflow-engine runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event -- --exact` after the production change.
  - Expected result: the command exits successfully; the test observes `Some("Initial topic")` in the resumed `RunStarted` event.
  - Observed result: `cargo test -p cowboy-workflow-engine runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event -- --exact` exited 0 with 1 test passed; the unchanged repro test passed.

- [x] TODO-05: Add focused coverage for all existing-run topic paths.
  - Procedure: Add or update focused tests in `crates/workflow/engine/src/runtime.rs` for `step_run`, `answer_run`, and `resolve_run` continuations, then run `cargo test -p cowboy-workflow-engine runtime::tests:: -- request_topic`.
  - Expected result: each existing-run path that emits a fresh `RunStarted` event has an assertion proving it reuses `WorkflowRun.request_topic`, and the selected request-topic tests pass.
  - Observed result: Added `step_run_restores_persisted_request_topic_in_run_started_event`, updated the prompt-answer continuation coverage to assert `Some("Initial prompt topic")`, and updated the manual-resolution continuation coverage to assert `Some("Original topic")`; `cargo test -p cowboy-workflow-engine runtime::tests:: -- request_topic` exited 0 with 72 tests passed.

- [x] TODO-06: Run the engine crate test suite.
  - Procedure: Run `cargo test -p cowboy-workflow-engine`.
  - Expected result: the engine crate test suite passes, and the output contains no new Rust compiler warnings from the changed code.
  - Observed result: `cargo test -p cowboy-workflow-engine` exited 0 with 124 tests passed across lib/bin/doc-test suites and no compiler warnings in the output.
