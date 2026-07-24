# Plan

Continue and finish the existing uncommitted `action.workflow` implementation so a Lua step can invoke another catalog workflow with an explicit initial request. Review and adapt the current worktree changes rather than replacing them wholesale. The child must be a normal durable `WorkflowRun`, use the same catalog, store, locks, agent factory, config-set resolution, `WorkflowRunner`, event bus, retry policy, and cancellation path as a top-level run, and execute until it completes, fails, is cancelled, or asks for input.

Use the catalog workflow id shown by `/workflows` as the `workflow` field, matching `cowboy run --workflow`; do not resolve by the Lua-declared display name. The Lua shape is:

```lua
return action.workflow {
  workflow = "review/security",
  request = "Review the current implementation"
}
```

When the child completes, create a parent `StepRecord` with action `"workflow"` and copy the child terminal step's `StepOutput.status`, `fields`, `body`, and `raw` unchanged. This lets the parent route directly on the status returned by the child workflow and lets its next step consume the child result through `ctx.prev`. Record the target workflow id, exact request, and child run id in the parent step input/detail rather than injecting metadata into the child output.

Map a child `RunStatus::Failed` to a completed parent workflow-action output with status `"failed"` and the child run id/reason in its diagnostic fields. Map a child cancellation to status `"cancelled"`. If the child waits for input, mirror its prompt as the parent's existing `RunStatus::WaitingForInput` with a durable workflow-child resume callback. Answering the parent prompt must answer and continue the child through the shared executor; if the child asks again, refresh the mirrored parent prompt, and if it becomes terminal, complete the parent workflow action and continue the parent automatically.

Make child creation idempotent across retries, resume, and process interruption without assuming that the parent `StepRecord` already exists. Before child creation, derive a stable workflow invocation UUID-v5 from the durable tuple `(parent run id, current step id, previous head hash or no-head marker)`. Use the fixed namespace `d31e45ed-1aac-578f-9314-765ee417df28`. Encode the UUID name as the ASCII domain tag `cowboy.workflow.invocation.v1\0`, followed by each UTF-8 string as an unsigned 64-bit big-endian byte length and its exact bytes; encode `previous_head` as one byte `0x00` for `None`, or `0x01` followed by the same length-prefixed UTF-8 representation for `Some`. Those values are already persisted before the action runs and change when the same step is reached again after another completed record. Use `run-<invocation-uuid>` as the child run id, persist parent run id, parent step id, parent previous head, and invocation id as child lineage, and reuse only a child whose stored workflow id, exact request, and lineage all match. Enable the existing `uuid` dependency's `v5` feature rather than adding a second identity implementation. Walk durable lineage before acquiring/executing the child to reject direct or indirect workflow-call cycles with a clear error, preventing recursive lock waits and unbounded workflow trees.

# Changes

- In `crates/workflow/core/src/action.rs`, add `StepAction::Workflow(WorkflowAction)` with required `workflow` and `request` strings, serde support, and `"workflow"` action-name coverage.
- In `crates/workflow/core/src/state.rs`, add backward-compatible optional child-lineage metadata to `WorkflowRun`, identifying the parent run, parent step, parent previous head, and stable workflow invocation id. Update constructors/fixtures and serialization compatibility tests without changing the SQLite schema, which already stores the run as serialized data.
- In `crates/workflow/core/src/traits.rs`, make resume callback handling async so a durable callback can continue a child workflow before returning the parent's next `ActionResult`.
- In `crates/workflow/lua/src/api.rs` and `crates/workflow/lua/src/convert.rs`, register `action.workflow`, require a nonblank catalog workflow id, require `request` to be a string while preserving it exactly, and report malformed tables through the existing action-field errors.
- Add `crates/workflow/actions/src/workflow.rs` with a provider-neutral `WorkflowActionHandler` adapter. Extend `EngineActionDispatcher` to route `StepAction::Workflow` without making `cowboy-workflow-actions` depend on the engine or catalog crates.
- Update `ResumeCallbackRegistry`, `AskUserActionRunner`, and `cowboy-workflow-engine::ResumeRouter` for async callback dispatch while preserving current ask-user behavior.
- In `crates/workflow/engine/src/runtime.rs`, implement the workflow action handler and workflow-child resume callback by factoring the existing explicit-workflow start/resume/answer logic into one reusable execution path used by both public parent operations and nested children. Child runs must independently compile/snapshot their target source, resolve their own config set, acquire their own run lock, and use the same `run_existing_with_events`/`WorkflowRunner` path.
- Add stable workflow-invocation/child-run identity and validation helpers in the engine using the fixed UUID-v5 namespace and length-prefixed canonical byte encoding defined above. On first dispatch, create and persist the linked child; on retry or parent resume, load and continue the same child; on a terminal child, normalize it into the parent workflow-action record. Record `workflow`, exact `request`, `child_run_id`, and `invocation_id` under the parent workflow step's `StepInput.context`, with `StepDetail.backend = "workflow"` and `StepDetail.session_id = child_run_id`.
- Enable the `v5` feature on the engine crate's existing `uuid` dependency.
- Add lineage-cycle validation before child execution. Reject calls when the target catalog id already appears in the current run's ancestor chain, and include the attempted workflow chain in the error without including request contents.
- Keep event logs scoped by run id when parent and child runners subscribe to the shared `EventBus`, so child events persist only in the child log and parent events only in the parent log. Emit exact parent `StepProgress` messages `child workflow <workflow> started (<child-run-id>)`, `child workflow <workflow> waiting for input (<child-run-id>)`, `child workflow <workflow> resumed (<child-run-id>)`, and `child workflow <workflow> finished (<child-run-id>, status=<status>)` so the active parent is not silent and tests can assert stable progress text.
- Preserve existing top-level CLI/TUI behavior by representing child input through the parent's existing waiting prompt and by returning a normal parent `StepCompleted` event for action `"workflow"`; no workflow-specific command grammar is required.
- Update `docs/workflow-authoring.md`, `docs/architecture.md`, `docs/module-map.md`, and the repository guidance where complete action lists are enumerated. Document catalog-id lookup, exact request preservation, output propagation, failure/cancellation statuses, interactive child behavior, durable child runs, idempotent resume, and cycle rejection.

# Tests to be added/updated

- Add `action::tests::workflow_action_serializes_and_names_variant` for `WorkflowAction` tagged serialization/deserialization and `StepAction::action_name`.
- Add `state::tests::workflow_run_parent_lineage_defaults_and_round_trips` proving old serialized runs default child lineage to `None` and linked child runs round-trip with parent run/step/previous-head/invocation identities.
- Add `runtime::tests::workflow_action_converts_and_preserves_request` and `runtime::tests::workflow_action_rejects_invalid_fields` in the Lua crate for a valid `action.workflow`, missing/blank/non-string `workflow`, missing/non-string `request`, and exact whitespace/newline preservation.
- Add `tests::action_dispatcher_routes_workflow_variant` in the actions crate using a fake `WorkflowActionHandler` to prove the workflow variant is routed only to that handler and its completed/blocked results pass through unchanged.
- Add `tests::async_ask_user_resume_callback_preserves_behavior` in the actions crate and `input::tests::async_resume_router_preserves_ask_user_behavior` in the engine crate before adding workflow-child callbacks.
- Add `runtime::tests::workflow_action_child_uses_shared_executor_and_persists_lineage` proving the child uses the requested catalog id and exact initial request, runs through the same internal executor helper as a top-level run, resolves its own config set, and persists the defined lineage.
- Add `runtime::tests::workflow_action_invocation_id_matches_fixed_uuid_v5_vector` proving the production identity helper matches a predetermined UUID-v5 namespace/name/result vector computed independently of production helpers.
- Add `runtime::tests::workflow_action_propagates_terminal_child_output` proving a custom child terminal status and `fields`/`body`/`raw` are copied unchanged and route the parent.
- Add `runtime::tests::workflow_action_maps_failed_and_cancelled_children` proving child failure and cancellation become routable `"failed"` and `"cancelled"` parent outputs with diagnostics.
- Add `runtime::tests::workflow_action_parent_answers_repeated_child_prompts` and `runtime::tests::workflow_action_parent_prompt_survives_runtime_reconstruction` proving the parent mirrors/answers child prompts, handles a second prompt, and resumes after rebuilding `WorkflowRuntime`.
- Add `runtime::tests::workflow_action_retry_and_resume_reuse_child_invocation` and `runtime::tests::workflow_action_rejects_mismatched_existing_child` proving UUID-v5 invocation reuse across retry/resume and explicit rejection of workflow/request/lineage mismatch.
- Add `runtime::tests::workflow_action_rejects_direct_cycle_before_lock` and `runtime::tests::workflow_action_rejects_indirect_cycle_before_lock` proving `A -> A` and `A -> B -> A` fail before a conflicting ancestor lock is acquired.
- Add `runtime::tests::workflow_action_nested_events_are_isolated_and_parent_reports_progress` proving parent and child event files contain only their own run ids while the combined start/answer operation reports expose child lifecycle progress and final action completion.
- Update exhaustive `StepAction` matches, workflow-run fixtures, action lists, and documentation examples across core, actions, Lua, engine, store contracts/test apps, and repository guidance.

# How to verify

1. Create the evidence directories and define manifest-backed command helpers. Every recorded command recomputes the current Git revision and a SHA-256 worktree fingerprint covering `HEAD`, tracked changes, and untracked file contents; records UTC start/finish timestamps, the shell-reproducible command, raw-log path, and exit status in `source-manifest.jsonl`; and keeps raw command output separate from assertion reports:

   ```bash
   set -euo pipefail
   mkdir -p target/workflow-action-validation
   EVID_ROOT="$(
     mktemp -d \
       "target/workflow-action-validation/run-$(date -u +%Y%m%dT%H%M%S)-XXXXXXXX"
   )"
   export EVID_ROOT
   mkdir -p "$EVID_ROOT/raw" "$EVID_ROOT/assertions" "$EVID_ROOT/artifacts"
   : > "$EVID_ROOT/source-manifest.jsonl"

   worktree_fingerprint() {
     {
       git rev-parse HEAD
       git diff --binary HEAD
       while IFS= read -r -d '' path; do
         printf 'untracked:%s\n' "$path"
         sha256sum "$path"
       done < <(git ls-files --others --exclude-standard -z | sort -z)
     } | sha256sum | awk '{print $1}'
   }

   record_command() {
     label="$1"
     shift
     python3 - "$EVID_ROOT/source-manifest.jsonl" "$label" <<'PY'
   import json
   import pathlib
   import sys

   manifest, label = sys.argv[1:]
   entries = [
       json.loads(line)
       for line in pathlib.Path(manifest).read_text(encoding="utf-8").splitlines()
       if line.strip()
   ]
   assert all(entry["label"] != label for entry in entries), f"duplicate label: {label}"
   PY
     raw_log="$EVID_ROOT/raw/${label}.log"
     command_text="$(printf '%q ' "$@")"
     started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
     head_revision="$(git rev-parse HEAD)"
     fingerprint="$(worktree_fingerprint)"
     case "$-" in
       *e*) had_errexit=1 ;;
       *) had_errexit=0 ;;
     esac
     set +e
     "$@" 2>&1 | tee "$raw_log"
     exit_status="${PIPESTATUS[0]}"
     if [ "$had_errexit" -eq 1 ]; then
       set -e
     else
       set +e
     fi
     finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
     python3 - \
       "$EVID_ROOT/source-manifest.jsonl" "$label" "$command_text" \
       "$started_at" "$finished_at" "$head_revision" "$fingerprint" \
       "$exit_status" "$raw_log" "${STAMP-}" "${PREFIX-}" \
       "${SMOKE_ROOT-}" "${EVID-}" "$EVID_ROOT" \
       "${PARENT_RUN_ID-}" "${CHILD_RUN_ID-}" "${DB-}" <<'PY'
   import json
   import sys

   (
       manifest,
       label,
       command,
       started_at,
       finished_at,
       head_revision,
       worktree_fingerprint,
       exit_status,
       raw_log,
       stamp,
       prefix,
       smoke_root,
       evidence_dir,
       evidence_root,
       parent_run_id,
       child_run_id,
       database_path,
   ) = sys.argv[1:]
   with open(manifest, "a", encoding="utf-8") as stream:
       stream.write(json.dumps({
           "label": label,
           "command": command,
           "started_at": started_at,
           "finished_at": finished_at,
           "head_revision": head_revision,
           "worktree_fingerprint": worktree_fingerprint,
           "exit_status": int(exit_status),
           "raw_log": raw_log,
           "environment": {
               "STAMP": stamp,
               "PREFIX": prefix,
               "SMOKE_ROOT": smoke_root,
               "EVID": evidence_dir,
               "EVID_ROOT": evidence_root,
               "PARENT_RUN_ID": parent_run_id,
               "CHILD_RUN_ID": child_run_id,
               "DB": database_path,
           },
       }, sort_keys=True) + "\n")
   PY
     return "$exit_status"
   }

   run_exact() {
     package="$1"
     selector="$2"
     label="$3"
     raw_log="$EVID_ROOT/raw/${label}.log"
     assertion="$EVID_ROOT/assertions/${label}.txt"
     record_command "$label" cargo test -p "$package" "$selector" -- --exact --nocapture
     record_command "${label}-assert" bash -o pipefail -c '
       selector="$1"
       raw_log="$2"
       assertion="$3"
       {
         grep -F "test $selector ... ok" "$raw_log"
         grep -F "test result: ok. 1 passed; 0 failed;" "$raw_log"
       } | tee "$assertion"
     ' _ "$selector" "$raw_log" "$assertion"
   }
   ```

   Execute each labeled command at most once within one validation root; TODO command blocks and the consolidated list above refer to the same captured invocations. If source changes after any recorded command or a command must be retried, create a new unique `EVID_ROOT` and rerun the complete validation sequence rather than appending conflicting evidence.

2. Run every named focused test:

   ```bash
   run_exact cowboy-workflow-core action::tests::workflow_action_serializes_and_names_variant core-action
   run_exact cowboy-workflow-core state::tests::workflow_run_parent_lineage_defaults_and_round_trips core-lineage
   run_exact cowboy-workflow-lua runtime::tests::workflow_action_converts_and_preserves_request lua-convert
   run_exact cowboy-workflow-lua runtime::tests::workflow_action_rejects_invalid_fields lua-invalid
   run_exact cowboy-workflow-actions tests::action_dispatcher_routes_workflow_variant actions-dispatch
   run_exact cowboy-workflow-actions tests::async_ask_user_resume_callback_preserves_behavior actions-async-resume
   run_exact cowboy-workflow-engine input::tests::async_resume_router_preserves_ask_user_behavior engine-input
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_child_uses_shared_executor_and_persists_lineage engine-shared-executor
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_invocation_id_matches_fixed_uuid_v5_vector engine-uuid-vector
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_retry_and_resume_reuse_child_invocation engine-idempotent
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_rejects_mismatched_existing_child engine-mismatch
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_rejects_direct_cycle_before_lock engine-direct-cycle
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_rejects_indirect_cycle_before_lock engine-indirect-cycle
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_propagates_terminal_child_output engine-terminal
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_maps_failed_and_cancelled_children engine-terminal-failure
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_parent_answers_repeated_child_prompts engine-prompts
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_parent_prompt_survives_runtime_reconstruction engine-prompt-restart
   run_exact cowboy-workflow-engine runtime::tests::workflow_action_nested_events_are_isolated_and_parent_reports_progress engine-events
   ```

3. Run `record_command store-tests cargo test -p cowboy-workflow-store` and require its raw log to report zero failures, covering serialized-run persistence through the public store contract.
4. Execute TODO-10's reviewed-source anchor and named-failure comparison. The anchored files/function must remain byte-identical to their fixed `HEAD` reference, and both isolated/workspace failure-name sets must equal the one documented test name without relying on pass/fail counts.
5. Run `record_command clippy cargo clippy --workspace --all-targets -- -D warnings` and require exit status zero in `source-manifest.jsonl`.
6. Execute the end-to-end smoke procedure under TODO-11. Keep the complete evidence tree unchanged until the reviewer has reproduced the assertions; cleanup is a reviewer-controlled follow-up, not an implementer completion step.

# TODO

- [x] TODO-01: Add the declarative workflow action and durable child-lineage model.
  - Procedure: Update `crates/workflow/core/src/action.rs` and `crates/workflow/core/src/state.rs`. Define `run_exact` exactly as shown in **How to verify**, then run:

    ```bash
    run_exact cowboy-workflow-core action::tests::workflow_action_serializes_and_names_variant core-action
    run_exact cowboy-workflow-core state::tests::workflow_run_parent_lineage_defaults_and_round_trips core-lineage
    ```

  - Expected result: Both logs contain their exact named `test ... ok` line and `test result: ok. 1 passed; 0 failed;`. The tests assert `StepAction::Workflow` round-trips as `action = "workflow"`, old run JSON loads with `parent = None`, and linked child JSON retains parent run id, step id, previous head, and invocation id.
  - Implementer observed result: Both exact core tests passed once. Tagged workflow actions round-trip and report the `"workflow"` action name; legacy run JSON defaults `parent` to `None`, while linked child lineage round-trips all four identity fields.
- [x] TODO-02: Add and validate `action.workflow` in the Lua authoring/runtime conversion layer.
  - Procedure: Register the helper in `crates/workflow/lua/src/api.rs`, convert it in `crates/workflow/lua/src/convert.rs`, add the two named tests in `crates/workflow/lua/src/runtime.rs`, define `run_exact` as shown in **How to verify**, and run:

    ```bash
    run_exact cowboy-workflow-lua runtime::tests::workflow_action_converts_and_preserves_request lua-convert
    run_exact cowboy-workflow-lua runtime::tests::workflow_action_rejects_invalid_fields lua-invalid
    ```

  - Expected result: Both exact tests execute once and pass. The first asserts a nonblank catalog id and byte-for-byte request value including leading/trailing whitespace and a newline; the second individually asserts field-specific errors for missing, blank, and non-string `workflow`, plus missing and non-string `request`.
  - Implementer observed result: Both exact Lua tests passed once. Valid conversion preserved request whitespace and newlines byte-for-byte, and each malformed workflow/request field produced its expected field-specific error.
- [x] TODO-03: Add a provider-neutral workflow action handler to the actions dispatcher.
  - Procedure: Add `crates/workflow/actions/src/workflow.rs`, extend `EngineActionDispatcher`, update exhaustive matches, add `tests::action_dispatcher_routes_workflow_variant`, define `run_exact` as shown in **How to verify**, and run:

    ```bash
    run_exact cowboy-workflow-actions tests::action_dispatcher_routes_workflow_variant actions-dispatch
    ```

  - Expected result: The exact test executes once and passes after asserting the fake workflow handler receives the original action/context exactly once, the fake agent handler receives no call, and completed and blocked workflow-handler results are returned unchanged.
  - Implementer observed result: The exact dispatcher test passed once, proving workflow actions route only to the workflow handler with unchanged action/context and that completed and blocked results pass through unchanged.
- [x] TODO-04: Make durable resume callback dispatch asynchronous without changing ask-user semantics.
  - Procedure: Update `ResumeCallbackHandler`, `ResumeCallbackRegistry`, `AskUserActionRunner`, and `ResumeRouter`; add the two named regression tests; define `run_exact` as shown in **How to verify**; run:

    ```bash
    run_exact cowboy-workflow-actions tests::async_ask_user_resume_callback_preserves_behavior actions-async-resume
    run_exact cowboy-workflow-engine input::tests::async_resume_router_preserves_ask_user_behavior engine-input
    ```

  - Expected result: Both exact tests execute once and pass. They assert the original prompt id/message/choices/callback payload survive dispatch, valid answers still produce the same ask-user `StepRecord`, invalid answers still fail before callback execution, and a callback can await before returning its `ActionResult`.
  - Implementer observed result: Both exact async-resume regression tests passed once. Prompt and callback data remained unchanged, invalid answers were rejected before callback execution, and awaited callbacks returned the original ask-user action result.
- [x] TODO-05: Reuse the engine's existing workflow execution path for top-level and child runs.
  - Procedure: Refactor `crates/workflow/engine/src/runtime.rs` so explicit top-level starts and the workflow action handler call one internal executor helper for catalog compile, config resolution, run creation/resume, lock acquisition, dispatcher construction, `WorkflowRunner`, cancellation, and event persistence. Add `runtime::tests::workflow_action_child_uses_shared_executor_and_persists_lineage`, but do not use a test-only entry counter as proof that the paths are shared; remove that probe if no other test requires it. Define `run_exact` and `record_command` as shown in **How to verify**, run the behavioral test, then execute an independent source-structure assertion that extracts Rust function bodies without invoking production helpers:

    ```bash
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_child_uses_shared_executor_and_persists_lineage engine-shared-executor
    record_command engine-shared-structure python3 - <<'PY'
    from pathlib import Path

    path = Path("crates/workflow/engine/src/runtime.rs")
    source = path.read_text(encoding="utf-8")
    production = source[:source.index("\n#[cfg(test)]\nmod tests")]

    def section(start_marker, end_marker):
        start = production.index(start_marker)
        end = production.index(end_marker, start)
        return production[start:end]

    start_catalog = section(
        "async fn start_catalog_workflow(",
        "async fn execute_catalog_run(",
    )
    execute_catalog = section(
        "async fn execute_catalog_run(",
        "fn validate_catalog_run_match(",
    )
    execute_loaded = section(
        "async fn execute_loaded_run(",
        "async fn run_workflow_action(",
    )
    child_action = section(
        "async fn run_workflow_action(",
        "async fn resume_workflow_child(",
    )
    shared_executor = section(
        "async fn run_existing_with_events(",
        "fn workflow_event_from_agent_progress(",
    )

    assert start_catalog.count("execute_catalog_run(") == 1
    assert child_action.count("execute_catalog_run(") == 1
    assert "execute_loaded_run(" in execute_catalog
    assert "run_existing_with_events(" in execute_catalog
    assert execute_loaded.count("run_existing_with_events(") == 1
    assert production.count("WorkflowRunner::new(") == 1
    assert shared_executor.count("WorkflowRunner::new(") == 1
    print("PASS: top-level and child catalog starts converge before the sole WorkflowRunner construction")
    PY
    ```

  - Expected result: The exact behavioral test executes once and proves exactly two durable runs, exact child workflow/request selection, distinct source snapshots, child config-set resolution, and normal parent/child lifecycle events. The independently recorded structural command exits zero and prints the exact `PASS` line after proving top-level and child catalog starts both enter `execute_catalog_run`, all new/existing-run branches converge on `run_existing_with_events`, and the production file contains exactly one `WorkflowRunner::new` inside that shared executor.
  - Implementer observed result: The exact behavioral test passed once with two durable runs, exact catalog id/request selection, distinct snapshots, child config resolution, and lifecycle events. The independent source assertion printed its exact `PASS` line and found one shared `WorkflowRunner::new` construction.
- [x] TODO-06: Make child execution idempotent and reject workflow-call cycles.
  - Procedure: Enable UUID-v5 in `crates/workflow/engine/Cargo.toml`. Derive the invocation UUID before child creation from the canonical persisted tuple `(parent run id, current step id, previous head hash or no-head marker)`; do not use the not-yet-completed parent `StepRecord`. Use namespace `d31e45ed-1aac-578f-9314-765ee417df28` and exactly the domain-tagged, unsigned-64-bit big-endian length-prefixed byte encoding defined in **Plan**. Add `runtime::tests::workflow_action_invocation_id_matches_fixed_uuid_v5_vector` with hard-coded vectors independently computed outside the production helper:

    - `("run-parent", "delegate", None)` must encode as hex `636f77626f792e776f726b666c6f772e696e766f636174696f6e2e763100000000000000000a72756e2d706172656e74000000000000000864656c656761746500` and produce UUID `b31f15e7-8e04-5446-b17e-ffc74442ce9a`.
    - `("run-parent", "delegate", Some("abc123"))` must encode as hex `636f77626f792e776f726b666c6f772e696e766f636174696f6e2e763100000000000000000a72756e2d706172656e74000000000000000864656c6567617465010000000000000006616263313233` and produce UUID `4434c4a7-e226-5ab2-9593-58f07f609931`.

    Use the resulting `run-<uuid>` child id, persist the tuple plus UUID as lineage, validate existing child workflow/request/lineage before reuse, and walk ancestor lineage before any child lock acquisition. Add the four existing named tests, define `run_exact` as shown in **How to verify**, and run:

    ```bash
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_invocation_id_matches_fixed_uuid_v5_vector engine-uuid-vector
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_retry_and_resume_reuse_child_invocation engine-idempotent
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_rejects_mismatched_existing_child engine-mismatch
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_rejects_direct_cycle_before_lock engine-direct-cycle
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_rejects_indirect_cycle_before_lock engine-indirect-cycle
    ```

  - Expected result: All five exact tests execute once and pass. The fixed-vector test compares the production encoder and UUID result to both hard-coded byte/UUID vectors without deriving expected values through production helpers. The idempotency test asserts identical invocation/child ids across retry, parent resume, and runtime reconstruction with exactly one child row; the mismatch test mutates each stored workflow/request/lineage component in separate cases and observes an explicit mismatch error; the cycle tests observe the full workflow chain in the error and a lock-spy count of zero for the rejected ancestor target.
  - Implementer observed result: All five exact identity, reuse, mismatch, and cycle tests passed once. The canonical byte encoder and UUID-v5 helper matched both hard-coded vectors, retries/reconstruction reused one child, mismatches were rejected, and direct/indirect cycles failed before ancestor lock acquisition.
- [x] TODO-07: Propagate terminal child results into a normal parent workflow step record.
  - Procedure: Implement normalization for completed, failed, and cancelled child runs; add the two named runtime tests; define `run_exact` as shown in **How to verify**; run:

    ```bash
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_propagates_terminal_child_output engine-terminal
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_maps_failed_and_cancelled_children engine-terminal-failure
    ```

  - Expected result: Both exact tests execute once and pass. The terminal-output test compares the complete child and parent `StepOutput` values for equality and verifies the parent takes the custom-status transition; the failure/cancellation test verifies statuses `"failed"`/`"cancelled"`, child id/reason diagnostics, `StepInput.context` metadata, and `StepDetail.backend/session_id`.
  - Implementer observed result: Both exact terminal-result tests passed once. Completed output was copied unchanged and routed by custom status, while failed/cancelled children produced the documented routable diagnostics and workflow step metadata.
- [x] TODO-08: Support interactive child workflows through the parent's existing input flow.
  - Procedure: Persist a workflow-child resume callback when the child waits, mirror the child prompt on the parent, and route `WorkflowRuntime::answer_run(parent_id, ...)` through the child executor. Add the two named tests, define `run_exact` as shown in **How to verify**, and run:

    ```bash
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_parent_answers_repeated_child_prompts engine-prompts
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_parent_prompt_survives_runtime_reconstruction engine-prompt-restart
    ```

  - Expected result: Both exact tests execute once and pass. The repeated-prompt test asserts the first and second child prompt ids/messages/choices appear on the parent in order, both answers use only the parent run id, the child id remains unchanged, and parent/child complete after the second answer. The reconstruction test drops and recreates `WorkflowRuntime` between prompt and answer, then asserts the persisted callback resumes the same child and advances the parent automatically.
  - Implementer observed result: Both exact interactive-child tests passed once. Repeated prompts were answered through the parent id with one stable child id, and a reconstructed runtime resumed the persisted child callback and completed the parent automatically.
- [x] TODO-09: Isolate nested event collection and expose parent-visible child progress.
  - Procedure: Filter both the receive loop and final drain in `run_existing_with_events` by the target run id; subscribe before async workflow-child callbacks so their parent progress is included in the current operation report; emit parent progress at child start/wait/resume/finish; add `runtime::tests::workflow_action_nested_events_are_isolated_and_parent_reports_progress` with explicit nonempty event-set assertions for these four surfaces: `report_start.events` may contain only `parent_run_id`; `report_answer.events` may contain only `parent_run_id`; the persisted parent event file may contain only `parent_run_id`; and the persisted child event file may contain only `child_run_id`. Each parent surface must explicitly reject `child_run_id`, and the child file must explicitly reject `parent_run_id`. Define `run_exact` as shown in **How to verify**; and run:

    ```bash
    run_exact cowboy-workflow-engine runtime::tests::workflow_action_nested_events_are_isolated_and_parent_reports_progress engine-events
    ```

  - Expected result: The exact test executes once and passes with these observed run-id sets: `ids(report_start.events) == {parent_run_id}`, `ids(report_answer.events) == {parent_run_id}`, `ids(parent_event_file) == {parent_run_id}`, and `ids(child_event_file) == {child_run_id}`. Concatenated parent operation reports contain the four exact lifecycle progress messages in order, the final parent report/file contains exactly one `StepCompleted { action: "workflow" }`, and the child file contains `RunCompleted`.
  - Implementer observed result: The exact nested-event test passed once. Parent reports and persisted parent events contained only the parent id, child events contained only the child id, all four lifecycle messages appeared in order, and the parent emitted exactly one completed workflow action.
- [x] TODO-10: Update all affected fixtures, exhaustive matches, documentation, and repository guidance.
  - Procedure: Define `record_command` as shown in **How to verify**. Build an explicit source inventory, generate and complete a one-row-per-match review ledger, run a zero-stale-enumeration guard, and compare workspace failures to the isolated baseline by test name rather than hard-coded counts:

    ```bash
    INVENTORY="$EVID_ROOT/artifacts/exhaustive-inventory.log"
    REVIEW="$EVID_ROOT/artifacts/inventory-review.tsv"
    record_command exhaustive-inventory bash -o pipefail -c '
      rg --sort path -n \
        -e "StepAction::" \
        -e "action\\.(agent|command|status|ask_user|fail|workflow)" \
        -e "WorkflowRun \\{" \
        crates README.md AGENTS.md docs/architecture.md docs/module-map.md docs/workflow-authoring.md \
        | tee "$1"
    ' _ "$INVENTORY"
    test -s "$INVENTORY"

    record_command inventory-review-template python3 - "$INVENTORY" "$REVIEW" <<'PY'
    import pathlib
    import sys

    inventory = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
    review = pathlib.Path(sys.argv[2])
    review.write_text(
        "".join(f"{line}\tUNREVIEWED\tUNREVIEWED\tUNREVIEWED\n" for line in inventory),
        encoding="utf-8",
    )
    PY
    ```

    Review every ledger row and replace the final three columns with `<category>\tPASS\t<rationale>` under these explicit criteria:

    1. `StepAction::` match/exhaustiveness rows pass only when `Workflow` is handled wherever the match is exhaustive; intentionally partial matches must name why workflow is out of scope.
    2. Action-enumeration rows pass only when a complete action list contains `workflow` exactly once; examples intentionally showing a subset must be labeled `subset`, not treated as complete enumerations.
    3. `WorkflowRun {` rows pass only when the constructor explicitly sets `parent`, or the row is a documented backward-compatibility fixture whose test proves omitted lineage deserializes to `None`.

    Record the manually completed ledger as a source-labeled artifact before validating it:

    ```bash
    record_command inventory-review-complete python3 - \
      "$INVENTORY" "$REVIEW" \
      "$EVID_ROOT/artifacts/inventory-review-completion.json" <<'PY'
    import hashlib
    import json
    import pathlib
    import sys
    from datetime import datetime, timezone

    inventory_path, review_path, output_path = map(pathlib.Path, sys.argv[1:])
    inventory = inventory_path.read_bytes()
    review = review_path.read_bytes()
    assert inventory and review
    record = {
        "source_label": "manual-ledger-completion",
        "procedure": "TODO-10 inventory ledger review criteria 1-3",
        "completed_at": datetime.now(timezone.utc).isoformat(),
        "inventory_path": str(inventory_path),
        "inventory_sha256": hashlib.sha256(inventory).hexdigest(),
        "review_path": str(review_path),
        "review_sha256": hashlib.sha256(review).hexdigest(),
        "review_row_count": len(review.decode("utf-8").splitlines()),
    }
    pathlib.Path(output_path).write_text(
        json.dumps(record, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    print("PASS: source-labeled manual ledger completion recorded")
    PY

    record_command inventory-review-validate python3 - "$INVENTORY" "$REVIEW" <<'PY'
    from collections import Counter
    import pathlib
    import sys

    inventory = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
    rows = [
        line.split("\t", 3)
        for line in pathlib.Path(sys.argv[2]).read_text(encoding="utf-8").splitlines()
    ]
    assert all(len(row) == 4 for row in rows)
    assert Counter(row[0] for row in rows) == Counter(inventory)
    assert all(row[1] in {"exhaustive_match", "partial_match", "complete_list", "subset", "run_constructor", "compatibility_fixture"} for row in rows)
    assert all(row[2] == "PASS" for row in rows)
    assert all(row[3].strip() and row[3] != "UNREVIEWED" for row in rows)
    print(f"PASS: reviewed {len(rows)} inventory rows with no unreviewed findings")
    PY

    FINAL_INVENTORY="$EVID_ROOT/artifacts/exhaustive-inventory-final.log"
    record_command exhaustive-inventory-final bash -o pipefail -c '
      rg --sort path -n \
        -e "StepAction::" \
        -e "action\\.(agent|command|status|ask_user|fail|workflow)" \
        -e "WorkflowRun \\{" \
        crates README.md AGENTS.md docs/architecture.md docs/module-map.md docs/workflow-authoring.md \
        | tee "$1"
    ' _ "$FINAL_INVENTORY"
    record_command inventory-currentness bash -o pipefail -c '
      cmp "$1" "$2"
      echo "PASS: reviewed inventory exactly matches regenerated final inventory"
    ' _ "$INVENTORY" "$FINAL_INVENTORY"

    record_command stale-enumeration-guard bash -o pipefail -c '
      if rg -n \
        -e "\\[\"agent\", \"command\", \"status\", \"ask_user\", \"fail\"\\]" \
        -e "\`agent\`, \`command\`, \`status\`, \`ask_user\`, \`fail\`" \
        -e "agent, command, status, ask_user, and fail" \
        crates README.md AGENTS.md docs/architecture.md docs/module-map.md docs/workflow-authoring.md \
        > "$1"; then
        cat "$1"
        exit 1
      fi
      test ! -s "$1"
    ' _ "$EVID_ROOT/artifacts/stale-five-action-enumerations.log"

    record_command baseline-source-anchor python3 - \
      "$EVID_ROOT/assertions/baseline-source-anchor.txt" <<'PY'
    import hashlib
    import pathlib
    import subprocess
    import sys

    output_path = pathlib.Path(sys.argv[1])
    fixed_files = {
        "examples/workflows/steps/triage_blocked.lua":
            "37a938d6c51c4225539cc16f3ed673b21dec3324dbd7d2118d1f2544b6168bcc",
        "examples/workflows/workflows/feature.lua":
            "a59bae25aaf10f371794e66c2f7fb5c6513aa93ec78dfeec0f4911b09ff451e4",
    }
    for path, expected_hash in fixed_files.items():
        head_bytes = subprocess.check_output(["git", "show", f"HEAD:{path}"])
        current_bytes = pathlib.Path(path).read_bytes()
        assert hashlib.sha256(head_bytes).hexdigest() == expected_hash
        assert current_bytes == head_bytes, f"baseline source changed: {path}"

    runtime_path = "crates/workflow/engine/src/runtime.rs"
    marker = "    async fn example_blocked_step_requests_user_direction()"
    next_test = "    #[tokio::test]"

    def test_function(source):
        start = source.index(marker)
        end = source.index(next_test, start + len(marker))
        return source[start:end].encode()

    head_runtime = subprocess.check_output(
        ["git", "show", f"HEAD:{runtime_path}"],
        text=True,
    )
    current_runtime = pathlib.Path(runtime_path).read_text(encoding="utf-8")
    head_function = test_function(head_runtime)
    current_function = test_function(current_runtime)
    expected_test_hash = "a61b3c3995faf886089d21f145bf42435cd90b654736e0dc7609c8e228820106"
    assert hashlib.sha256(head_function).hexdigest() == expected_test_hash
    assert current_function == head_function, "baseline test function changed"
    report = "PASS: exact known-failure sources match the reviewed HEAD reference\n"
    output_path.write_text(report, encoding="utf-8")
    print(report, end="")
    PY

    set +e
    record_command known-baseline cargo test -p cowboy-workflow-engine \
      runtime::tests::example_blocked_step_requests_user_direction -- --exact --nocapture
    BASELINE_STATUS=$?
    record_command workspace-tests cargo test --workspace --no-fail-fast
    WORKSPACE_STATUS=$?
    set -e

    record_command workspace-failure-compare python3 - \
      "$EVID_ROOT/raw/known-baseline.log" \
      "$EVID_ROOT/raw/workspace-tests.log" \
      "$BASELINE_STATUS" "$WORKSPACE_STATUS" \
      "$EVID_ROOT/assertions/workspace-failure-compare.txt" <<'PY'
    import pathlib
    import re
    import sys

    baseline_path, workspace_path, baseline_status, workspace_status, output_path = sys.argv[1:]

    def failure_names(path):
        names = set()
        inside = False
        for line in pathlib.Path(path).read_text(encoding="utf-8").splitlines():
            if line == "failures:":
                inside = True
                continue
            if inside and line.startswith("test result:"):
                inside = False
                continue
            if inside and re.fullmatch(r"    [A-Za-z0-9_:]+", line):
                names.add(line.strip())
        return sorted(names)

    expected_failure = "runtime::tests::example_blocked_step_requests_user_direction"
    baseline = failure_names(baseline_path)
    workspace = failure_names(workspace_path)
    baseline_status = int(baseline_status)
    workspace_status = int(workspace_status)
    baseline_text = pathlib.Path(baseline_path).read_text(encoding="utf-8")
    assert baseline_status != 0
    assert workspace_status != 0
    assert baseline == [expected_failure], baseline
    assert workspace == [expected_failure], workspace
    assert 'left: "implement"' in baseline_text
    assert 'right: "revise"' in baseline_text
    report = (
        f"PASS: baseline_failures={baseline!r}; "
        f"workspace_failures={workspace!r}; statuses=({baseline_status},{workspace_status})"
    )
    pathlib.Path(output_path).write_text(report + "\n", encoding="utf-8")
    print(report)
    PY
    ```

  - Expected result: The initial inventory is nonempty; `inventory-review.tsv` has exactly one `PASS` row per inventory line with an allowed category and concrete rationale; `inventory-review-completion.json` records the manual procedure label, completion timestamp, row count, and SHA-256 values for both inventory and completed ledger; the regenerated final inventory is byte-for-byte identical to the reviewed inventory; and the stale-enumeration artifact is empty. `baseline-source-anchor.txt` proves the two workflow files and exact engine test function still match the reviewed `HEAD` content and fixed SHA-256 values. The isolated test must fail only as `runtime::tests::example_blocked_step_requests_user_direction` with the documented `implement`/`revise` mismatch, and the workspace failure-name set must equal that one anchored failure exactly. No fixed total-test count is used, every focused workflow-action test has already passed, and any change to the anchored source requires explicit replanning rather than silently redefining the baseline.
  - Implementer observed result: The path-sorted 420-row source inventory was reviewed with one categorized `PASS` rationale per row, and an independent regeneration matched it byte-for-byte. The stale five-action guard was empty, anchored sources matched `HEAD`, and the isolated/workspace failure-name sets both contained only the documented `example_blocked_step_requests_user_direction` baseline mismatch.

- [x] TODO-11: Perform final linting and an end-to-end parent/interactive-child smoke test.
  - Procedure: Define `record_command` as shown in **How to verify**. Use `mktemp`-generated validation and smoke directories so an earlier unreviewed run is never overwritten and same-second starts cannot collide. Keep raw logs under `$EVID_ROOT/raw`, generated state under the unique `target/workflow-action-smoke/<run-key>`, durable artifacts under `$EVID_ROOT/smoke/<run-key>/artifacts`, and assertion reports under `$EVID_ROOT/smoke/<run-key>/assertions`.

    1. Create a unique run and record the exact smoke fixtures:

       ```bash
       mkdir -p target/workflow-action-smoke "$EVID_ROOT/smoke"
       SMOKE_ROOT="$(
         mktemp -d \
           "target/workflow-action-smoke/smoke-$(date -u +%Y%m%dT%H%M%S)-XXXXXXXX"
       )"
       STAMP="${SMOKE_ROOT##*/}"
       PREFIX="$STAMP"
       EVID="$EVID_ROOT/smoke/$STAMP"
       export STAMP PREFIX SMOKE_ROOT EVID EVID_ROOT
       mkdir "$EVID"
       mkdir "$SMOKE_ROOT/workflows" "$EVID/artifacts" "$EVID/assertions"

       record_command "$PREFIX-fixtures" bash -o pipefail -c '
         cat > "$SMOKE_ROOT/config.toml" <<EOF
       state_dir = "$SMOKE_ROOT/state"
       workflow_store = "$SMOKE_ROOT/state/data.db"
       workflow_dirs = ["$SMOKE_ROOT/workflows"]

       [config_sets.default]
       max_steps_per_run = 20
       max_visits_per_step = 5
       max_retries_per_run = 4
       max_retries_per_step = 1

       [[agents]]
       name = "default"
       command = "false"
       args = []
       EOF
         cat > "$SMOKE_ROOT/workflows/child.lua" <<EOF
       local finish = step("finish")
       finish.run = function(ctx)
         return action.status {
           status = "child_ok",
           fields = { answer = ctx.prev.fields.answer, marker = "child-output" },
           body = "child completed",
         }
       end
       local confirm = step("confirm")
       confirm.run = function(ctx)
         return action.ask_user {
           id = "child-confirm",
           message = "Continue child?",
           choices = { "yes" },
         }
       end
       confirm:on("answered", finish)
       return workflow("smoke-child", confirm)
       EOF
         cat > "$SMOKE_ROOT/workflows/parent.lua" <<EOF
       local finish = step("finish")
       finish.run = function(ctx)
         return action.status {
           status = "success",
           fields = {
             answer = ctx.prev.fields.answer,
             child_status = ctx.prev.status,
             marker = ctx.prev.fields.marker,
           },
           body = "parent completed",
         }
       end
       local call_child = step("call_child")
       call_child.run = function(ctx)
         return action.workflow {
           workflow = "child",
           request = "child smoke request",
         }
       end
       call_child:on("child_ok", finish)
       return workflow("smoke-parent", call_child)
       EOF
       '
       ```

    2. Record both builds, Clippy, and the three CLI operations:

       ```bash
       record_command "$PREFIX-build-cowboy" cargo build -p cowboy --bin cowboy
       record_command "$PREFIX-build-store-cli" cargo build -p cowboy-workflow-store --bin store-cli
       record_command "$PREFIX-clippy" cargo clippy --workspace --all-targets -- -D warnings
       record_command "$PREFIX-start" target/debug/cowboy \
         --config "$SMOKE_ROOT/config.toml" run --workflow parent "parent smoke request"

       record_command "$PREFIX-parent-id" bash -o pipefail -c '
         sed -n "s/^run=\\([^ ]*\\).*/\\1/p" "$EVID_ROOT/raw/$PREFIX-start.log" \
           | head -n 1 | tee "$EVID/artifacts/parent-run-id.txt"
         test -s "$EVID/artifacts/parent-run-id.txt"
       '
       PARENT_RUN_ID="$(cat "$EVID/artifacts/parent-run-id.txt")"
       export PARENT_RUN_ID

       record_command "$PREFIX-answer" target/debug/cowboy \
         --config "$SMOKE_ROOT/config.toml" answer "$PARENT_RUN_ID" child-confirm yes
       record_command "$PREFIX-runs" target/debug/cowboy \
         --config "$SMOKE_ROOT/config.toml" runs
       record_command "$PREFIX-child-id" bash -o pipefail -c '
         awk "/^run-/{id=\$0} /^  workflow: child$/{print id; exit}" \
           "$EVID_ROOT/raw/$PREFIX-runs.log" \
           | tee "$EVID/artifacts/child-run-id.txt"
         test -s "$EVID/artifacts/child-run-id.txt"
       '
       CHILD_RUN_ID="$(cat "$EVID/artifacts/child-run-id.txt")"
       export CHILD_RUN_ID
       ```

    3. Produce a separate CLI assertion report:

       ```bash
       record_command "$PREFIX-cli-assert" bash -o pipefail -c '
         START="$EVID_ROOT/raw/$PREFIX-start.log"
         ANSWER="$EVID_ROOT/raw/$PREFIX-answer.log"
         RUNS="$EVID_ROOT/raw/$PREFIX-runs.log"
         {
           grep -F "status=WaitingForInput" "$START"
           grep -F "prompt_id: \"child-confirm\"" "$START"
           grep -F "child workflow child started ($CHILD_RUN_ID)" "$START"
           grep -F "child workflow child waiting for input ($CHILD_RUN_ID)" "$START"
           grep -F "run=$PARENT_RUN_ID workflow=parent status=Completed" "$ANSWER"
           grep -F "child workflow child resumed ($CHILD_RUN_ID)" "$ANSWER"
           grep -F "child workflow child finished ($CHILD_RUN_ID, status=child_ok)" "$ANSWER"
           test "$(grep -c "^run-" "$RUNS")" -eq 2
           test "$(grep -c "^  workflow: parent$" "$RUNS")" -eq 1
           test "$(grep -c "^  workflow: child$" "$RUNS")" -eq 1
           echo "PASS: CLI parent/child lifecycle verified"
         } > "$EVID/assertions/cli.txt"
         cat "$EVID/assertions/cli.txt"
       '
       ```

    4. Record every store query that creates a durable JSON artifact:

       ```bash
       DB="$SMOKE_ROOT/state/data.db"
       export DB
       record_command "$PREFIX-replay-context" python3 - \
         "$PARENT_RUN_ID" "$CHILD_RUN_ID" "$DB" \
         "$EVID/artifacts/replay-context.json" <<'PY'
       import json
       import pathlib
       import sys

       parent_run_id, child_run_id, database_path, output_path = sys.argv[1:]
       pathlib.Path(output_path).write_text(json.dumps({
           "PARENT_RUN_ID": parent_run_id,
           "CHILD_RUN_ID": child_run_id,
           "DB": database_path,
       }, sort_keys=True, indent=2) + "\n", encoding="utf-8")
       print("PASS: replay-critical variables captured")
       PY
       record_command "$PREFIX-load-parent" bash -o pipefail -c \
         'target/debug/store-cli "$DB" load-run "$PARENT_RUN_ID" > "$EVID/artifacts/parent-run.json"'
       record_command "$PREFIX-load-child" bash -o pipefail -c \
         'target/debug/store-cli "$DB" load-run "$CHILD_RUN_ID" > "$EVID/artifacts/child-run.json"'
       record_command "$PREFIX-load-records" bash -o pipefail -c '
         PARENT_HEAD="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))[\"head\"])" "$EVID/artifacts/parent-run.json")"
         CHILD_HEAD="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))[\"head\"])" "$EVID/artifacts/child-run.json")"
         target/debug/store-cli "$DB" get-step "$PARENT_HEAD" > "$EVID/artifacts/parent-final-record.json"
         target/debug/store-cli "$DB" get-step "$CHILD_HEAD" > "$EVID/artifacts/child-terminal-record.json"
         WORKFLOW_HEAD="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))[\"prev\"])" "$EVID/artifacts/parent-final-record.json")"
         target/debug/store-cli "$DB" get-step "$WORKFLOW_HEAD" > "$EVID/artifacts/parent-workflow-record.json"
       '
       record_command "$PREFIX-copy-events" bash -o pipefail -c '
         cp "$SMOKE_ROOT/state/events/$PARENT_RUN_ID.json" "$EVID/artifacts/parent-events.json"
         cp "$SMOKE_ROOT/state/events/$CHILD_RUN_ID.json" "$EVID/artifacts/child-events.json"
       '
       ```

    5. Record separate durable-state and event-isolation assertion reports:

       ```bash
       record_command "$PREFIX-durable-assert" python3 - "$PARENT_RUN_ID" "$CHILD_RUN_ID" "$EVID" <<'PY'
       import json
       import pathlib
       import sys

       parent_id, child_id, evidence_dir = sys.argv[1:]
       root = pathlib.Path(evidence_dir)
       load = lambda name: json.loads((root / "artifacts" / name).read_text())
       parent = load("parent-run.json")
       child = load("child-run.json")
       parent_final = load("parent-final-record.json")
       parent_call = load("parent-workflow-record.json")
       child_terminal = load("child-terminal-record.json")
       assert parent["id"] == parent_id and parent["status"] == {"status": "completed"}
       assert child["id"] == child_id and child["workflow_name"] == "child"
       assert child["original_request"] == "child smoke request"
       assert child["status"] == {"status": "completed"}
       assert child["parent"]["run_id"] == parent_id
       assert child["parent"]["step_id"] == "call_child"
       assert child["parent"]["previous_head"] is None
       assert child["parent"]["invocation_id"]
       assert parent_call["action"] == "workflow"
       assert parent_call["input"]["context"] == {
           "workflow": "child",
           "request": "child smoke request",
           "child_run_id": child_id,
           "invocation_id": child["parent"]["invocation_id"],
       }
       assert parent_call["detail"]["backend"] == "workflow"
       assert parent_call["detail"]["session_id"] == child_id
       assert parent_call["output"] == child_terminal["output"]
       assert parent_call["output"]["status"] == "child_ok"
       assert parent_call["output"]["fields"] == {"answer": "yes", "marker": "child-output"}
       assert parent_call["output"]["body"] == "child completed"
       assert parent_final["output"]["fields"] == {
           "answer": "yes",
           "child_status": "child_ok",
           "marker": "child-output",
       }
       assert parent_final["output"]["status"] == "success"
       assert parent_final["output"]["body"] == "parent completed"
       report = "PASS: parent-child lineage and output propagation verified\n"
       (root / "assertions" / "durable-state.txt").write_text(report)
       print(report, end="")
       PY

       record_command "$PREFIX-event-assert" python3 - "$PARENT_RUN_ID" "$CHILD_RUN_ID" "$EVID" <<'PY'
       import json
       import pathlib
       import sys

       parent_id, child_id, evidence_dir = sys.argv[1:]
       root = pathlib.Path(evidence_dir)
       parent_events = json.loads((root / "artifacts" / "parent-events.json").read_text())
       child_events = json.loads((root / "artifacts" / "child-events.json").read_text())
       assert {event["run_id"] for event in parent_events} == {parent_id}
       assert {event["run_id"] for event in child_events} == {child_id}
       assert sum(
           event["kind"]["kind"] == "step_completed"
           and event["kind"]["action"] == "workflow"
           for event in parent_events
       ) == 1
       assert any(event["kind"]["kind"] == "run_completed" for event in child_events)
       report = "PASS: parent and child event files are isolated\n"
       (root / "assertions" / "event-isolation.txt").write_text(report)
       print(report, end="")
       PY
       ```

    6. Link every retained artifact to its producing manifest entry and verify required command exits:

       ```bash
       record_command "$PREFIX-provenance" python3 - "$EVID_ROOT/source-manifest.jsonl" "$PREFIX" "$SMOKE_ROOT" "$EVID" <<'PY'
       import hashlib
       import json
       import pathlib
       import sys

       manifest_path, prefix, smoke_root, evidence_dir = sys.argv[1:]
       evidence_root = pathlib.Path(manifest_path).parent
       evidence = pathlib.Path(evidence_dir)
       entries = [
           json.loads(line)
           for line in pathlib.Path(manifest_path).read_text().splitlines()
           if line.strip()
       ]
       by_label = {entry["label"]: entry for entry in entries}
       focused_test_labels = [
           "core-action", "core-lineage", "lua-convert", "lua-invalid",
           "actions-dispatch", "actions-async-resume", "engine-input",
           "engine-shared-executor", "engine-uuid-vector", "engine-idempotent",
           "engine-mismatch", "engine-direct-cycle", "engine-indirect-cycle",
           "engine-terminal", "engine-terminal-failure", "engine-prompts",
           "engine-prompt-restart", "engine-events",
       ]
       required_global_labels = {
           "store-tests", "clippy", "engine-shared-structure",
           "exhaustive-inventory", "inventory-review-template",
           "inventory-review-complete", "inventory-review-validate",
           "exhaustive-inventory-final", "inventory-currentness",
           "stale-enumeration-guard", "baseline-source-anchor",
           "known-baseline", "workspace-tests", "workspace-failure-compare",
       }
       for label in focused_test_labels:
           required_global_labels.add(label)
           required_global_labels.add(f"{label}-assert")
       required_smoke_labels = {
           f"{prefix}-fixtures", f"{prefix}-build-cowboy",
           f"{prefix}-build-store-cli", f"{prefix}-clippy", f"{prefix}-start",
           f"{prefix}-parent-id", f"{prefix}-answer", f"{prefix}-runs",
           f"{prefix}-child-id", f"{prefix}-cli-assert", f"{prefix}-replay-context",
           f"{prefix}-load-parent",
           f"{prefix}-load-child", f"{prefix}-load-records", f"{prefix}-copy-events",
           f"{prefix}-durable-assert", f"{prefix}-event-assert",
       }
       required_labels = required_global_labels | required_smoke_labels
       assert required_labels.issubset(set(by_label)), required_labels - set(by_label)
       expected_nonzero = {"known-baseline", "workspace-tests"}
       for label in required_labels:
           if label in expected_nonzero:
               assert by_label[label]["exit_status"] != 0
           else:
               assert by_label[label]["exit_status"] == 0
       producers = {
           pathlib.Path(smoke_root) / "config.toml": f"{prefix}-fixtures",
           pathlib.Path(smoke_root) / "workflows/child.lua": f"{prefix}-fixtures",
           pathlib.Path(smoke_root) / "workflows/parent.lua": f"{prefix}-fixtures",
           evidence / "artifacts/parent-run-id.txt": f"{prefix}-parent-id",
           evidence / "artifacts/child-run-id.txt": f"{prefix}-child-id",
           evidence / "artifacts/replay-context.json": f"{prefix}-replay-context",
           evidence / "artifacts/parent-run.json": f"{prefix}-load-parent",
           evidence / "artifacts/child-run.json": f"{prefix}-load-child",
           evidence / "artifacts/parent-final-record.json": f"{prefix}-load-records",
           evidence / "artifacts/child-terminal-record.json": f"{prefix}-load-records",
           evidence / "artifacts/parent-workflow-record.json": f"{prefix}-load-records",
           evidence / "artifacts/parent-events.json": f"{prefix}-copy-events",
           evidence / "artifacts/child-events.json": f"{prefix}-copy-events",
           evidence / "assertions/cli.txt": f"{prefix}-cli-assert",
           evidence / "assertions/durable-state.txt": f"{prefix}-durable-assert",
           evidence / "assertions/event-isolation.txt": f"{prefix}-event-assert",
       }
       for label in focused_test_labels:
           producers[evidence_root / "assertions" / f"{label}.txt"] = f"{label}-assert"
       producers.update({
           evidence_root / "artifacts/exhaustive-inventory.log":
               "exhaustive-inventory",
           evidence_root / "artifacts/inventory-review.tsv":
               "inventory-review-complete",
           evidence_root / "artifacts/inventory-review-completion.json":
               "inventory-review-complete",
           evidence_root / "artifacts/exhaustive-inventory-final.log":
               "exhaustive-inventory-final",
           evidence_root / "artifacts/stale-five-action-enumerations.log":
               "stale-enumeration-guard",
           evidence_root / "assertions/baseline-source-anchor.txt":
               "baseline-source-anchor",
           evidence_root / "assertions/workspace-failure-compare.txt":
               "workspace-failure-compare",
       })
       nonempty_artifacts = set(producers)
       allow_empty = {
           evidence_root / "artifacts/stale-five-action-enumerations.log",
       }
       for label in required_labels:
           producers[pathlib.Path(by_label[label]["raw_log"])] = label
       output = evidence / "artifact-provenance.jsonl"
       with output.open("w", encoding="utf-8") as stream:
           for path, label in producers.items():
               assert path.is_file(), path
               if path in nonempty_artifacts and path not in allow_empty:
                   assert path.stat().st_size > 0, path
               producer = by_label[label]
               stream.write(json.dumps({
                   "artifact": str(path),
                   "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                   "producer_label": label,
                   "command": producer["command"],
                   "started_at": producer["started_at"],
                   "finished_at": producer["finished_at"],
                   "head_revision": producer["head_revision"],
                   "worktree_fingerprint": producer["worktree_fingerprint"],
                   "exit_status": producer["exit_status"],
                   "raw_log": producer["raw_log"],
                   "environment": producer["environment"],
               }, sort_keys=True) + "\n")
       print(f"PASS: provenance recorded for {len(producers)} retained artifacts")
       PY
       ```

    7. Seal and validate the complete source manifest after provenance generation:

       ```bash
       record_command "$PREFIX-manifest-seal" true
       python3 - \
         "$EVID_ROOT/source-manifest.jsonl" "$PREFIX" "$EVID" \
         "$EVID/manifest-validation.txt" <<'PY'
       import hashlib
       import json
       import pathlib
       import sys
       from datetime import datetime, timezone

       manifest_path, prefix, evidence_dir, output_path = sys.argv[1:]
       manifest = pathlib.Path(manifest_path)
       evidence = pathlib.Path(evidence_dir)
       entries = [
           json.loads(line)
           for line in manifest.read_text(encoding="utf-8").splitlines()
           if line.strip()
       ]
       labels = [entry["label"] for entry in entries]
       assert len(labels) == len(set(labels)), "duplicate manifest labels"
       identities = {
           (entry["head_revision"], entry["worktree_fingerprint"])
           for entry in entries
       }
       assert len(identities) == 1, identities
       focused_test_labels = [
           "core-action", "core-lineage", "lua-convert", "lua-invalid",
           "actions-dispatch", "actions-async-resume", "engine-input",
           "engine-shared-executor", "engine-uuid-vector", "engine-idempotent",
           "engine-mismatch", "engine-direct-cycle", "engine-indirect-cycle",
           "engine-terminal", "engine-terminal-failure", "engine-prompts",
           "engine-prompt-restart", "engine-events",
       ]
       required_global_labels = {
           "store-tests", "clippy", "engine-shared-structure",
           "exhaustive-inventory", "inventory-review-template",
           "inventory-review-complete", "inventory-review-validate",
           "exhaustive-inventory-final", "inventory-currentness",
           "stale-enumeration-guard", "baseline-source-anchor",
           "known-baseline", "workspace-tests", "workspace-failure-compare",
       }
       for label in focused_test_labels:
           required_global_labels.add(label)
           required_global_labels.add(f"{label}-assert")
       required_smoke_labels = {
           f"{prefix}-fixtures", f"{prefix}-build-cowboy",
           f"{prefix}-build-store-cli", f"{prefix}-clippy", f"{prefix}-start",
           f"{prefix}-parent-id", f"{prefix}-answer", f"{prefix}-runs",
           f"{prefix}-child-id", f"{prefix}-cli-assert",
           f"{prefix}-replay-context", f"{prefix}-load-parent",
           f"{prefix}-load-child", f"{prefix}-load-records",
           f"{prefix}-copy-events", f"{prefix}-durable-assert",
           f"{prefix}-event-assert", f"{prefix}-provenance",
           f"{prefix}-manifest-seal",
       }
       required_labels = required_global_labels | required_smoke_labels
       assert set(labels) == required_labels, {
           "missing": sorted(required_labels - set(labels)),
           "unexpected": sorted(set(labels) - required_labels),
       }
       by_label = {entry["label"]: entry for entry in entries}
       expected_nonzero = {"known-baseline", "workspace-tests"}
       for label in required_labels:
           entry = by_label[label]
           if label in expected_nonzero:
               assert entry["exit_status"] != 0, entry
           else:
               assert entry["exit_status"] == 0, entry
       replay = json.loads(
           (evidence / "artifacts" / "replay-context.json").read_text(encoding="utf-8")
       )
       assert replay["PARENT_RUN_ID"]
       assert replay["CHILD_RUN_ID"]
       assert replay["DB"]
       replay_labels = {
           f"{prefix}-replay-context", f"{prefix}-load-parent",
           f"{prefix}-load-child", f"{prefix}-load-records",
           f"{prefix}-copy-events", f"{prefix}-durable-assert",
           f"{prefix}-event-assert", f"{prefix}-provenance",
           f"{prefix}-manifest-seal",
       }
       for label in replay_labels:
           environment = by_label[label]["environment"]
           assert environment["PARENT_RUN_ID"] == replay["PARENT_RUN_ID"]
           assert environment["CHILD_RUN_ID"] == replay["CHILD_RUN_ID"]
           assert environment["DB"] == replay["DB"]
       provenance = evidence / "artifact-provenance.jsonl"
       assert provenance.is_file() and provenance.stat().st_size > 0
       provenance_raw = pathlib.Path(by_label[f"{prefix}-provenance"]["raw_log"])
       seal_raw = pathlib.Path(by_label[f"{prefix}-manifest-seal"]["raw_log"])
       assert provenance_raw.is_file() and seal_raw.is_file()
       head_revision, worktree_fingerprint = next(iter(identities))
       report = {
           "status": "PASS",
           "entry_count": len(entries),
           "expected_global_label_count": len(required_global_labels),
           "expected_smoke_label_count": len(required_smoke_labels),
           "head_revision": head_revision,
           "worktree_fingerprint": worktree_fingerprint,
           "manifest_sha256": hashlib.sha256(manifest.read_bytes()).hexdigest(),
           "provenance_sha256": hashlib.sha256(provenance.read_bytes()).hexdigest(),
           "provenance_raw_sha256": hashlib.sha256(provenance_raw.read_bytes()).hexdigest(),
           "manifest_seal_raw_sha256": hashlib.sha256(seal_raw.read_bytes()).hexdigest(),
           "parent_run_id": replay["PARENT_RUN_ID"],
           "child_run_id": replay["CHILD_RUN_ID"],
           "database_path": replay["DB"],
           "validated_at": datetime.now(timezone.utc).isoformat(),
           "validator_procedure": "TODO-11 step 7",
       }
       pathlib.Path(output_path).write_text(
           json.dumps(report, sort_keys=True, indent=2) + "\n",
           encoding="utf-8",
       )
       print("PASS: manifest labels, identities, exits, and replay context validated")
       PY
       ```

       `source-manifest.jsonl` and `artifact-provenance.jsonl` are root evidence metadata, and `manifest-validation.txt` is the root verification report. These root files are intentionally exempt from recursive self-hashing: the source manifest records the provenance/seal commands; the verification report hashes both root metadata files plus the provenance/seal raw logs; and the provenance index hashes all earlier non-root global and smoke artifacts, including raw logs, focused-test assertion reports, structural/store/lint output, both inventories, the completed ledger and its source-labeled completion record, baseline/workspace reports, smoke fixtures, captured state/events, and smoke assertion reports, without attempting to hash itself, the source manifest, or the root verification report.

    8. Do not delete or overwrite `$SMOKE_ROOT`, `$EVID`, the referenced raw logs, `source-manifest.jsonl`, `artifact-provenance.jsonl`, or `manifest-validation.txt` during implementation completion. The reviewer may remove the unique tuple only after independently rerunning the recorded commands or validating all recorded hashes.

  - Expected result: Collision-resistant validation/smoke directories are created without overwriting prior evidence. The source manifest has no duplicate labels and its label set equals the complete enumerated global-plus-smoke set: every focused test and assertion, structural check, store tests, global Clippy, inventory/template/manual-completion/validation/regeneration/currentness checks, stale guard, baseline anchor, isolated/workspace tests and comparison, and every smoke command through provenance/seal. Every entry shares exactly one `HEAD` and worktree fingerprint; all expected-success commands exit zero; and only the two anchored failure-producing test commands are nonzero. Manifest environments and `replay-context.json` preserve identical nonempty `PARENT_RUN_ID`, `CHILD_RUN_ID`, and `DB` values for all replay-dependent commands. Raw logs and assertion reports are separate. The parent waits on `child-confirm`, completes through its own id, and produces exactly one parent and one child run; durable assertions prove lineage, exact child-output propagation, and parent final status `"success"` with body `"parent completed"`; event assertions prove parent/child file isolation. `artifact-provenance.jsonl` provenance-links and hashes every non-root retained global and smoke artifact, `manifest-validation.txt` hashes both root metadata files and records the complete label-set/shared-identity/replay context, and all unique evidence remains present for reviewer reproduction.
  - Implementer observed result: A fresh reviewer rerun executed every affected focused test and assertion, the complete deterministic inventory workflow, both anchored failure-producing commands, Clippy, both builds, and every parent/interactive-child smoke stage as distinct commands. The current smoke completed parent `run-795bcd5c-0afa-4ae4-a9c9-eecdb9b6ded2` with linked child `run-f8a69e83-8b1f-5dbb-af5f-791f13e258c9`; durable output and event-isolation assertions passed, and current provenance plus manifest validation cover the retained rerun artifacts.
