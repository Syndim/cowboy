# Plan

When the TUI is idle after the active workflow run reaches `Completed` or `Failed`, treat the next non-slash composer submission as a restart of that workflow instead of sending it through normal workflow selection. The restart creates a new durable run so the terminal run, step records, counters, event log, and export remain immutable, but it reuses the terminal run's exact snapshotted workflow sources and inherited agent sessions.

Add explicit restart lineage to the new run. The runtime restart entry point must load the source run, accept only `Completed` or `Failed`, compile the source run's `WorkflowSnapshot`, create a new run at the workflow head with fresh step/visit/retry accounting, and record the source run id. It must not consult the selector or current workflow catalog, because "current workflow" means the exact workflow version that produced the visible terminal run.

For every role in the snapshotted workflow, load the source run's persisted `RoleSession`, when one exists, and save an inherited row under the new run id. Preserve the backend session id, `role_instructions_sent`, and `delivered_task_contracts` so role definitions, static task contracts, and deliverable contracts already held by the session are not resent. Reset only `last_sent_input_sequence` because user-input sequence numbers are scoped to a run and the restart request at sequence `0` must be delivered once to each reused role session. Mark the copied row with the existing `PROVIDED_SESSION_BACKEND` exact-load policy; `AgentExecutor::ensure_session` must therefore fail if the session cannot be loaded instead of silently creating a replacement environment and replaying initial prompts. Roles that never created a session in the source run continue normally and create one only if the restarted workflow reaches them.

Apply the same rule recursively to durable workflow-action children. Child and grandchild runs have their own `Run` rows, `WorkflowSnapshot`s, and role sessions keyed by their own run ids, so copying only root sessions is insufficient. Carry the source run id as restart lineage on every cloned run. When a restarted parent reaches `StepAction::Workflow`, use the current step visit ordinal to locate the corresponding source-parent step record in chronological order. If that source occurrence invoked the same workflow, load its recorded `child_run_id`, require a terminal source child with matching `ParentRun` lineage, and create the new child from that source child's exact snapshot, config-set pointer, and role sessions. The new child still uses the newly evaluated `action.request` and its normal deterministic child id under the new parent. Its restart lineage then enables the same lookup recursively for grandchildren. If execution takes a genuinely new branch with no corresponding source workflow invocation, use the existing fresh catalog path; if purported matching source-child data is missing, nonterminal, or inconsistent, fail explicitly instead of silently substituting current catalog code or fresh sessions.

Establish one consistency boundary before any restarted run executes. Acquire the source run's existing sidecar execution lock, compile and validate its stored snapshot, then call one SQLite transaction that re-loads the source run, re-validates the eligible status and expected snapshot hash, and inserts the new run, run head, and all inherited role-session rows atomically. The status observed in that transaction is authoritative. Concurrent resume/resolve and restart serialize on the source lock: whichever operation acquires it first completes its durable mutation first, and the later restart re-validates the resulting status. An injected error while inserting the head or any inherited session must roll back every target row. Only after commit may the runtime execute the new run or emit its start events. Lazily cloned child runs use the same source-child lock and atomic transaction.

Represent the new request as restart input while keeping its content unchanged. `Run.original_request` and `ctx.request` remain the exact submitted text, and the synthesized sequence-zero `UserInput` uses a restart-specific kind when restart lineage is present. Carry that initial-input kind through `ExecutionContext` so both Lua context generation and agent prompt assembly agree. Agent prompt composition must introduce the unseen restart input with wording equivalent to "The user restarted the workflow with the following prompt" before presenting the request. A reused session receives that restart instruction and request plus only workflow/task blocks not already recorded as delivered; it does not receive prior-run user inputs, role definitions, or already-delivered task contracts.

Use the existing reviewed `scripts/run-exact-test.sh` artifact for focused proof gates. It first lists the fully qualified library test and requires exactly one match, then executes that exact test and requires `1 passed`, `0 failed`, `0 ignored`, plus the specified evidence marker. Its `set -euo pipefail` and explicit command-result checks fail on listing/build/test command errors, zero or multiple matches, failed or ignored tests, malformed summaries, and missing markers. This feature does not modify that helper.

Keep evidence generation and semantic validation independent. The deterministic restart fixture writes JSON evidence data only. Add a fixed, checked-in `scripts/validate-restart-workflow-evidence.py` validator whose implementation is reviewable independently of the fixture. The validator owns the evidence schemas and exits nonzero for missing/extra required artifacts, invalid JSON or field types, malformed restart/parent lineage, unequal workflow snapshots or config-set pointers, unequal inherited session identities/delivery metadata, replayed role/task sentinels, missing restart instructions or exact requests, dirty seed counters, unexpected source-run mutation, or inconsistent completed/failed statuses.

The TUI owns only the interaction decision. Keep pending `ask_user` answers and active-agent prompt submission at their current higher priorities. When there is no execution task and the active run status is `Completed` or `Failed`, dispatch plain text to the runtime restart entry point using the active run id. Slash commands retain their existing behavior, and plain text after `Cancelled`, with no active run, or with an unknown durable status continues to start a normal new run.

# Changes

- Extend `crates/workflow/core/src/state.rs` with optional, serde-defaulted restart lineage on `Run` and a `Restart` origin in `UserInputKind`. Update ordered-input construction so sequence `0` is `initial` for ordinary runs and `restart` for restarted runs while preserving exact request text and timestamp rules.
- Extend `crates/workflow/core/src/traits.rs` and all `ExecutionContext` construction sites with the sequence-zero input kind (or equivalent restart metadata), because `AgentExecutor` currently receives only `original_request` and `run_created_at`, not the complete `Run`.
- Carry the current step's post-increment visit ordinal in `ExecutionContext`; workflow-action inheritance uses it to match the same chronological source occurrence, and recoverable retries retain it.
- Extend `crates/workflow/store/src/sqlite_store.rs` with an atomic restart-creation operation and typed outcomes. In one retryable write transaction it must re-load and validate the source run/status/hash, reject an existing target id, and insert the target run, run head, and supplied role sessions together. Add a `cfg(test)` failure-injection seam after run/head insertion and during session insertion to prove complete rollback and pool reuse.
- Extend `crates/workflow/engine/src/runtime.rs` with a public restart operation that:
  - validates that the source run exists and is `Completed` or `Failed`;
  - compiles `snapshot_from_run(source)` and reuses the exact workflow name, hash, source bundle, and durable config-set name without consulting the selector or current catalog;
  - creates a new run id with a clean head and zeroed execution/retry accounting;
  - records restart lineage while leaving the source run immutable;
  - loads role sessions by iterating the compiled definition's role ids, copies each available session id and static delivery metadata, resets the per-run input watermark, and uses `PROVIDED_SESSION_BACKEND` to require exact loading;
  - holds the source run execution guard through the atomic store transaction and begins execution only after the complete seed commits;
  - executes the new run through the existing `WorkflowRunner` and event/report path.
- Update workflow-action dispatch in `crates/workflow/engine/src/runtime.rs` to traverse the restarted source parent's `StepRecord.prev` chain, reverse it into chronological order, select the current step occurrence by visit ordinal, validate the recorded workflow and child lineage, and choose either exact child restart creation or the existing fresh catalog path. A matched child restarts from its own snapshot head with the newly evaluated action request; it does not copy terminal step state.
- Refactor the existing supplied-session construction in `crates/workflow/engine/src/runtime.rs` only as needed to share `PROVIDED_SESSION_BACKEND` exact-load behavior without changing the public `--session-id` contract.
- Update `crates/workflow/agent/src/prompt.rs` so an unseen restart sequence-zero input is introduced by the restart instruction and delivered exactly once. Keep ordinary initial/follow-up wording unchanged and continue deriving role/task inclusion from `RoleSession` delivery state.
- Update `crates/workflow/agent/src/executor.rs` to derive ordered inputs with the `ExecutionContext` initial-input kind, preserve copied restart-session delivery metadata after successful load, and record the restart input/prompt block selection in existing `StepInput.context` diagnostics.
- Update `crates/tui/app/src/app/state.rs` with a terminal restart target derived from the active run id, idle execution state, and durable `Completed`/`Failed` status.
- Update `crates/tui/app/src/app/commands.rs` so plain idle input dispatches in this order: slash command, pending answer, terminal restart, normal new run. Add a restart-specific background card/status label while preserving exact draft/history behavior on successful dispatch and synchronous errors.
- Update TUI composer copy in `crates/tui/app/src/app/controls/composer.rs` if needed so the idle terminal state communicates that Enter restarts the current workflow; do not alter active-agent, waiting-for-input, or cancelled-state affordances.
- Reuse the existing reviewed `scripts/run-exact-test.sh` unchanged for all named focused tests. Document its exact-one discovery, one-pass/no-fail/no-ignore, marker, and command-error guarantees in the verification procedures.
- Add `scripts/validate-restart-workflow-evidence.py` as a fixed repository validator and `scripts/tests/test_validate_restart_workflow_evidence.py` as its standard-library regression suite. Define strict schemas for the fixture config, source/restart run trees, session inheritance, and prompt evidence; reject unknown schema versions; compare source/restart data independently; print one success marker only after all semantic checks pass.
- Update `README.md`, `docs/architecture.md`, and `docs/workflow-authoring.md` to document terminal TUI restart behavior, immutable source runs/new run ids, exact snapshot/config-set reuse, session inheritance, the `restart` sequence-zero input shape, and the exact-session load-failure rule.

# Tests to be added/updated

- Core state tests:
  - old serialized runs without restart lineage still deserialize as ordinary runs;
  - ordinary sequence-zero input remains `initial`;
  - restarted sequence-zero input is `restart`, preserves the exact request and source run id, and keeps follow-up ordering unchanged.
- Core execution-context tests:
  - the runner passes `Initial` for ordinary runs and `Restart` for restarted runs to every action dispatch and recoverable retry;
  - Lua receives the same sequence-zero kind that the agent executor receives.
- Agent prompt tests:
  - a restart input renders the restart instruction immediately before the exact raw request;
  - ordinary initial and follow-up prompt wording is unchanged;
  - reused-session block selection omits role and already-delivered task contracts while including the restart request once;
  - multiline and leading/trailing whitespace in the restart request remain unchanged in the prompt payload.
- Evidence-validator tests:
  - one valid fixture bundle passes and prints the validator success marker;
  - independently mutated bundles fail for every required class: missing artifact, schema/type error, malformed lineage, snapshot mismatch, session mismatch, static-prompt replay, missing restart request/instruction, dirty seed counters, source-tree mutation, and wrong completed/failed status or reason.
- Runtime integration tests:
  - restarting a completed run creates a distinct run id at the same snapshotted workflow head and leaves the source run unchanged;
  - restarting a failed run starts from the workflow head rather than the failed step and resets head, step, visit, and retry accounting;
  - catalog edits or removal after the source run finishes do not change the restarted workflow snapshot;
  - all persisted role sessions are copied with the same session ids, `role_instructions_sent`, and task-contract fingerprints, but with a reset input watermark and `PROVIDED_SESSION_BACKEND`;
  - roles without a source-run session do not receive a fabricated inherited row;
  - the scripted clients load inherited sessions, receive the restart instruction/request, and do not receive role definitions or previously delivered task contracts;
  - an inherited session load failure fails explicitly and creates no replacement session;
  - attempts to restart `Running`, `WaitingForInput`, or `Cancelled` runs are rejected without creating a new run;
  - the copied config-set name follows existing live-limit fallback behavior if that set is later removed from configuration;
  - a parent workflow whose source run called an agent-backed child inherits the source child's exact snapshot and child-owned role sessions under the new child id, even after the child catalog file changes or disappears;
  - a parent-child-grandchild fixture recursively inherits distinct descendant snapshots and sessions, with every new descendant linked to the corresponding source descendant;
  - a divergent branch with no source invocation creates a fresh catalog child, while missing, nonterminal, or lineage-mismatched source-child data fails explicitly;
  - repeated visits to one workflow-action step inherit the source child at the matching visit ordinal rather than repeatedly selecting the first child;
  - concurrent failed-run resolution and restart follow source-lock acquisition order and revalidate the authoritative transactional status;
  - injected failure after target run/head insertion or during inherited-session insertion leaves no target run, head, session, event, or dispatched step.
- TUI command/state/composer tests:
  - plain text after `Completed` calls restart for the active run and does not invoke workflow selection;
  - plain text after `Failed` follows the same restart path;
  - the resulting report switches `active_run_id` to the new run while retaining the old run in persistence;
  - pending answers and active-agent prompt windows still take precedence;
  - plain text after `Cancelled`, with no active run, or with unknown status still starts a normal run;
  - slash commands in terminal states remain slash commands;
  - successful restart submissions enter history once and clear the composer, while rejected restart submissions follow the existing error/history contract;
  - terminal composer text advertises restart behavior without changing other lifecycle-state copy.

# How to verify

1. Run formatting and focused domain/prompt tests:

   ```bash
   cargo fmt --all -- --check
   test -x scripts/run-exact-test.sh
   bash scripts/run-exact-test.sh cowboy-workflow-core \
     state::tests::restart_lineage_deserializes_and_orders_restart_input \
     'EVIDENCE restart-core legacy=true initial=true restart=true'
   bash scripts/run-exact-test.sh cowboy-workflow-core \
     engine::tests::restart_execution_context_preserves_input_kind_and_visit \
     'EVIDENCE restart-context kind=restart visit=stable retry=stable'
   bash scripts/run-exact-test.sh cowboy-workflow-agent \
     prompt::tests::restart_input_prepends_instruction_without_static_replay \
     'EVIDENCE restart-prompt instruction=true request=exact static_blocks=absent'
   bash scripts/run-exact-test.sh cowboy-workflow-agent \
     executor::tests::inherited_restart_session_receives_only_restart_input \
     'EVIDENCE restart-session loaded=true input_once=true role_replayed=false'
   ```

   Expected result: formatting passes; each exact-test invocation proves exactly one fully qualified test exists and reports `1 passed`, `0 failed`, and `0 ignored`; all four evidence markers are present; legacy run data remains compatible; initial/restart kinds and visit ordinals reach execution; and the inherited session receives the exact restart request once without replaying static role/task blocks.

2. Run focused runtime restart integration tests:

   ```bash
   cargo test -p cowboy-workflow-store restart_creation -- --nocapture
   cargo test -p cowboy-workflow-engine restart_run -- --nocapture
   cargo test -p cowboy-workflow-engine inherited_restart_session -- --nocapture
   cargo test -p cowboy-workflow-engine nested_restart -- --nocapture
   ```

   Expected result: each command reports at least one executed test and no failures; completed and failed source runs produce new runs from the same snapshot and config-set pointer, seeding is atomic, concurrent resolution follows the source-lock winner, direct and recursive child runs inherit their separately stored snapshots/sessions, and exact-load failure creates no replacement session.

3. Run focused TUI behavior tests:

   ```bash
   bash scripts/run-exact-test.sh cowboy \
     app::commands::tests::terminal_plain_input_restarts_current_workflow \
     'EVIDENCE tui-restart completed=true failed=true selector_bypassed=true'
   bash scripts/run-exact-test.sh cowboy \
     app::commands::tests::terminal_restart_preserves_submission_priority \
     'EVIDENCE tui-restart priorities=slash,pending_answer,active_agent,restart,new_run'
   bash scripts/run-exact-test.sh cowboy \
     app::commands::tests::terminal_restart_updates_active_run \
     'EVIDENCE tui-restart source_retained=true active_run_switched=true'
   ```

   Expected result: every gate proves exactly one fully qualified test exists, then reports `1 passed`, `0 failed`, and `0 ignored`, and observes its evidence marker. The tests prove completed/failed input takes the restart path, pending answers and active prompts still win, cancelled/unknown states retain normal start behavior, and the TUI follows the new run.

4. Run affected regression and lint gates:

   ```bash
   cargo test -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-agent -p cowboy-workflow-engine -p cowboy
   cargo clippy -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-agent -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings
   git diff --check
   ```

   Expected result: all commands exit with status `0` and no compiler, Clippy, test, or whitespace failures are reported.

5. Run the deterministic two-role restart evidence procedure:

   1. Use the fixture test `runtime::tests::restart_two_role_fixture_emits_source_labeled_evidence`. It must create isolated `parent.lua`, `child.lua`, and `grandchild.lua` files; define `planner` and `implementer` roles with unique role-instruction sentinels; call child and grandchild through `action.workflow`; and use `ScriptedAgentFactory` with successful session loading.
   2. Configure `max_steps_per_run = 16`, `max_visits_per_step = 4`, `max_retries_per_run = 0`, and `max_retries_per_step = 0`. Queue exact valid frontmatter responses for both roles so the backend cannot select the lifecycle branch.
   3. Create the completed source tree with request `mode=complete; change=alpha`. Create the failed source tree with request `mode=fail; change=beta`; the fixture must execute both roles and both nested workflow levels before `action.fail { reason = "fixture requested failure" }`.
   4. Restart those sources with `mode=complete; change=alpha-restarted` and `mode=complete; change=beta-restarted`.
   5. Run:

      ```bash
      rm -rf target/restart-workflow-evidence
      mkdir -p target/restart-workflow-evidence
      COWBOY_RESTART_EVIDENCE_DIR=target/restart-workflow-evidence \
        bash scripts/run-exact-test.sh cowboy-workflow-engine \
          runtime::tests::restart_two_role_fixture_emits_source_labeled_evidence \
          'EVIDENCE restart-fixture artifacts_written=true'
      ```

   6. Inspect the fixture-produced, source-labeled JSON data:
      - `target/restart-workflow-evidence/fixture-config.json`
      - `target/restart-workflow-evidence/completed-source-before.json`
      - `target/restart-workflow-evidence/completed-source-after.json`
      - `target/restart-workflow-evidence/completed-restart-tree.json`
      - `target/restart-workflow-evidence/failed-source-before.json`
      - `target/restart-workflow-evidence/failed-source-after.json`
      - `target/restart-workflow-evidence/failed-restart-tree.json`
      - `target/restart-workflow-evidence/session-inheritance.json`
      - `target/restart-workflow-evidence/agent-prompts.json`
   7. Require every JSON file to declare `schema_version: 1`. The config file must contain the four exact limits and four exact requests from steps 2-4. Each tree file must contain `root_run_id` and chronological `runs`; every run entry must include its label (`root`, `child`, or `grandchild`), ids, restart source id, parent lineage, status/reason, workflow name/hash/sources, config-set name, seed state, and final state. Session evidence must contain source, seeded-target, and final-target records for each `(tree, run label, role)` mapping. Prompt evidence must contain run label, role, session id, exact restart request, prompt text, included block names, and the role/task sentinel strings that must be absent.
   8. Run the fixed repository validator and its mutation tests:

      ```bash
      python3 scripts/validate-restart-workflow-evidence.py \
        target/restart-workflow-evidence
      python3 scripts/tests/test_validate_restart_workflow_evidence.py
      ```

   Expected result: the exact-test gate proves one fixture test executed and prints only the artifact-production marker. The fixed validator independently fails on any missing or extra required artifact, unknown schema, malformed type/lineage, unequal snapshot/config-set/session data, dirty seed counters, missing restart instruction/request, present static sentinel, wrong completed/failed status or reason, or unequal before/after source trees. For the valid bundle it prints `EVIDENCE restart-artifacts snapshots=equal nested_lineage=complete sessions=equal prompts=minimal counters=clean sources=unchanged`; the mutation suite reports all negative cases passed.

# TODO

- [x] TODO-01: Add backward-compatible workflow-restart lineage and restart user-input semantics to the core model.
  - Procedure:
    1. Add optional restart lineage to `Run` with serde defaults that preserve legacy data.
    2. Add the restart input origin, update ordered sequence-zero input construction, and carry the initial-input kind through `ExecutionContext` and every runner/action construction site.
    3. Add `state::tests::restart_lineage_deserializes_and_orders_restart_input` and `engine::tests::restart_execution_context_preserves_input_kind_and_visit`, make them print the TODO-01 markers from `How to verify`, and run both through the existing reviewed `scripts/run-exact-test.sh`.
  - Expected result: both gates prove exactly one test was discovered and report `1 passed`, `0 failed`, and `0 ignored`; legacy runs deserialize unchanged, ordinary runs synthesize and dispatch `initial` sequence zero, and restarted runs synthesize and dispatch `restart` sequence zero with exact request content, source-run lineage, and stable visit ordinal across retries.
  - Implementer observed result: `cargo fmt --all -- --check` and the executable-helper check passed. Both exact tests were discovered once and each reported exactly `1 passed`, `0 failed`, and `0 ignored` with the required markers. The tests observed legacy deserialization, ordinary `initial` input, restart lineage and exact `restart` input, and a stable visit ordinal through retry dispatch.

- [x] TODO-02: Implement runtime restart creation from the terminal run's exact workflow snapshot and inherited role sessions.
  - Procedure:
    1. Add a runtime restart API that accepts a source run id and new request.
    2. Acquire the source-run guard, compile its stored snapshot, and invoke one SQLite transaction that re-loads and re-validates the source status/hash before atomically inserting the target run, run head, and inherited sessions; inject failures after run/head insertion and during session insertion.
    3. Iterate compiled role ids, copy available role sessions with identical session ids and static delivery state, reset only the input watermark, and set `PROVIDED_SESSION_BACKEND` to require exact session loading.
    4. Carry the current step visit ordinal into workflow-action dispatch, traverse the source parent's chronological records to find the corresponding workflow invocation, and restart matching child/grandchild runs from their own snapshots, config-set pointers, and separately keyed sessions; use the current catalog only when no source invocation corresponds to the new branch.
    5. Execute each root or child only after its complete seed commits, and add completed, failed, snapshot-stability, invalid-status, multi-role, load-failure, direct-child, recursive-grandchild, repeated-visit, divergent-branch, lineage-corruption, concurrent-resolution, and rollback tests.
    6. Run `cargo test -p cowboy-workflow-store restart_creation -- --nocapture`, `cargo test -p cowboy-workflow-engine restart_run -- --nocapture`, `cargo test -p cowboy-workflow-engine inherited_restart_session -- --nocapture`, and `cargo test -p cowboy-workflow-engine nested_restart -- --nocapture`; require every command to report at least one executed test and zero failures.
  - Expected result: eligible source runs create distinct fresh runs from the same stored workflow version and config-set pointer; root and matched descendants inherit their own snapshots and role sessions under new ids; source-lock ordering makes concurrent resolution deterministic; persistence failure leaves no partial target rows or events; roles without prior sessions remain fresh; source runs remain unchanged; and unavailable inherited sessions fail without replacement.
  - Implementer observed result: all four focused store/runtime commands executed nonzero tests with zero failures. The observed tests covered atomic run/head/session seeding and rollback with pool reuse, completed/failed restart reset and snapshot reuse, exact inherited-session loading without static replay, and recursive child/grandchild snapshot and session inheritance. The combined affected-crate gate initially exposed second-runtime startup repeatedly setting WAL mode while another runtime had an active pool; after making WAL establishment return immediately when the database is already in WAL mode, the unchanged contention repro and the full combined gate passed while preserving the immediate active-run error.

- [x] TODO-03: Render restart-specific agent instructions while preserving minimal reused-session prompt delivery.
  - Procedure:
    1. Update agent execution to derive sequence-zero input from the `ExecutionContext` initial-input kind.
    2. Update prompt assembly to recognize an unseen restart sequence-zero input and introduce it with the restart instruction without adding prior-run inputs.
    3. Preserve existing role, task-contract, deliverable, retry, and input-watermark selection rules.
    4. Add `prompt::tests::restart_input_prepends_instruction_without_static_replay` and `executor::tests::inherited_restart_session_receives_only_restart_input`, make them print the TODO-03 markers from `How to verify`, and run both through the existing reviewed `scripts/run-exact-test.sh`.
  - Expected result: both gates prove exactly one test was discovered and report `1 passed`, `0 failed`, and `0 ignored`; each reused role session receives the restart instruction and exact new request once, while role definitions and already-delivered task contracts remain absent.
  - Implementer observed result: both exact agent tests were discovered once and each reported exactly `1 passed`, `0 failed`, and `0 ignored` with the required markers. Prompt and executor observations showed the restart instruction immediately preceding the exact raw request, one-time delivery to the loaded inherited session, and omission of role instructions and previously delivered task contracts.

- [x] TODO-04: Route completed and failed TUI plain-text submissions through the runtime restart path.
  - Procedure:
    1. Add an `AppState` terminal restart target for idle `Completed` and `Failed` active runs.
    2. Insert restart dispatch after slash and pending-answer handling but before normal new-run dispatch.
    3. Add restart-specific task/card copy and preserve existing composer clearing, draft retention, and history rules.
    4. Add state, command, input, and composer tests covering terminal, cancelled, unknown, pending-answer, active-agent, and slash-command cases.
    5. Use the existing reviewed `scripts/run-exact-test.sh` unchanged. Make the three fully qualified `app::commands::tests::*` tests print the source-labeled markers from `How to verify`, then run them through the helper; require it to fail on command errors, zero or multiple exact matches, failed or ignored tests, a summary other than exactly one pass, or a missing marker.
  - Expected result: each gate proves one test was discovered and reports `1 passed`, `0 failed`, and `0 ignored`; completed/failed plain text restarts the visible workflow, all higher-priority input paths remain unchanged, excluded states still start normally, and the active TUI state follows the new run id.
  - Implementer observed result: all three exact TUI tests were discovered once and each reported exactly `1 passed`, `0 failed`, and `0 ignored` with the required source-labeled marker. The tests observed completed and failed restart routing without selector use, priority order `slash,pending_answer,active_agent,restart,new_run`, source-run retention, active-run switching, excluded-state normal starts, and restart-specific composer copy.

- [x] TODO-05: Document and validate the terminal restart contract across runtime and TUI surfaces.
  - Procedure:
    1. Update `README.md`, `docs/architecture.md`, and `docs/workflow-authoring.md` with snapshot, lineage, session, prompt, status, and failure semantics.
    2. Add the fixed `scripts/validate-restart-workflow-evidence.py` validator and `scripts/tests/test_validate_restart_workflow_evidence.py` mutation suite; the fixture must not generate or modify either file.
    3. Run the affected crate tests, validator mutation suite, Clippy command, and `git diff --check` from `How to verify`.
    4. Run `runtime::tests::restart_two_role_fixture_emits_source_labeled_evidence` exactly as specified in `How to verify`, including its parent/child/grandchild fixture, exact limits/responses/requests, fixed evidence directory, nine JSON artifacts, artifact-production marker, schema inspection, and fixed-validator command.
  - Expected result: documentation matches the implemented contract; all automated gates execute nonzero tests and pass; the independently reviewed validator rejects every malformed evidence class and accepts the valid bundle only when it proves completed and failed setup, new root/child/grandchild ids, equal snapshots/config sets, separately keyed but equal inherited sessions, exact restart prompts, omitted static prompts/contracts, clean seed counters, valid lineage, and unchanged source trees.
  - Implementer observed result: README and architecture/authoring documentation describe terminal restart eligibility, immutable source/new target lineage, exact snapshot and config-set reuse, recursive separately keyed session inheritance, restart input/prompt semantics, and exact-load failure. All affected crate tests, Clippy with `-D warnings`, formatting, and `git diff --check` passed. The exact fixture test produced the required marker and nine schema-version-1 JSON artifacts. The fixed validator printed `EVIDENCE restart-artifacts snapshots=equal nested_lineage=complete sessions=equal prompts=minimal counters=clean sources=unchanged`, and its mutation suite reported all 11 negative cases passed.
