# Bug behavior

Generated run titles are persisted on newly started runs, but the active title is lost when a persisted run is resumed. A run started with a generated topic has `WorkflowRun.request_topic = Some("Initial topic")` in the redb-backed run record. The subsequent `resume_run` report emits a new `RunStarted` event with `request_topic: None`, so TUI state that restores the header from live workflow events has no title to render after resume.

# Root cause

The durable field exists: `WorkflowRun.request_topic` is serialized in `crates/workflow/core/src/state.rs`, and `WorkflowRuntime::start_catalog_workflow` stores the generated topic on the run before execution continues.

The resume path drops the durable value. `WorkflowRuntime::resume_with` loads the run from the store, compiles the saved workflow snapshot, then calls `run_existing(..., None, active_clock)`. `run_existing_with_events` forwards that `None` into `WorkflowRunner::with_request_topic`, and the runner emits the resumed `RunStarted` event without a topic. The same helper already attaches the generated topic on initial start, but resume does not pass `run.request_topic.clone()`.

This means the title is in the database, but resume does not restore it into the event stream that the TUI uses for the active header.

# Reproduction steps

1. Start a stepwise run using a runtime whose topic generator returns `Initial topic`.
2. Confirm the returned and persisted `WorkflowRun.request_topic` both equal `Some("Initial topic")`.
3. Resume the same run.
4. Inspect the resumed `RunStarted` event in the returned `RunReport`.
5. Observe that the resumed event carries `request_topic: None` instead of the persisted topic.

# Regression test

- Test file path: `crates/workflow/engine/src/runtime.rs`
- Test name: `runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event`
- Command: `cargo test -p cowboy-workflow-engine runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event -- --exact`
- Expected failure before the fix: the test fails because `first_run_started_topic(&report)` is `None` after `resume_run`, even though the persisted run has `request_topic = Some("Initial topic")`.

# Current failing result

```text
running 1 test
failures:

---- runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event stdout ----

thread 'runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event' panicked at crates/workflow/engine/src/runtime.rs:3250:9:
assertion `left == right` failed
  left: None
 right: Some("Initial topic")
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    runtime::tests::resume_run_restores_persisted_request_topic_in_run_started_event

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 122 filtered out; finished in 0.09s

error: test failed, to rerun pass `-p cowboy-workflow-engine --lib`
```

# Fix constraints

- Do not generate a new title when resuming an existing run; reuse the durable `WorkflowRun.request_topic` value.
- Preserve legacy runs with `request_topic: None`; resumed events for those runs should still omit the topic.
- Keep list summaries backed by `WorkflowRun.request_topic` and the existing legacy event fallback.
- Update resume-like continuation paths consistently if they emit a fresh `RunStarted` event for an existing run.
- Preserve `user_feedback` exactly in output fields when present; it is cumulative raw user direction and must not include agent- or reviewer-generated feedback.
