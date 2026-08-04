# Plan

Reduce repeated agent input by separating each agent action into a durable task contract and a per-dispatch turn payload. The task contract will contain a stable task key, static task instructions, recovery context, and the output contract. Cowboy will send the role instructions and task contract only when the active backend session has not received them. A reused session revisiting the same task will receive only new user-input entries and the current turn payload.

Use task keys scoped to a run and role rather than step ids. Reuse requires both the same key and the same fingerprinted durable contract: static task instructions and output specification must be identical. Steps that return work to the same agent, such as `implement` and `revise`, will consume one shared implementation-contract definition; initial/revision-specific wording belongs only in `task.turn` and recovery data. Distinct responsibilities performed by the same role, such as plan review and implementation review, will use different task keys.

Preserve safe recovery behavior. Persist delivered task-contract fingerprints in `RoleSession`, reset them when a backend session must be recreated, and resend the role, current task contract, recovery context, and current turn payload to a fresh session. A successfully loaded session keeps its delivery state. Existing snapshotted workflows that only provide `action.agent.prompt` will continue using the current full-prompt behavior; the optimized structured contract will be used by updated workflows and documented for new workflows.

Standardize revision handoffs as structured previous-step output. Reviewer and tester paths that return work to a prior agent will emit a routing status plus `changes_needed` and `change_context`. The receiving workflow step will format the incremental turn as:

```text
Changes needed:
- <change item>

Context:
<necessary reason, constraints, and artifact references>
```

Do not include the full plan, full specification, prior response body, static role instructions, static task instructions, or deliverable schema in this reused-session revision turn. Put the complete durable state needed after session recreation in the action's recovery context instead.

Keep `StepOutput` as the persisted source of truth and expose it canonically to Lua as `ctx.prev.output = { status, fields, body, raw }`. Retain the existing flattened `ctx.prev.status`, `fields`, `body`, and `raw` aliases for snapshotted-workflow compatibility. Updated prompt helpers will read the structured output, select only declared fields, and avoid serializing `raw` or unrelated evidence into normal revision turns.

# Changes

- Extend `AgentAction` in `crates/workflow/core/src/action.rs` with an optional structured task contract containing:
  - a non-empty stable task key;
  - static task instructions;
  - durable recovery context rendered from the current structured workflow state;
  - the existing output specification as the response contract.
- Extend Lua conversion in `crates/workflow/lua/src/convert.rs` so `action.agent` accepts the structured task contract while retaining the legacy prompt-only form for existing snapshotted workflows.
- Extend `RoleSession` in `crates/workflow/core/src/state.rs` with a defaulted map of delivered task keys to canonical contract fingerprints. Because role sessions are stored as serialized data, keep the field backward-compatible without changing the SQLite table layout.
- Refactor `crates/workflow/agent/src/prompt.rs` into explicit prompt blocks:
  - role block, sent once per backend session;
  - task and deliverable blocks, sent once per task-contract fingerprint;
  - recovery-context block, sent with a task contract or after fresh-session fallback;
  - turn block, sent for the current dispatch;
  - user-input delta, sent according to the existing sequence watermark;
  - retry correction, sent without replaying the task or turn when the same reused session already has them.
- Update `crates/workflow/agent/src/executor.rs` to compute the task-contract fingerprint, choose the required blocks from session freshness and delivery state, and persist role/task/input watermarks after successful delivery. Record the rendered prompt plus included block metadata in `StepInput.context` for debugging and transcript verification.
- Update `crates/workflow/engine/src/runner.rs::previous_step_context` to add the canonical nested `output` object while preserving current aliases.
- Extend `examples/workflows/utils/context.lua` with typed support for `changes_needed` and `change_context`, a recovery-context renderer, and a revision-turn builder that emits only the required changes and necessary context.
- Update reviewer, tester, validator, result-feedback, and blocker-review outputs that can route work backward to produce non-empty `changes_needed` and `change_context` for change/replan/failure statuses. Keep existing human-readable `feedback` where needed for display and compatibility.
- Add one shared implementation task-contract module under `examples/workflows/` and make both `steps/implement.lua` and `steps/revise.lua` use its exact key, static instructions, and output specification. Put the initial implementation objective and reviewer/test/validation change delta only in `task.turn`; do not derive fingerprinted instructions from each step's different objective or instruction text.
- Migrate the remaining feature, bug-fix, and dev-loop planning, investigation, testing, validation, and review steps to stable task keys. Share a contract only where key, static instructions, and output specification are identical; otherwise use distinct keys.
- Replace the synthetic block-composition capture in `crates/workflow/engine/tests/example_prompt_composition.rs` with persisted runtime tests in `crates/workflow/engine/src/runtime.rs`. The tests will compile a temporary workflow that imports the repository's real `steps/implement.lua` and `steps/revise.lua`, execute it through `WorkflowRuntime`, SQLite persistence, `EngineActionDispatcher`, and `AgentExecutor`, and capture prompts at the scripted client boundary.
- Update `docs/workflow-authoring.md`, `docs/architecture.md`, and `docs/module-map.md` to document task-key scope, prompt block delivery, fresh-session recovery, structured revision fields, and the canonical `ctx.prev.output` shape.

# Tests to be added/updated

- Add core serialization tests proving old `RoleSession` data defaults to no delivered task contracts and new task-delivery state round-trips.
- Add Lua conversion tests for valid structured task contracts, blank task keys, legacy prompt-only actions, and recovery-context preservation.
- Expand prompt unit tests to cover the exact block matrix for fresh sessions, reused sessions with a new task, reused sessions revisiting the same task, changed task-contract fingerprints, retries, and new user-input deltas.
- Expand executor tests to prove task-contract delivery state persists across executor recreation and loaded sessions, resets after load failure creates a fresh session, and advances after a delivered response even when output parsing requires a retry.
- Add runner tests proving `ctx.prev.output.status`, `fields`, `body`, and `raw` match the persisted `StepOutput` while legacy aliases remain available.
- Add example-workflow tests proving `changes_requested`, `replan_requested`, failed-test, validation-failure, and blocker-recovery outputs carry structured change fields. Compile actual `implement` and `revise` actions and assert their task keys, static instructions, and output specifications are equal while their turns differ.
- Add persisted runtime scenarios using the compiled implementation/revision loop and the real `AgentExecutor` path. Cover same-runtime revision routing, retry, runtime restart with successful session loading, and runtime restart with session-load failure/new-session recreation. Assert captured client prompts and persisted `StepRecord.input.context` fingerprints/block lists/session ids.
- Keep coverage for new on-the-fly user inputs so a reused-session revision prompt can contain both the minimal change delta and only input sequences not previously delivered.

# How to verify

1. Run the exact formatting and focused model/conversion checks:

   ```bash
   cargo fmt --all -- --check
   cargo test -p cowboy-workflow-core state::tests::role_session_defaults_delivery_state -- --exact
   cargo test -p cowboy-workflow-core action::tests::structured_agent_task_contract_round_trips -- --exact
   cargo test -p cowboy-workflow-lua convert::tests::converts_structured_agent_task_contract -- --exact
   cargo test -p cowboy-workflow-lua convert::tests::legacy_agent_prompt_remains_supported -- --exact
   cargo test -p cowboy-workflow-engine runner::tests::lua_provider_exposes_previous_step_output -- --exact
   ```

   Expected result: every command exits with status `0`; the core and Lua tests prove backward-compatible serialization/conversion, and the runner test proves the nested and flattened previous-output views are equal.

2. Run the exact prompt/session delivery matrix:

   ```bash
   cargo test -p cowboy-workflow-agent prompt::tests::structured_prompt_block_matrix -- --exact
   cargo test -p cowboy-workflow-agent executor::tests::structured_task_contract_delivery_matrix -- --exact
   cargo test -p cowboy-workflow-agent executor::tests::structured_task_contract_survives_loaded_session -- --exact
   cargo test -p cowboy-workflow-agent executor::tests::fresh_session_fallback_resends_recovery_context -- --exact
   cargo test -p cowboy-workflow-agent executor::tests::retry_omits_already_delivered_task_and_turn -- --exact
   ```

   Expected result: every command exits with status `0`; fresh/new-task cases contain the required static blocks, reused-task and retry cases omit them, and loaded/fallback cases preserve the documented recovery behavior.

3. Run the exact compiled-workflow contract and routing checks:

   ```bash
   cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_implementation_steps_share_identical_contract -- --exact
   cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_emit_structured_revision_handoffs -- --exact
   cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_matrix_uses_only_stage_context -- --exact
   cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_preserve_evidence_through_blocker_detours -- --exact
   ```

   Expected result: every command exits with status `0`; compiled `implement` and `revise` actions have equal `task.key`, `task.instructions`, and `output`, have different `task.turn` values, backward routes carry `changes_needed`/`change_context`, and existing evidence/artifact routing remains unchanged.

4. Generate captures from actual persisted workflow execution through `WorkflowRuntime` and `AgentExecutor`:

   ```bash
   capture_dir=target/prompt-captures/reduce_agent_step_prompt_payloads
   rm -rf "$capture_dir"
   mkdir -p "$capture_dir"
   export COWBOY_PROMPT_CAPTURE_DIR="$capture_dir"

   cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_implementation_revision_reuses_delivered_contract -- --exact --nocapture
   cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_implementation_retry_sends_only_retry_nudge -- --exact --nocapture
   cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_loaded_implementation_session_reuses_delivered_contract -- --exact --nocapture
   cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_recreated_implementation_session_resends_contract -- --exact --nocapture
   ```

   Each test must copy the repository's `examples/workflows` tree into its temporary workflow root, add only a deterministic routing entrypoint that imports the real `steps/implement.lua` and `steps/revise.lua`, and execute steps with `start_run_with_workflow_stepwise`/`step_run`. The loaded and recreated tests must drop the first runtime, reopen the same SQLite store in a second runtime, and configure the scripted client to respectively succeed or fail `load_session`.

   Expected result: every command exits with status `0` and creates these non-empty files from prompts observed inside `ScriptedAgentClient::prompt`, not from direct prompt-builder calls:

   ```text
   target/prompt-captures/reduce_agent_step_prompt_payloads/same_runtime/implement.prompt.txt
   target/prompt-captures/reduce_agent_step_prompt_payloads/same_runtime/revise.prompt.txt
   target/prompt-captures/reduce_agent_step_prompt_payloads/same_runtime/records.json
   target/prompt-captures/reduce_agent_step_prompt_payloads/retry/initial.prompt.txt
   target/prompt-captures/reduce_agent_step_prompt_payloads/retry/retry.prompt.txt
   target/prompt-captures/reduce_agent_step_prompt_payloads/retry/records.json
   target/prompt-captures/reduce_agent_step_prompt_payloads/loaded_session/implement.prompt.txt
   target/prompt-captures/reduce_agent_step_prompt_payloads/loaded_session/revise.prompt.txt
   target/prompt-captures/reduce_agent_step_prompt_payloads/loaded_session/records.json
   target/prompt-captures/reduce_agent_step_prompt_payloads/recreated_session/implement.prompt.txt
   target/prompt-captures/reduce_agent_step_prompt_payloads/recreated_session/revise.prompt.txt
   target/prompt-captures/reduce_agent_step_prompt_payloads/recreated_session/records.json
   ```

   Each `records.json` must be produced from persisted `StepRecord.input.context`, `StepDetail.session_id`, and scripted-client session events. It must include the implementation/revision task keys, contract fingerprints, prompt-block arrays, session ids, load attempts/results, and newly created session ids.

5. Verify the real dispatched prompts and persisted metadata:

   ```bash
   python3 crates/workflow/engine/tests/verify_prompt_captures.py \
     target/prompt-captures/reduce_agent_step_prompt_payloads
   ```

   Expected result: the script prints `persisted workflow prompt comparisons passed`. Same-runtime and successfully loaded revision dispatches retain the original fingerprint/session and send only the revision turn; retry sends only the correction nudge; load failure creates a new session and resends the full durable contract plus the revision turn.

6. Run the affected-crate regression and lint gates:

   ```bash
   cargo test -p cowboy-workflow-lua
   cargo test -p cowboy-workflow-agent
   cargo test -p cowboy-workflow-engine
   cargo clippy -p cowboy-workflow-core -p cowboy-workflow-lua -p cowboy-workflow-agent -p cowboy-workflow-engine --all-targets -- -D warnings
   git diff --check
   ```

   Expected result: every command exits with status `0`; no test, compiler, Clippy, formatting, or whitespace warning remains.

# TODO

- [x] TODO-01: Add the backward-compatible structured agent task contract and Lua conversion.
  - Procedure:
    1. Add the optional task key, static instructions, and recovery-context fields to the core agent action model.
    2. Update Lua conversion and serialization tests for structured and legacy prompt-only actions.
    3. Run `cargo test -p cowboy-workflow-core agent_action` and `cargo test -p cowboy-workflow-lua agent_action`.
  - Expected result: Structured actions preserve all task components, invalid blank keys are rejected, and legacy prompt-only workflow snapshots still compile and execute with their existing behavior.
  - Observed result: Core and Lua now round-trip stable task keys, static instructions, recovery context, and minimal turns; blank task keys are rejected, while legacy prompt-only actions remain supported. The declared core `agent_action` filter ran 2 passing tests, and the declared Lua `agent_action` filter ran 4 passing tests covering structured conversion, blank-key rejection, and legacy compatibility.

- [x] TODO-02: Persist per-role task-contract delivery state and implement minimal prompt block selection.
  - Procedure:
    1. Add a defaulted delivered-task fingerprint map to `RoleSession`.
    2. Refactor prompt assembly and executor delivery-state updates for fresh, loaded, reused, changed-contract, retry, and user-input-delta cases.
    3. Run `cargo test -p cowboy-workflow-agent prompt` and `cargo test -p cowboy-workflow-agent reused_session`.
  - Expected result: A fresh session receives role, task, recovery context, turn, user inputs, and deliverable blocks; a reused session revisiting the same task receives only the current turn and new user inputs; a new or changed task and a recreated session receive the required full recovery blocks.
  - Observed result: `RoleSession` persists defaulted task fingerprints, executor recreation preserves loaded-session delivery state, load failure resets it, reused tasks send only turn/input deltas, changed tasks resend contract blocks, and structured retries omit the prior task, turn, and deliverable. Both declared prompt/reused-session filters exited with status 0.

- [x] TODO-03: Expose the persisted previous-step output through a canonical structured Lua context.
  - Procedure:
    1. Add `ctx.prev.output` in `LuaStepActionProvider` from the persisted `StepOutput`.
    2. Preserve the existing flattened aliases.
    3. Run `cargo test -p cowboy-workflow-engine lua_provider_exposes_previous_step_output`.
  - Expected result: Lua observes identical status, fields, body, and raw values through both the canonical nested object and compatibility aliases, including ask-user and non-agent outputs.
  - Observed result: `ctx.prev.output` now contains the persisted status, fields, body, and raw value while flattened aliases remain unchanged; the exact runner regression test compared both views and exited with status 0.

- [x] TODO-04: Add structured change handoffs and minimal revision prompt helpers to the example workflow library.
  - Procedure:
    1. Add `changes_needed` and `change_context` validation/rendering to `examples/workflows/utils/context.lua`.
    2. Update backward-routing reviewer, tester, validator, result-feedback, and blocker-review outputs to populate those fields.
    3. Add helper tests that render the exact `Changes needed:` and `Context:` shape from `ctx.prev.output.fields`.
    4. Run `cargo test -p cowboy-workflow-lua examples_workflows`.
  - Expected result: Every backward route supplies a non-empty structured change list and necessary context, and the revision helper omits unrelated fields, previous raw output, and full previous bodies.
  - Observed result: Backward reviewer, tester, validator, result-feedback, confirmation, and blocker-recovery paths now provide `changes_needed` and `change_context`; the revision helper renders only the exact headings and selected values. The complete `examples_workflows` test filter passed 28 tests with status 0.

- [x] TODO-05: Migrate feature, bug-fix, and dev-loop agent steps to stable shared task keys and separate recovery context from turn deltas.
  - Procedure:
    1. Add a shared implementation-contract module under `examples/workflows/` containing the single `implementation` key, durable static instructions, and output specification used by both `examples/workflows/steps/implement.lua` and `examples/workflows/steps/revise.lua`.
    2. Refactor `context.build_agent_contract` or its callers so initial implementation wording and revision-only instructions are rendered exclusively into `task.turn`; neither step may derive `task.instructions` from its distinct objective, heading, guidance, or instruction body.
    3. Keep recovery context stage-specific and complete enough for fresh-session recreation, but exclude recovery context and turn text from the fingerprint. Use distinct task keys for responsibilities whose static instructions or output specifications differ.
    4. Add `loader::tests::examples_workflows_implementation_steps_share_identical_contract` in `crates/workflow/lua/src/loader.rs`. Compile the feature, bug-fix, and dev-loop workflows, execute their real `implement` and `revise` Lua steps with valid previous outputs, and assert equal task keys, static instructions, and output specifications plus unequal initial/revision turns.
    5. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_implementation_steps_share_identical_contract -- --exact`.
    6. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_emit_structured_revision_handoffs -- --exact`.
    7. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_matrix_uses_only_stage_context -- --exact`.
    8. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_preserve_evidence_through_blocker_detours -- --exact`.
  - Expected result: Returning work to the same responsibility reuses its delivered task contract, distinct responsibilities still receive their own contracts, and all existing workflow statuses, artifact preservation rules, and evidence requirements continue to route correctly.
  - Observed result: Feature, bug-fix, and dev-loop implementation/revision steps now import one shared `implementation` contract with identical static instructions and output specification; their compiled actions retain distinct initial/revision turns. All four exact compiled-workflow checks passed with status 0, including structured routing and evidence-preservation coverage.

- [x] TODO-06: Add end-to-end prompt-size and session-recovery regression coverage.
  - Procedure:
    1. Remove `captures_structured_prompt_composition_matrix` and its direct `build_prompt_blocks` capture helper from `crates/workflow/engine/tests/example_prompt_composition.rs`; retain only integration coverage there that exercises its original example-prompt responsibility.
    2. Extend the `ScriptedAgentFactory`/client test harness in `crates/workflow/engine/src/runtime.rs` with configurable successful/failed `load_session` behavior and structured recording of actual `prompt` calls, load attempts/results, and newly created session ids.
    3. Add a runtime-test helper that copies the repository's `examples/workflows` tree into a temporary workflow directory and writes a deterministic entrypoint importing the real `steps/implement.lua`, `steps/revise.lua`, and implementer role. Route a seed status to implement, a structured `changes_needed`/`change_context` feedback status to revise, and revise to completion.
    4. Run `capture_dir=target/prompt-captures/reduce_agent_step_prompt_payloads`.
    5. Run `rm -rf "$capture_dir"`.
    6. Run `mkdir -p "$capture_dir"`.
    7. Run `export COWBOY_PROMPT_CAPTURE_DIR="$capture_dir"`.
    8. Run `cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_implementation_revision_reuses_delivered_contract -- --exact --nocapture`.
    9. Run `cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_implementation_retry_sends_only_retry_nudge -- --exact --nocapture`.
    10. Run `cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_loaded_implementation_session_reuses_delivered_contract -- --exact --nocapture`.
    11. Run `cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_recreated_implementation_session_resends_contract -- --exact --nocapture`.
    12. Run `python3 crates/workflow/engine/tests/verify_prompt_captures.py target/prompt-captures/reduce_agent_step_prompt_payloads`.
  - Expected result: Reused-session revision prompts contain only the required delta and new user inputs, are less than half the corresponding initial prompt length in representative loops, while fresh-session fallback remains self-contained and produces the same workflow result.
  - Observed result: Synthetic prompt capture was removed. Four exact persisted-runtime tests now copy and execute the real implementation/revision modules through `WorkflowRuntime`, SQLite, the action dispatcher, `AgentExecutor`, and scripted client-boundary capture. The checked-in Python comparison command passed and proved turn-only same-runtime/load-success revisions, retry-only correction, full-contract load-failure recreation, stable fingerprints, expected session identities, and revision prompts below half the initial length.

- [x] TODO-07: Document the prompt contract and complete affected-crate quality checks.
  - Procedure:
    1. Update `docs/workflow-authoring.md`, `docs/architecture.md`, and `docs/module-map.md` to state that reuse requires equal key plus equal fingerprinted static instructions/output specification, and document that initial/revision wording belongs in `task.turn`.
    2. Document the persisted runtime validation path: compiled real workflow modules, SQLite role-session state, actual client-boundary prompt capture, successful session loading, load-failure recreation, retry behavior, capture paths, and `records.json` provenance.
    3. Run `cargo fmt --all -- --check`.
    4. Run `cargo test -p cowboy-workflow-core state::tests::role_session_defaults_delivery_state -- --exact`.
    5. Run `cargo test -p cowboy-workflow-core action::tests::structured_agent_task_contract_round_trips -- --exact`.
    6. Run `cargo test -p cowboy-workflow-lua convert::tests::converts_structured_agent_task_contract -- --exact`.
    7. Run `cargo test -p cowboy-workflow-lua convert::tests::legacy_agent_prompt_remains_supported -- --exact`.
    8. Run `cargo test -p cowboy-workflow-engine runner::tests::lua_provider_exposes_previous_step_output -- --exact`.
    9. Run `cargo test -p cowboy-workflow-agent prompt::tests::structured_prompt_block_matrix -- --exact`.
    10. Run `cargo test -p cowboy-workflow-agent executor::tests::structured_task_contract_delivery_matrix -- --exact`.
    11. Run `cargo test -p cowboy-workflow-agent executor::tests::structured_task_contract_survives_loaded_session -- --exact`.
    12. Run `cargo test -p cowboy-workflow-agent executor::tests::fresh_session_fallback_resends_recovery_context -- --exact`.
    13. Run `cargo test -p cowboy-workflow-agent executor::tests::retry_omits_already_delivered_task_and_turn -- --exact`.
    14. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_implementation_steps_share_identical_contract -- --exact`.
    15. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_emit_structured_revision_handoffs -- --exact`.
    16. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_matrix_uses_only_stage_context -- --exact`.
    17. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_preserve_evidence_through_blocker_detours -- --exact`.
    18. Run `capture_dir=target/prompt-captures/reduce_agent_step_prompt_payloads`.
    19. Run `rm -rf "$capture_dir"`.
    20. Run `mkdir -p "$capture_dir"`.
    21. Run `export COWBOY_PROMPT_CAPTURE_DIR="$capture_dir"`.
    22. Run `cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_implementation_revision_reuses_delivered_contract -- --exact --nocapture`.
    23. Run `cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_implementation_retry_sends_only_retry_nudge -- --exact --nocapture`.
    24. Run `cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_loaded_implementation_session_reuses_delivered_contract -- --exact --nocapture`.
    25. Run `cargo test -p cowboy-workflow-engine runtime::tests::workflow_runtime_recreated_implementation_session_resends_contract -- --exact --nocapture`.
    26. Run `python3 crates/workflow/engine/tests/verify_prompt_captures.py target/prompt-captures/reduce_agent_step_prompt_payloads`.
    27. Run `cargo test -p cowboy-workflow-lua`.
    28. Run `cargo test -p cowboy-workflow-agent`.
    29. Run `cargo test -p cowboy-workflow-engine`.
    30. Run `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-lua -p cowboy-workflow-agent -p cowboy-workflow-engine --all-targets -- -D warnings`.
    31. Run `git diff --check`.
    32. Run `git status --short -- target/prompt-captures/reduce_agent_step_prompt_payloads` and confirm it prints no tracked or untracked repository changes.
    33. Run `python3 crates/workflow/engine/tests/verify_prompt_captures.py target/prompt-captures/reduce_agent_step_prompt_payloads` and confirm its only success line is `persisted workflow prompt comparisons passed`.
  - Expected result: Documentation matches the implemented API and recovery semantics, all focused tests pass, formatting is clean, and Clippy reports no warnings in the affected crates.
  - Observed result: Authoring, architecture, and module-map documentation now describes equal-key/equal-fingerprint reuse, turn-only initial/revision wording, and persisted runtime capture provenance. Every explicitly listed command was rerun in order, including capture setup, all focused/runtime checks, the checked-in Python comparison, 77 Lua tests, 83 agent tests plus 2 binary tests, 171 engine tests plus the integration test, affected-crate Clippy with `-D warnings`, formatting, and `git diff --check`; the capture-path status was empty and the final comparison printed only `persisted workflow prompt comparisons passed`.
