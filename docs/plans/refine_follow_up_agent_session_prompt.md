# Refine follow-up agent session prompt

## Plan

### Problem

Every agent step dispatch rebuilds the full prompt through
`crates/workflow/agent/src/prompt.rs::build_agent_prompt` (lines 9-27) and sends
it to the backend, regardless of whether the backend session is brand-new or
reused. The built prompt always contains:

1. `## Role` — the role's static persona/instructions (`role.instructions`)
2. `## Task` — the current step's action prompt
3. `## User Inputs` — the **cumulative** ordered user-input history
4. `## Deliverable Format` — allowed statuses, fields, and YAML-frontmatter rules,
   emitted only when `action.output` is `Some` (today's behavior)

Agent sessions are reused per `(run_id, role_id)` (`RoleSessionKey` and
`AgentExecutor.clients` in `executor.rs`), so when the same role runs a later
step (e.g. `planner` re-runs `plan` after `changes_requested`, or `implementer`
runs `implement` then `revise`), the identical multi-thousand-character
`## Role` block and the already-sent user inputs are re-transmitted into a
session that already holds them. This wastes tokens and context.

The `## Role` block is static and identical on every dispatch to the same
session; once the session has seen it, it never needs to be resent. The
`## User Inputs` history is cumulative and append-only, so a follow-up prompt
only needs the **new** inputs the session has not seen. `## Task` and
`## Deliverable Format` must still be sent on every dispatch because the task
differs per step and the format is a per-action contract.

### Approach

Track, per reused backend session, what has already been sent, and compose a
minimal follow-up prompt:

- Persist a per-session watermark on the durable `RoleSession` record:
  - `role_instructions_sent: bool` — whether `## Role` was sent to this session.
  - `last_sent_input_sequence: Option<u64>` — highest user-input `sequence`
    already delivered to this session (`None` = nothing sent yet).
- A brand-new backend session (first dispatch, or a load-failure fallback that
  creates a new backend session) resets the watermark to `false`/`None`, so the
  full `## Role` block and the full user-input history are sent. A reused session
  (in-memory active, or successfully loaded from the store) keeps its watermark,
  so `## Role` is omitted and only user inputs with `sequence >
  last_sent_input_sequence` are sent.
- The watermark is advanced immediately after the prompt window seals (all prompt
  turns succeeded), **before** frontmatter parsing/validation. This makes a
  parse-failure retry (`attempt > 1`) on the same live in-process session omit
  `## Role` and already-sent inputs, because the retry reuses the same session
  whose watermark was advanced before the earlier parse failed.

The decision is keyed on session identity (via `RoleSession` reset on
`new_session`), not on the workflow step or attempt number, so it stays correct
across step transitions, retries, and process restarts with session reload.

### Deliverable-contract decision (resolves the `always-emit` ambiguity)

`## Deliverable Format` is emitted **iff `action.output` is `Some`**, on **every**
dispatch (first and follow-up) — unchanged from today. When `action.output` is
`None`, no deliverable-format section is emitted on any dispatch, also unchanged.
`## Task` is emitted on every dispatch unconditionally. Only `## Role` (gated on
the watermark) and `## User Inputs` (delta-filtered / omitted when empty) change
between first and follow-up prompts. TODO-02's checkbox retains its original
refactor wording (which names `## Deliverable Format`); a subordinate bullet
under TODO-02 delegates the exact `action.output`-gated presence rule and its
`action.output == None` test to TODO-14, so the two TODOs do not duplicate the
same assertion subject.

### Why this is safe

- `## Task` remains on every dispatch and `## Deliverable Format` retains its
  exact current presence rule, so each step's instruction and its output contract
  are always present — no change to what the agent is asked to produce or how it
  must format results.
- The correction/prompt-window loop (`build_correction_prompt`, lines 519-614)
  already sends only new follow-up prompts plus the deliverable format and never
  included `## Role`; it is unchanged. Its final `applied_sequence` is exactly the
  value used to advance the watermark, keeping both paths consistent.
- `RoleSession` is stored as a whole serialized JSON value in redb (no dedicated
  columns), so adding `#[serde(default)]` fields needs no table migration and old
  rows deserialize with `false`/`None`. This is proven by an explicit
  deserialization test (TODO-10), not asserted.
- On a fresh backend session created because a persisted session failed to load,
  `new_session` overwrites `RoleSession` with a reset watermark, so `## Role` is
  correctly resent to the new session that lacks the history (TODO-12).

## Changes

### `crates/workflow/core/src/state.rs`

- Extend `RoleSession` (lines 486-499) with:
  - `pub role_instructions_sent: bool` with `#[serde(default)]`.
  - `pub last_sent_input_sequence: Option<u64>` with
    `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- Update the struct doc comment to describe the per-session prompt watermark.

### `crates/workflow/agent/src/prompt.rs`

- Change `build_agent_prompt` (lines 9-27) to accept `include_role: bool`.
  Behavior:
  - Push `## Role` only when `include_role` is true and instructions are
    non-empty.
  - Always push `## Task`.
  - Push `## Deliverable Format` only when `action.output` is `Some` (unchanged).
  - Push `## User Inputs` only when the supplied `user_inputs` slice is
    non-empty. When `include_role` is true (first prompt / full history) keep the
    existing cumulative header wording ("All entries below are cumulative user
    direction. Apply them in sequence."); when `include_role` is false (follow-up
    delta) use header wording appropriate for new-only direction ("New user
    direction not yet sent in this session. Apply in sequence."). The JSON body
    shape is identical in both cases.
- Keep `build_correction_prompt` and `build_retry_nudge` unchanged.
- Update the two in-file `build_agent_prompt` test call sites (lines 193, 220) to
  pass `include_role = true`.

### `crates/workflow/agent/src/executor.rs`

- Change `ensure_session` (lines 659-747) to report whether a **new** backend
  session was created this dispatch. Introduce a return type, e.g.
  `struct AcquiredSession { session_id: String, fresh: bool }`, with `fresh =
  true` only on the `new_session` path (both the initial-create case and the
  load-failure fallback). The `RoleSession` written on `new_session` sets
  `role_instructions_sent: false`, `last_sent_input_sequence: None`.
- In `execute_agent` (lines 366-657):
  - After `ensure_session`, resolve the watermark: if `fresh`, use
    `role_instructions_sent = false`, `last_sent_input_sequence = None`
    (ignoring any stale stored row); otherwise load it via
    `self.store.load_role_session(&key.run_id, &key.role_id)`, defaulting to
    `false`/`None` when absent.
  - `include_role = !role_instructions_sent`.
  - Compute delta inputs: filter `user_inputs` to entries whose `sequence` is
    greater than `last_sent_input_sequence` (all entries when `None`).
  - Build `base_prompt` via `build_agent_prompt(role, &action, &delta_inputs,
    include_role)`. Keep the `attempt > 1` retry-nudge append unchanged.
  - After the seal loop breaks on `CompareAndSealPromptWindowOutcome::Sealed`
    and **before** `parse_frontmatter_output` (line 616), advance and persist the
    watermark: set `role_instructions_sent = true`, `last_sent_input_sequence =
    Some(max(prior.unwrap_or(0)-aware, final applied_sequence))` — concretely
    `Some(applied_sequence.max(prior_last_sent))` when a prior exists else
    `Some(applied_sequence)` — then `save_role_session` preserving `backend` and
    `session_id`, refreshing `updated_at`.
- Update the `RoleSession { .. }` literal in `ensure_session` (line 731) and the
  test literal (line 2341) to include the new fields.

### `crates/workflow/store/src/redb_store.rs`

- Update the `RoleSession { .. }` test literals (lines 919, 940) to include the
  new fields. No table or key change; `RoleSession` is serialized whole.

### Other `RoleSession` construction sites

- Only `save_role_session`/`load_role_session` pass-throughs exist in fake stores
  and test apps (`crates/workflow/core/src/engine.rs`,
  `crates/workflow/engine/src/runner.rs`,
  `crates/workflow/agent/src/bin/execute-agent.rs`,
  `crates/workflow/store/src/bin/store-cli.rs`); they construct no `RoleSession`
  literal, so serde defaults cover them. Add fields only where a literal is
  constructed (enumerated in TODO-06).

## Tests to be added/updated

### `crates/workflow/core/src/state.rs` (unit)

- Legacy deserialization: deserialize a `RoleSession` JSON value that lacks both
  watermark fields; assert `role_instructions_sent == false` and
  `last_sent_input_sequence == None` (TODO-10).

### `crates/workflow/agent/src/prompt.rs` (unit)

- Update the two existing tests to pass `include_role = true`; assert role text
  still appears.
- `include_role = false` omits the `## Role`/instructions text but retains
  `## Task` and, when `action.output` is `Some`, `## Deliverable Format`
  (TODO-02).
- `action.output == None`: assert the exact invariant that `## Task` is present
  and `## Deliverable Format` is absent, for both `include_role` values,
  regardless of any other permitted section (TODO-14).
- Empty `user_inputs` slice omits `## User Inputs`; a non-empty follow-up delta
  emits only the given sequences with the follow-up header wording (TODO-02).

### `crates/workflow/agent/src/executor.rs` (async unit)

- In-memory reuse: two dispatches for the same `(run, role)` reusing one session;
  first prompt contains `Instructions for developer`, second omits it, both
  contain `Do work` (TODO-07).
- Delta inputs: with follow-up user prompts appended between dispatches, the
  second prompt's `## User Inputs` contains only the new sequences and omits the
  initial request sent first (TODO-07).
- Watermark advance: after one dispatch the persisted `RoleSession` has
  `role_instructions_sent == true` and `last_sent_input_sequence == Some(..)`
  (TODO-07).
- Persisted-session reload: using the cloneable `Arc`-backed shared fake store
  added in TODO-15, dispatch once, build a **new** executor over a clone of that
  shared store and a fresh `FakeClient::with_load` whose `load_session` succeeds,
  dispatch again, and assert the prompt omits role instructions and contains only
  unseen input sequences (TODO-11).
- Load-failure fallback: using the configurable `load_session` failure added in
  TODO-15, seed a persisted watermark, make the client's `load_session` return
  `Err` so a new backend session is created, and assert role instructions and the
  complete applicable input history are resent (TODO-12).
- Actual retry path: seed a `FakeClient` event queue whose first reply is
  malformed frontmatter (parse fails → `execute_agent` returns `Err`) and second
  reply is valid; dispatch attempt 1 (expect error), then dispatch attempt 2 on
  the same executor/session. Assert the second prompt omits role and already-sent
  inputs, retains task and deliverable contract, includes the retry nudge, and
  that the stored watermark was advanced by the first (failed) dispatch before
  its parse failed (TODO-13).
- Keep `reuses_same_client_for_same_run_and_role`,
  `uses_different_clients_for_different_roles`,
  `loads_persisted_role_session`, `retry_attempt_appends_corrective_frontmatter_nudge`,
  and `retry_prompt_selects_no_result_branch_for_no_result_reason` passing.

### `crates/workflow/store/src/redb_store.rs` (unit)

- Extend `persists_role_sessions_by_run_and_role` to set non-default watermark
  values and assert they round-trip through redb (TODO-08).

## How to verify

1. Build the workspace:
   - `cargo build`
   - Expected: clean build, no errors.
2. Run the changed-crate test suites:
   - `cargo test -p cowboy-workflow-core -p cowboy-workflow-agent -p cowboy-workflow-store`
   - Expected: all tests pass, including the new prompt-composition,
     session-watermark, reload, fallback, retry, and legacy-deserialization tests.
3. Compile all targets across the workspace (catches every test literal):
   - `cargo check --workspace --all-targets`
   - Expected: clean, no missing-field errors on any `RoleSession` literal.
4. Lint the changed crates:
   - `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-agent -p cowboy-workflow-store --all-targets`
   - Expected: no new warnings.

All behavioral verification is performed by deterministic in-process `FakeClient`
tests that inspect captured prompts in memory (`FakeClient.prompt_calls`); no
real backend, credentials, fixtures, or prompt-content logging are used, so no
sensitive data is emitted or persisted. The reload and load-failure tests require
new test-only scaffolding (a cloneable shared store and a configurable
`load_session` error) that does not exist today and is added in TODO-15.

## TODO

- [x] TODO-01: Add `role_instructions_sent: bool` and `last_sent_input_sequence: Option<u64>` (with serde defaults) to `RoleSession` in `crates/workflow/core/src/state.rs` and update its doc comment.
  - Procedure: edit `crates/workflow/core/src/state.rs`; run `cargo test -p cowboy-workflow-core state::` (compiles the crate and runs the legacy-deserialization test from TODO-10 which lives in this module).
  - Expected result: `cowboy-workflow-core` compiles and the `RoleSession` legacy-deserialization test passes, proving the two fields exist with working serde defaults.
  - Dependency: TODO-10 adds the legacy-deserialization test in this module; land TODO-10 before running this procedure so the filtered test exists.
  - Observed result: `cargo test -p cowboy-workflow-core state::` compiled `cowboy-workflow-core` and passed the module suite (10 passed; 26 filtered out), including `state::tests::role_session_deserializes_legacy_rows_with_default_watermark`. Fields added with `#[serde(default)]` / `#[serde(default, skip_serializing_if = "Option::is_none")]` and the doc comment now describes the per-session prompt watermark. Matches expected.

- [x] TODO-02: Refactor `build_agent_prompt` in `crates/workflow/agent/src/prompt.rs` to take an `include_role` flag, omit `## Role` when false, omit `## User Inputs` when the input slice is empty, use follow-up-appropriate header wording for delta inputs, and always emit `## Task` and `## Deliverable Format`; update the two in-file test call sites.
  - Scope: TODO-02 owns the refactor and the role/user-input composition tests. The exact `action.output`-gated presence of `## Deliverable Format` and its `action.output == None` absence test are owned by TODO-14; this TODO's tests only assert the deliverable section is present when `action.output` is `Some`.
  - Procedure: edit `prompt.rs`; run `cargo test -p cowboy-workflow-agent prompt::`.
  - Expected result: prompt unit tests pass, including new assertions that `include_role = false` omits role text but keeps task text; that an empty input slice omits `## User Inputs`; that a follow-up delta emits only the supplied sequences with follow-up header wording; and that with `action.output == Some(..)` both the first (`include_role = true`) and follow-up (`include_role = false`) prompt assertions contain `## Deliverable Format`.
  - Observed result: `cargo test -p cowboy-workflow-agent prompt::` passed all prompt tests including `follow_up_prompt_omits_role_but_keeps_task_and_deliverable`, `empty_inputs_omit_user_inputs_and_delta_uses_follow_up_header`, and `full_history_uses_cumulative_header`. Both existing call sites updated to `include_role = true`. Matches expected.

- [x] TODO-03: Change `ensure_session` in `crates/workflow/agent/src/executor.rs` to report whether the acquired session is fresh (new backend session) vs reused, and set the reset watermark (`role_instructions_sent: false`, `last_sent_input_sequence: None`) when writing `RoleSession` on `new_session`.
  - Procedure: edit `executor.rs` so `ensure_session` returns the fresh/reused signal; add a same-module test `ensure_session_fresh_then_reused` that observes the transient reset directly (not through a completed `execute_agent`): (1) call `ensure_session` on a fresh active `FakeClient`; assert `fresh == true` and the stored `RoleSession` is `role_instructions_sent == false`/`last_sent_input_sequence == None`; (2) overwrite the stored watermark with `true`/`Some(n)`; (3) call `ensure_session` again on the same active client; assert `fresh == false` and the stored `true`/`Some(n)` is unchanged. Run `cargo test -p cowboy-workflow-agent ensure_session_fresh_then_reused`.
  - Expected result: the test passes, directly observing (a) `fresh == true` with a reset watermark write on new-session creation and (b) `fresh == false` with the pre-seeded `true`/`Some(n)` watermark left unchanged on reuse; loaded-session→reused and load-fallback→fresh are additionally covered by TODO-11 and TODO-12.
  - Observed result: `ensure_session` now returns `AcquiredSession { session_id, fresh }`. `cargo test -p cowboy-workflow-agent ensure_session_fresh_then_reused` passed, directly observing `fresh == true` with a reset watermark write on new-session creation and `fresh == false` leaving the pre-seeded `true`/`Some(3)` watermark unchanged on reuse. Matches expected.

- [x] TODO-04: In `execute_agent`, load the `RoleSession` watermark (treating a fresh session as unset), compute `include_role` and the delta user inputs (`sequence > last_sent_input_sequence`), and build the prompt via the new `build_agent_prompt` signature, preserving the existing `attempt > 1` retry-nudge append.
  - Procedure: edit `executor.rs`; run `cargo test -p cowboy-workflow-agent` for the in-memory reuse and delta-inputs tests added in TODO-07.
  - Expected result: the reused-session dispatch prompt omits `## Role` and already-sent inputs while the fresh dispatch prompt contains full role and history; both assertions pass.
  - Observed result: `cargo test -p cowboy-workflow-agent` passed `reused_session_omits_role_and_advances_watermark` (first prompt contains role, second omits it; both keep task + deliverable) and `follow_up_dispatch_sends_only_new_input_sequences` (second prompt carries only `sequence: 1`, omits `sequence: 0`/`Original request`, uses the follow-up header). Retry-nudge append preserved. Matches expected.

- [x] TODO-05: After the prompt-window seal loop and before frontmatter parsing, advance and persist the watermark (`role_instructions_sent = true`, `last_sent_input_sequence = Some(max(prior, final applied_sequence))`) via `save_role_session`, preserving `backend`/`session_id` and refreshing `updated_at`.
  - Procedure: edit `executor.rs`; extend the TODO-07 watermark-advance test so it seeds a reusable `RoleSession` with a known `backend`, `session_id`, watermark, and an intentionally old `updated_at`, dispatches successfully through the loaded/reused session, then loads the resulting `RoleSession`; also run the retry-path test (TODO-13). Run `cargo test -p cowboy-workflow-agent`.
  - Expected result: after a dispatch the loaded `RoleSession` shows the watermark advanced (`role_instructions_sent == true`, `last_sent_input_sequence == Some(..)`), `backend` equal to the seeded value, `session_id` equal to the seeded value, and `updated_at` strictly later than the seeded timestamp; and the retry test confirms the watermark was advanced by the first (parse-failing) dispatch.
  - Observed result: `cargo test -p cowboy-workflow-agent` passed `watermark_advance_preserves_backend_and_session_id` (loaded `RoleSession` shows `role_instructions_sent == true`, `last_sent_input_sequence == Some(0)`, `backend == "seeded-backend"`, `session_id == "seeded-session"`, `updated_at` strictly later than the seeded timestamp) and `retry_reuses_session` (watermark advanced by the first parse-failing dispatch). Matches expected.

- [x] TODO-06: Update all constructed `RoleSession` literals to include the new fields: `ensure_session` (executor.rs), the executor test literal, and the `redb_store.rs` test literals.
  - Procedure: use the repository search tool (built-in `grep`) for the pattern `RoleSession \{` to enumerate literal construction sites; edit each; run `cargo check --workspace --all-targets`.
  - Expected result: `cargo check --workspace --all-targets` completes with no missing-field errors on any `RoleSession` literal across the workspace, including all test targets.
  - Observed result: Step 1 — the built-in `grep` search for `RoleSession \{` enumerated every literal construction site: two `save_role_session(RoleSession { .. })` lib sites in `executor.rs` (`ensure_session` new-session write and the post-seal watermark-advance write), five `executor.rs` test literals, and two `redb_store.rs` test literals (plus the `state.rs` struct definition); each was updated to include the two new fields. Step 2 — `cargo check --workspace --all-targets` completed clean with no missing-field errors on any `RoleSession` literal across the workspace. Matches expected.

- [x] TODO-07: Add executor async unit tests: (a) two same-role dispatches reuse a session with the second prompt omitting role instructions but keeping task and deliverable format; (b) the second dispatch's user-inputs section contains only new follow-up sequences; (c) the persisted `RoleSession` watermark advances after a dispatch.
  - Procedure: add tests in `executor.rs`; run `cargo test -p cowboy-workflow-agent`.
  - Expected result: the three new behavioral tests pass and all pre-existing executor tests still pass.
  - Observed result: `cargo test -p cowboy-workflow-agent` passed the three behavioral tests (`reused_session_omits_role_and_advances_watermark`, `follow_up_dispatch_sends_only_new_input_sequences`, `watermark_advance_preserves_backend_and_session_id`) and all pre-existing executor tests (73 passed total in the agent lib). Matches expected.

- [x] TODO-08: Extend the `redb_store.rs` `persists_role_sessions_by_run_and_role` test to set and assert round-trip of non-default watermark values, and add/adjust `prompt.rs` tests for the empty vs delta `## User Inputs` behavior.
  - Procedure: edit `redb_store.rs` (extend `persists_role_sessions_by_run_and_role` with non-default watermark values) and `prompt.rs` (empty vs delta `## User Inputs` assertions); run both subjects:
    - `cargo test -p cowboy-workflow-store persists_role_sessions_by_run_and_role`
    - `cargo test -p cowboy-workflow-agent prompt::`
  - Expected result: the redb round-trip test preserves non-default watermark values (`role_instructions_sent == true` and `last_sent_input_sequence == Some(sequence)`) through save/load; and the prompt tests confirm an empty input slice omits `## User Inputs` while a delta emits only the supplied sequences with follow-up header wording.
  - Observed result: `cargo test -p cowboy-workflow-store persists_role_sessions_by_run_and_role` passed (1 passed) round-tripping `role_instructions_sent == true` / `last_sent_input_sequence == Some(4)` through redb; `cargo test -p cowboy-workflow-agent prompt::` passed the empty-vs-delta `## User Inputs` assertions. Matches expected.

- [x] TODO-09: Run full verification for the changed crates and fix all compiler and Clippy warnings.
  - Procedure: `cargo test -p cowboy-workflow-core -p cowboy-workflow-agent -p cowboy-workflow-store`; then `cargo check --workspace --all-targets`; then `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-agent -p cowboy-workflow-store --all-targets`.
  - Expected result: all tests pass, the workspace compiles all targets, and clippy reports no new warnings.
  - Observed result: `cargo test -p cowboy-workflow-core -p cowboy-workflow-agent -p cowboy-workflow-store` all passed (core 36, agent 73, store 22, plus bins); `cargo check --workspace --all-targets` clean; `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-agent -p cowboy-workflow-store --all-targets` reported no warnings. Matches expected.

- [x] TODO-10: Add a `RoleSession` legacy-deserialization test in `crates/workflow/core/src/state.rs` proving backward compatibility with the no-migration claim.
  - Procedure: add a `#[test]` that calls `serde_json::from_value`/`from_str` on a `RoleSession` JSON object containing only the pre-change fields (`run_id`, `role_id`, `backend`, `session_id`, `updated_at`) and asserts the two new fields default; run `cargo test -p cowboy-workflow-core role_session`.
  - Expected result: deserialization succeeds and asserts `role_instructions_sent == false` and `last_sent_input_sequence == None`.
  - Observed result: `cargo test -p cowboy-workflow-core role_session` passed `role_session_deserializes_legacy_rows_with_default_watermark`; deserializing a `RoleSession` JSON with only pre-change fields yields `role_instructions_sent == false` and `last_sent_input_sequence == None`. Matches expected.

- [x] TODO-11: Add an executor test for persisted-session reload across executor instances.
  - Procedure: using TODO-15's cloneable `Arc`-backed shared fake store, dispatch once; build a second `AgentExecutor::new` over a clone of that shared store and a fresh `FakeClient::with_load` whose `load_session` succeeds; dispatch again for the same `(run, role)`; capture prompts; run `cargo test -p cowboy-workflow-agent persisted_session_reload_omits_role`.
  - Expected result: the second executor's prompt omits role instructions and contains only user-input sequences greater than the persisted `last_sent_input_sequence`; assertions pass.
  - Observed result: `cargo test -p cowboy-workflow-agent persisted_session_reload_omits_role` passed. A second `AgentExecutor` over a clone of the shared store and a `FakeClient::with_load` produced a prompt omitting role instructions and carrying only `sequence: 1` (omitting `sequence: 0`). Matches expected.

- [x] TODO-12: Add an executor test for the load-failure fallback resend.
  - Procedure: using TODO-15's shared store that records the ordered sequence of `save_role_session` calls and its configurable `load_session` failure, seed the store with a `RoleSession` whose watermark is set (`role_instructions_sent == true`, `last_sent_input_sequence == Some(n)`); **capture the recorded save-history length after seeding** (or clear the recorded history) so only dispatch-time writes are asserted; build a client that reports `supports_load_session() == true` but whose injected `load_session` returns `Err`, forcing `new_session`; run one successful dispatch; capture the prompt and the save-history **suffix produced by the dispatch**; run `cargo test -p cowboy-workflow-agent load_failure_resends_role`.
  - Expected result: the prompt re-includes the `## Role` block and the complete applicable user-input history, and the dispatch-time save-history suffix for this `(run, role)` is exactly `[role_instructions_sent == false / last_sent_input_sequence == None, then role_instructions_sent == true / last_sent_input_sequence == Some(..)]`, proving the fallback first persisted the reset and then re-advanced it (the seeded pre-dispatch write is excluded by the captured-length slice).
  - Observed result: `cargo test -p cowboy-workflow-agent load_failure_resends_role` passed. With a seeded watermark and an injected `load_session` error, the dispatch prompt re-includes `Instructions for developer` and the full history (`sequence: 0` and `sequence: 1`); the dispatch-time save-history suffix was exactly `[(false, None), (true, Some(1))]`. Matches expected.

- [x] TODO-13: Add an executor test for the actual parse-failure retry path on a reused live session.
  - Procedure: build a `FakeClient` whose event queue yields a malformed-frontmatter reply first and a valid reply second; dispatch attempt 1 (expect `Err` from `execute_agent`); dispatch attempt 2 (`context.attempt = 2`, `retry_reason = Some(..)`) on the same executor; capture both prompts; run `cargo test -p cowboy-workflow-agent retry_reuses_session`.
  - Expected result: attempt 1 returns an error; the attempt-2 prompt omits `## Role` and already-sent inputs, retains `## Task` and the deliverable contract, includes the `## Retry` nudge, and the persisted watermark shows `role_instructions_sent == true` set by the first (failed) dispatch before its parse failure.
  - Observed result: `cargo test -p cowboy-workflow-agent retry_reuses_session` passed. Attempt 1 returned `Error::NoWorkflowResult`; the attempt-2 prompt omits role and already-sent inputs, retains task + deliverable contract, includes the retry nudge; the persisted watermark showed `role_instructions_sent == true` set by the first failed dispatch. Matches expected.

- [x] TODO-14: Encode the deliverable-contract-without-`OutputSpec` decision (emit `## Deliverable Format` iff `action.output` is `Some`, unchanged; `## Task` always emitted) consistently across `build_agent_prompt` and its tests.
  - Procedure: ensure `build_agent_prompt` gates the deliverable section on `action.output.is_some()`; add a `prompt.rs` test asserting the exact invariant for `action.output == None` (see Tests section); run `cargo test -p cowboy-workflow-agent prompt::`.
  - Expected result: the test passes, asserting that when `action.output == None` the prompt contains `## Task` and contains no `## Deliverable Format` section, for both `include_role` true and false, regardless of any other permitted section present.
  - Observed result: `cargo test -p cowboy-workflow-agent prompt::` passed `no_output_spec_omits_deliverable_format_for_both_role_flags`, asserting that with `action.output == None` the prompt contains `## Task` and no `## Deliverable Format` for both `include_role` values. `build_agent_prompt` gates the deliverable section on `action.output`. Matches expected.

- [x] TODO-15: Add test-only executor scaffolding shared by TODO-11 and TODO-12: a cloneable `Arc`-backed shared fake store whose persisted `RoleSession`/prompt state survives across two `AgentExecutor::new` instances (which consume their store), and a configurable `load_session` failure on the fake client (e.g. a `load_session_error: Option<String>` field or dedicated constructor) so `ensure_session` exercises the load-failure fallback.
  - Procedure: edit the `#[cfg(test)] mod tests` in `crates/workflow/agent/src/executor.rs` to add (a) an `Arc`-backed shared-store wrapper that delegates `RunStore` to shared inner state and records the ordered sequence of `save_role_session` calls, and (b) the configurable load-error field/constructor on `FakeClient`; add two focused helper tests — `shared_store_clone_observes_same_session` (save through one clone, load through a second clone, assert the loaded `RoleSession` equals the saved one) and `fake_client_load_error_is_returned` (assert a configured `load_session` error is surfaced by the client). Run each focused proof separately:
    - `cargo test -p cowboy-workflow-agent shared_store_clone_observes_same_session`
    - `cargo test -p cowboy-workflow-agent fake_client_load_error_is_returned`
    - `cargo test -p cowboy-workflow-agent persisted_session_reload_omits_role`
    - `cargo test -p cowboy-workflow-agent load_failure_resends_role`
  - Expected result: `shared_store_clone_observes_same_session` proves cloned stores observe the same persisted session; `fake_client_load_error_is_returned` proves the injected load error reaches callers; and the two consumer tests (TODO-11, TODO-12) pass, confirming the scaffolding supports cross-executor shared persistence and the load-failure fallback path.
  - Observed result: Added `SharedFakeStore` (cloneable `Arc<FakeStore>` wrapper delegating `RunStore`; `FakeStore` records ordered `save_role_session` calls in `save_history`) and `FakeClient::with_load_error`. `cargo test -p cowboy-workflow-agent shared_store_clone_observes_same_session` and `fake_client_load_error_is_returned` passed, and consumer tests `persisted_session_reload_omits_role` / `load_failure_resends_role` passed. Matches expected.
