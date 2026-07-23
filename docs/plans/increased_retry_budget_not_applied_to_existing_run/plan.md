# Plan: Resolve config-set limits live from config instead of snapshotting them into the run

Bug-fix plan for the reported symptom:

```
error: invalid action: config set "default" exhausted retry budget
for step "plan": 2/2 retries used; last recoverable error: recoverable
action failure: agent reply did not contain a workflow result
```

The user raised `max_retries_per_step` from `2` to `20` in `config.toml`, but a
run that already failed keeps re-failing at the stale `2/2`.

- RCA: [`rca.md`](./rca.md)
- Repro test (investigator-added; keep byte-for-byte unchanged as the fix's
  input): `crates/workflow/engine/src/runtime.rs::runtime::tests::resume_applies_increased_step_retry_budget_to_existing_failed_run`

## Revision note (architecture change per user direction)

Cumulative user direction:

1. *"we don't store the config in the snapshot, snapshot only stores the
   'pointer' to the agent, the config will be resolved every time cowboy is
   restarted."* → The run persists only the **config-set name**; effective
   `RunnerLimits` are **resolved from live config on every operation**.
2. *"we can make breaking change so we can remove existing items from database."*
   → No serde back-compat or migration; the persisted `config_set` shape may
   change and pre-existing runs may be discarded.

This supersedes the earlier snapshot-refresh design. There is **no snapshot to
refresh**: limits are never frozen into the run, so resume/step/answer/resolve
all use live config. The previous "preserve in-flight snapshot semantics"
constraint is intentionally dropped. Retired TODOs from prior revisions are
listed at the end of the TODO section; their IDs are not reused.

### Safety decisions (in response to plan review)

- **Live limits are a required runner dependency (no default state).**
  `WorkflowRunner::new` takes the resolved policy as a required parameter; there
  is no `RunnerLimits::default()` fallback inside the runner and no optional
  builder that a caller could forget. (Grep confirms **14** current
  `WorkflowRunner::new` call sites: 13 in `crates/workflow/engine/src/runner.rs`
  tests and 1 production in `crates/workflow/engine/src/runtime.rs:1212`.)
- **Resolution is infallible by construction (no fallible call after any durable
  mutation).** `resolve_limits(&self, name: &str) -> ResolvedRuntimePolicy`
  returns a value, never a `Result`, and never indexes `config_sets`. Three
  total, exhaustive branches: (1) requested set present → use it;
  (2) requested absent but `DEFAULT_CONFIG_SET_NAME` present → warn and use
  `default`; (3) both absent → warn and use the built-in `RunnerLimits::default()`
  with effective name `"default"`. Because it cannot fail, it may be called at
  any point — including inside `run_existing_with_events` after lifecycle
  mutations — without risk of leaving a run advanced/`Running` without a policy.
  This is the reviewer's "infallible default invariant" alternative; it removes
  the resolve-after-mutation ordering hazard entirely. (Engine `RuntimeConfig` is
  public and directly constructed with arbitrary `config_sets`, e.g. the
  empty-map fixture at `runtime.rs:1807`; branch 3 covers that case without a
  panic and without an error.)
- **Exhaustion errors carry effective identity.** The runner is handed a
  `ResolvedRuntimePolicy { name, limits }` where `name` is the **effective** set
  (`"default"` when fallback occurred), and prints `policy.name` in budget/retry
  exhaustion messages — never the stale persisted name — so diagnostics can't
  claim `careful` while enforcing `default`.

## Plan

### Root cause (unchanged from RCA)

`RunnerLimits` are captured into per-run durable state once, at run start
(`start_catalog_workflow` → `resolve_config_set` → `run.config_set.limits`,
`runtime.rs:638,655,685-707`), and enforcement reads that frozen copy
(`run.config_set.limits` at `runner.rs:157,213,271-284`). A later `config.toml`
edit never reaches an already-started run (RCA Layer A). The long-lived TUI also
loads config once per process (RCA Layer B), consistent with the user's
"resolved every time cowboy is restarted" model.

### Fix: live resolution, name-only pointer

- **Persist only the name.** `WorkflowRun.config_set` becomes a name-only
  reference (the selected set name), not resolved limits.
- **Resolve limits live at one seam.** `start_run`, `resume_run`, `step_run`,
  `answer_run`, and `resolve_run` all funnel execution through
  `run_existing_with_events` (`runtime.rs:1165-1214`). Resolving
  `ResolvedRuntimePolicy` there and passing it into the runner covers all five
  paths in one place.
- **Missing-set policy.** *At start* an unknown/misspelled set name is a hard
  error before the run is persisted (preserves
  `unknown_config_set_fails_before_run_persistence`, `runtime.rs:2868`). *When
  executing an existing run*, `resolve_limits` is **infallible**: a persisted
  name absent from live config falls back to `default` (warn), and if `default`
  is also absent it falls back to the built-in `RunnerLimits::default()` (warn)
  with effective name `"default"`. No lifecycle path can be left advanced or
  `Running` by a failed resolution because resolution cannot fail.
- **Counters stay durable and cumulative.** `retries_used`/`step_retries_used`
  (`runner.rs:226-231`) remain in the run and are never reset; a raised live
  limit yields additional budget (`step_remaining = 20 - 2 = 18`), a lowered one
  narrows against accumulated counters. Step/visit budgets (`max_steps_per_run`,
  `max_visits_per_step`) also become live for existing runs.
- **Breaking change accepted.** `ResolvedConfigSet { name, limits }` is replaced
  by a name-only reference; no migration is written and pre-existing persisted
  runs may be discarded (store may be reset).

### Layer B (unchanged framing)

`self.config` is loaded once per process (`crates/tui/app/src/main.rs:16`; one
`WorkflowRuntime` at `crates/tui/app/src/app.rs:32`). Live resolution reads that
per-process config, so a **restarted** cowboy applies new limits to existing
runs. In-process `config.toml` hot-reload remains out of scope (documented
restart caveat).

## Changes

- `crates/workflow/core/src/state.rs`
  - Replace `ResolvedConfigSet { name, limits }` with a name-only
    `ConfigSetRef { name: String }` (keep `Default` = `DEFAULT_CONFIG_SET_NAME`),
    remove `limits`, and point `WorkflowRun.config_set` at it (field name kept).
    Update the state round-trip test (`state.rs:657-674`) to drop `.limits`.

- `crates/workflow/engine/src/runner.rs`
  - Add a resolved-policy type `ResolvedRuntimePolicy { name: String, limits:
    RunnerLimits }` (engine crate) and store it on `WorkflowRunner` as a
    **required** field.
  - Change `WorkflowRunner::new(store, executor, provider, events, policy:
    ResolvedRuntimePolicy)` — required parameter, no default state, no
    `with_limits` builder. Replace the three `run.config_set.limits` reads
    (`runner.rs:157,213,271`) with `self.policy.limits`, and print
    `self.policy.name` (effective) in the two exhaustion messages
    (`runner.rs:273-284`).
  - Update all 13 runner-test constructor sites to pass an explicit
    `ResolvedRuntimePolicy` (tests may pass `default` deliberately). Replace
    `agent_run_with_retry_limits`'s snapshot mutation with policy construction.

- `crates/workflow/engine/src/runtime.rs`
  - Replace `resolve_config_set(&definition) -> ResolvedConfigSet` with a name
    resolver returning `ConfigSetRef` (hard error on unknown at start,
    `runtime.rs:685-707`), persisted by `start_catalog_workflow`
    (`runtime.rs:638,655`).
  - Add `fn resolve_limits(&self, name: &str) -> ResolvedRuntimePolicy`
    (infallible; returns a value, not `Result`) with the three exhaustive
    branches above (present → use; requested absent but `default` present → warn
    + default; both absent → warn + `RunnerLimits::default()`, effective name
    `"default"`). Never index `config_sets`.
  - In `run_existing_with_events` (`runtime.rs:1165-1214`), before building the
    runner, `let policy = self.resolve_limits(&run.config_set.name);` and pass it
    as the required `WorkflowRunner::new` argument. Because resolution is
    infallible, its placement relative to the `resume_with`/`answer_run`/
    `resolve_run` durable mutations is immaterial — no run can be persisted as
    advanced/`Running` without an acquired policy.
  - Update runtime tests reading `run.config_set.limits`
    (`runtime.rs:2856-2864,2947,2952,3011,3018,3032,3048`) per TODO-15.

- `crates/workflow/store/src/redb_store.rs`
  - No production change required (serde follows the new type). Add a name-only
    round-trip test per TODO-18.

- Docs (all five authoritative locations, per TODO-16):
  `README.md` (lines ~244-255, ~322-327), `AGENTS.md` (lines ~197, ~248-250,
  ~376, ~473-477), `docs/architecture.md` (lines ~70-78, ~121-125, ~233-236),
  `docs/module-map.md` (lines ~84, ~99-106, ~129), `docs/workflow-authoring.md`
  (lines ~205-236). `docs/plans/workflow_config_sets.md` is explicitly excluded
  as historical.

## Tests to be added/updated

- **Keep byte-for-byte unchanged (fix input):**
  `resume_applies_increased_step_retry_budget_to_existing_failed_run`
  (`runtime.rs:4701-4807`) — flips FAIL → PASS (integrity guarded by TODO-09).

- **Must stay green unchanged:**
  - `unknown_config_set_fails_before_run_persistence` (`runtime.rs:2868`).
  - `resume_refails_when_fresh_attempt_fails_with_exhausted_step_budget`
    (`runtime.rs:4667`) — same config still re-fails; counters retained at `2`.

- **Rewritten (snapshot-retention → live-resolution), TODO-15:**
  - `step_and_resume_use_snapshot_after_config_set_changes` (`runtime.rs:2898`)
    → renamed `changed_config_set_limits_apply_live_on_resume_and_step`.
  - `answer_resolve_and_options_use_snapshot_after_set_deletion`
    (`runtime.rs:2955`) → renamed
    `deleted_set_answer_and_resolve_fall_back_to_default_limits`.

- **Added:**
  - `resume_of_failed_run_advances_cumulative_retry_counters` (TODO-08).
  - `step_run_applies_increased_step_retry_budget_to_existing_failed_run`
    (TODO-10).
  - `resume_of_failed_run_applies_lowered_whole_set_limits` (TODO-11).
  - `resolve_limits_*` three-branch unit tests (TODO-14).
  - `resume_of_failed_run_uses_default_limits_when_selected_set_is_deleted`
    (TODO-17).
  - `stores_and_loads_run_with_named_config_set_ref` (TODO-18).
  - `exhaustion_error_names_effective_default_policy_not_persisted_pointer`
    (TODO-19).

## How to verify

1. Regression test passes unchanged:
   ```
   cargo test -p cowboy-workflow-engine resume_applies_increased_step_retry_budget_to_existing_failed_run
   ```
2. New behavior + invariant tests pass (one filter per command):
   ```
   cargo test -p cowboy-workflow-engine resume_of_failed_run_advances_cumulative_retry_counters
   cargo test -p cowboy-workflow-engine step_run_applies_increased_step_retry_budget_to_existing_failed_run
   cargo test -p cowboy-workflow-engine resume_of_failed_run_applies_lowered_whole_set_limits
   cargo test -p cowboy-workflow-engine resume_of_failed_run_uses_default_limits_when_selected_set_is_deleted
   cargo test -p cowboy-workflow-engine exhaustion_error_names_effective_default_policy_not_persisted_pointer
   cargo test -p cowboy-workflow-engine resolve_limits_uses_requested_set_when_present
   cargo test -p cowboy-workflow-engine resolve_limits_falls_back_to_default_when_requested_missing
   cargo test -p cowboy-workflow-engine resolve_limits_falls_back_to_builtin_default_when_all_sets_missing
   ```
3. Rewritten + preserved guards pass (one filter per command):
   ```
   cargo test -p cowboy-workflow-engine changed_config_set_limits_apply_live_on_resume_and_step
   cargo test -p cowboy-workflow-engine deleted_set_answer_and_resolve_fall_back_to_default_limits
   cargo test -p cowboy-workflow-engine unknown_config_set_fails_before_run_persistence
   cargo test -p cowboy-workflow-engine resume_refails_when_fresh_attempt_fails_with_exhausted_step_budget
   ```
4. Store + core + engine suites and Clippy clean:
   ```
   cargo test -p cowboy-workflow-core
   cargo test -p cowboy-workflow-store
   cargo test -p cowboy-workflow-engine
   cargo clippy -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-engine --all-targets -- -D warnings
   ```

## TODO

- [x] TODO-03: Confirm the investigator regression test passes unchanged (`resume` path).
  - Procedure: Do not edit the test body. After TODO-13/TODO-14 land, run
    `cargo test -p cowboy-workflow-engine resume_applies_increased_step_retry_budget_to_existing_failed_run`.
  - Expected result: `test result: ok. 1 passed; 0 failed`; the resumed run
    reaches `RunStatus::Completed` because the second runtime's
    `max_retries_per_step = 20` is resolved live on resume.
  - Implementer observed result: `cargo test -p cowboy-workflow-engine resume_applies_increased_step_retry_budget_to_existing_failed_run` printed `test result: ok. 1 passed; 0 failed`. The test body is byte-identical (TODO-09 hash MATCH).

- [x] TODO-06: Document resume/step whole-set re-resolution and the Layer B restart caveat.
  - Procedure: In `AGENTS.md` (Configuration/Persistence prose), state that a run
    persists only its config-set **name** and that effective limits are resolved
    from current config on every operation (so resuming or stepping after a
    config edit — including a raised retry budget — applies the current limits),
    that a deleted set falls back to `default` limits with a warning, and that
    new runs in a long-lived TUI still need a restart to pick up config edits
    (Layer B, unchanged). Verify by re-reading the edited section.
  - Expected result: the prose describes name-only persistence, live whole-set
    re-resolution on every operation, the deleted-set fallback, and the restart
    caveat.
  - Implementer observed result: `AGENTS.md` Configuration prose now states name-only persistence, live whole-set limit resolution on every operation, deleted-set fallback to `default` (then built-in), and the long-lived-TUI restart caveat for new runs. Re-read confirms the wording.

- [x] TODO-07: Run the full engine suite and Clippy clean.
  - Procedure: Run, in order,
    `cargo test -p cowboy-workflow-core`,
    `cargo test -p cowboy-workflow-store`,
    `cargo test -p cowboy-workflow-engine`, then
    `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-engine --all-targets -- -D warnings`.
  - Expected result: all three suites pass; Clippy reports no warnings across
    core, store, and engine.
  - Implementer observed result: `cargo test -p cowboy-workflow-core` (35 passed), `cargo test -p cowboy-workflow-store` (22 passed), `cargo test -p cowboy-workflow-engine` (139 passed) all `0 failed`; `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-engine --all-targets -- -D warnings` finished with no warnings.

- [x] TODO-08: Add a cumulative-counter progression test for successful resume
  recovery.
  - Procedure: Add `#[tokio::test]`
    `resume_of_failed_run_advances_cumulative_retry_counters`. Fail a run at
    `2/2` under `max_retries_per_step = 2` (assert `step_retries_used["start"]
    == 2` and `retries_used == 2` on the failed run), then resume through a
    second runtime with `max_retries_per_step = 20` whose scripted agent returns
    one frontmatter-less reply followed by a success reply. Run
    `cargo test -p cowboy-workflow-engine resume_of_failed_run_advances_cumulative_retry_counters`.
  - Expected result: `1 passed; 0 failed`, with `report.run.status ==
    RunStatus::Completed`, `report.run.retries_used == 3`, and
    `report.run.step_retries_used.get("start") == Some(3)` — proving the live
    limit adds exactly one further retry on top of the retained `2` and does
    **not** reset accounting.
  - Implementer observed result: `cargo test -p cowboy-workflow-engine resume_of_failed_run_advances_cumulative_retry_counters` printed `1 passed; 0 failed`; the resumed run reaches `RunStatus::Completed` with `retries_used == 3` and `step_retries_used["start"] == 3`.

- [x] TODO-09: Verify the investigator repro test block is byte-identical
  before and after implementation (source integrity, independent of behavior).
  - Execution order: this TODO's baseline capture (step 1) is the **first
    implementation action**, performed before any edit to
    `crates/workflow/engine/src/runtime.rs`. Step 2 is the last action after all
    edits. (Prior reviewer baseline SHA-256 for reference:
    `c98ae525088215befdfe778e34fc74554c205b06a79de2c6a02116cc3382d038`.)
  - Procedure (run via the Eval tool as Python — no shell heredoc; brace-matched
    extraction hashes the **complete test construct** starting at its
    `#[tokio::test]` attribute):
    1. **Baseline (before any edit).** Run this in a Python Eval cell and record
       the printed hash:
       ```python
       import hashlib
       s = open('crates/workflow/engine/src/runtime.rs').read()
       needle = 'async fn resume_applies_increased_step_retry_budget_to_existing_failed_run'
       f = s.index(needle)
       start = s.rindex('#[tokio::test]', 0, f)
       b = s.index('{', f)
       depth = 0
       end = None
       for j in range(b, len(s)):
           if s[j] == '{':
               depth += 1
           elif s[j] == '}':
               depth -= 1
               if depth == 0:
                   end = j + 1
                   break
       block = s[start:end]
       print(hashlib.sha256(block.encode()).hexdigest())
       ```
    2. **Post-implementation (after every edit).** Re-run the identical cell and
       compare the printed hash to the recorded baseline.
  - Expected result: the two SHA-256 hashes are identical, proving the
    investigator test construct (attribute through the brace-matched function)
    was not modified even though product code and new tests changed the same
    file.
  - Implementer observed result: baseline hash (before any edit) and post-implementation hash both `c98ae525088215befdfe778e34fc74554c205b06a79de2c6a02116cc3382d038` (MATCH), confirming the investigator test construct was unchanged.

- [x] TODO-10: Add a `step_run` (SingleStep) changed-config refresh test to lock
  the shared-path contract for both commands.
  - Procedure: Add `#[tokio::test]`
    `step_run_applies_increased_step_retry_budget_to_existing_failed_run`.
    Fail a run at `2/2` under `max_retries_per_step = 2`, then call `step_run`
    (not `resume_run`) on it through a second runtime with
    `max_retries_per_step = 20` whose scripted agent recovers after one retry.
    Run
    `cargo test -p cowboy-workflow-engine step_run_applies_increased_step_retry_budget_to_existing_failed_run`.
  - Expected result: `1 passed; 0 failed`. `step_run` does **not** re-fail at
    `2/2`: the `start` step recovers and the run advances (assert
    `report.run.status == RunStatus::Running`, `report.run.current_step ==
    "finish"`), and `report.run.step_retries_used.get("start") == Some(3)` —
    proving live resolution applies to the `SingleStep` path too, since both
    `resume_run` and `step_run` funnel through `run_existing_with_events`.
  - Implementer observed result: `cargo test -p cowboy-workflow-engine step_run_applies_increased_step_retry_budget_to_existing_failed_run` printed `1 passed; 0 failed`; `report.run.status == Running`, `current_step == "finish"`, `step_retries_used["start"] == 3`.

- [x] TODO-11: Add a lowered-limit whole-set-replacement test to make the
  whole-set contract observable in the narrowing direction.
  - Procedure: Add `#[tokio::test]`
    `resume_of_failed_run_applies_lowered_whole_set_limits`. Fail a run at its
    per-step ceiling under `max_retries_per_step = 5` (assert
    `step_retries_used["start"] == 5`), then resume through a second runtime
    whose set has `max_retries_per_step = 3` and a scripted agent that returns
    one frontmatter-less reply. Run
    `cargo test -p cowboy-workflow-engine resume_of_failed_run_applies_lowered_whole_set_limits`.
  - Expected result: `1 passed; 0 failed`. `resume_run(...).await.unwrap_err()`
    reports the lowered live limit — the message contains `5/3 retries used`
    (used count `5` against the new ceiling `3`), proving whole-set live
    resolution applied the lowered `max_retries_per_step`; and the reloaded run's
    `retries_used`/`step_retries_used["start"]` remain `5` (no retry ran,
    counters not reset).
  - Implementer observed result: `cargo test -p cowboy-workflow-engine resume_of_failed_run_applies_lowered_whole_set_limits` printed `1 passed; 0 failed`; the resume error contains `5/3 retries used` and the reloaded run keeps `retries_used == 5` / `step_retries_used["start"] == 5`.

- [x] TODO-13: Replace the stored resolved config set with a name-only pointer in
  core state (breaking change; no migration).
  - Procedure:
    1. In `crates/workflow/core/src/state.rs`, replace `ResolvedConfigSet {
       name, limits }` with `ConfigSetRef { name: String }` (keep `Default` =
       `DEFAULT_CONFIG_SET_NAME`), remove the `limits` field, and point
       `WorkflowRun.config_set` at it (field name unchanged). Update every
       constructor/`Default::default()` site to compile; update `.limits`
       readers per TODO-14.
    2. Update the state round-trip test (`state.rs:657-674`) to construct the
       name-only ref and drop `.limits` assertions.
    3. Build: `cargo build -p cowboy-workflow-core`.
  - Expected result: `cowboy-workflow-core` compiles; `WorkflowRun.config_set`
    has no `limits` field; `cargo test -p cowboy-workflow-core` passes.
  - Implementer observed result: `ResolvedConfigSet` replaced by `ConfigSetRef { name }`; `WorkflowRun.config_set` has no `limits` field; `cargo build -p cowboy-workflow-core` compiled and `cargo test -p cowboy-workflow-core` printed `35 passed; 0 failed`.

- [x] TODO-14: Resolve `RunnerLimits` from live config at runtime and pass them
  into the runner instead of reading them off the run.
  - Procedure:
    1. In `crates/workflow/engine/src/runner.rs`, add
       `pub struct ResolvedRuntimePolicy { pub name: String, pub limits:
       RunnerLimits }`; add it as a **required** `WorkflowRunner` field. Change
       the constructor to `WorkflowRunner::new(store, executor, provider, events,
       policy: ResolvedRuntimePolicy)` with **no** default and **no**
       `with_limits` builder. Replace the three `run.config_set.limits` reads
       (`runner.rs:157,213,271`) with `self.policy.limits`, and print
       `self.policy.name` in both exhaustion messages (`runner.rs:273-284`).
       Update all 13 runner-test call sites to pass an explicit
       `ResolvedRuntimePolicy`.
    2. In `crates/workflow/engine/src/runtime.rs`, replace `resolve_config_set`
       with a name resolver (hard error on unknown at start; returns
       `ConfigSetRef`) and add `fn resolve_limits(&self, name: &str) ->
       ResolvedRuntimePolicy` (**infallible**; returns a value, never `Result`)
       implementing exactly three exhaustive branches: (a) requested set present
       → `ResolvedRuntimePolicy { name: requested, limits }`; (b) requested absent
       but `DEFAULT_CONFIG_SET_NAME` present → `tracing::warn!` and return
       `ResolvedRuntimePolicy { name: "default", limits: default_limits }`;
       (c) both absent → `tracing::warn!` and return `ResolvedRuntimePolicy {
       name: "default", limits: RunnerLimits::default() }`. Never index
       `self.config.config_sets`. In `run_existing_with_events`
       (`runtime.rs:1165-1214`) call `let policy =
       self.resolve_limits(&run.config_set.name);` and pass it into
       `WorkflowRunner::new`. Because `resolve_limits` cannot fail, no durable
       lifecycle mutation in `resume_with`/`answer_run`/`resolve_run` can be
       stranded by a later resolution failure.
    3. Add three `#[tokio::test]` (or sync) unit tests for `resolve_limits` and
       run each with its exact command:
       - `resolve_limits_uses_requested_set_when_present` (asserts returned
         `name`/`limits` equal the requested set) —
         `cargo test -p cowboy-workflow-engine resolve_limits_uses_requested_set_when_present`.
       - `resolve_limits_falls_back_to_default_when_requested_missing` (requested
         absent, `default` present; asserts returned `name == "default"` and
         limits equal the `default` set) —
         `cargo test -p cowboy-workflow-engine resolve_limits_falls_back_to_default_when_requested_missing`.
       - `resolve_limits_falls_back_to_builtin_default_when_all_sets_missing`
         (empty `config_sets`; asserts the call returns — no panic — with
         `name == "default"` and `limits == RunnerLimits::default()`) —
         `cargo test -p cowboy-workflow-engine resolve_limits_falls_back_to_builtin_default_when_all_sets_missing`.
    4. Build: `cargo build -p cowboy-workflow-engine`.
  - Expected result: `cowboy-workflow-engine` compiles with no warnings; the
    three `resolve_limits_*` tests each print `1 passed; 0 failed` (including the
    all-missing branch returning the built-in default rather than panicking or
    erroring); and a Grep-tool search — `grep` for pattern `config_set\.limits`
    over path `crates/workflow/engine/src; crates/workflow/store/src;
    crates/workflow/core/src` — returns **zero** matches in production
    (non-`#[cfg(test)]`) code.
  - Implementer observed result: `cowboy-workflow-engine` compiles with no warnings; the three `resolve_limits_*` tests each printed `1 passed; 0 failed` (including the all-missing branch returning the built-in default); the Grep-tool search for `config_set\.limits` over `crates/workflow/engine/src; crates/workflow/store/src; crates/workflow/core/src` returned zero matches (all remaining reads use `self.policy.limits`).

- [x] TODO-15: Convert the config-set semantic tests from snapshot-retention to
  live-resolution.
  - Procedure:
    1. **Changed-set behavior, exercised on BOTH `step_run` and `resume_run`
       (separate runs).** Rename
       `step_and_resume_use_snapshot_after_config_set_changes`
       (`runtime.rs:2898`) to
       `changed_config_set_limits_apply_live_on_resume_and_step`, keeping its
       three-step `multi` workflow (`first --next--> second --next--> third`,
       `third` returns success). Start **two** separate stepwise runs (run A and
       run B) under `careful = RunnerLimitsConfig { max_steps_per_run: 5,
       max_visits_per_step: 5, max_retries_per_run: 5, max_retries_per_step: 2 }`
       (assert each `started.run.steps_executed == 1`, `current_step == "second"`).
       Build a second runtime whose `careful` set lowers `max_steps_per_run` to
       `1` (other fields unchanged). On run A call `step_run`; on run B call
       `resume_run`. For each: assert the returned
       `unwrap_err().to_string().contains("run exceeded max step count (1)")`,
       then reload the run and assert `matches!(loaded.status, RunStatus::Failed
       { .. })`, `loaded.current_step == "second"`, and `loaded.steps_executed ==
       1` (the budget check at `engine.rs:151` fires before
       `increment_budget`, so the count does not advance). Remove all
       `.config_set.limits` field access. Run
       `cargo test -p cowboy-workflow-engine changed_config_set_limits_apply_live_on_resume_and_step`.
    2. **Deleted-set answer/resolve fallback, with persisted-state reload.**
       Rename `answer_resolve_and_options_use_snapshot_after_set_deletion`
       (`runtime.rs:2955`) to
       `deleted_set_answer_and_resolve_fall_back_to_default_limits`, keeping the
       `answer` and `resolve` workflows (both `config_set = "careful"`). Start
       both under a creator runtime with `default { max_steps_per_run: 5 }` and
       `careful { max_steps_per_run: 2 }` (assert the `answer` run reaches
       `WaitingForInput` with `steps_executed == 1` and the `resolve` run reaches
       `Failed` with `steps_executed == 1`). Build a second runtime that
       **omits** `careful` and sets `default { max_steps_per_run: 1 }`. Assert
       `answer_run(&waiting.id, "approval", "yes").await.unwrap_err()
       .to_string().contains("run exceeded max step count (1)")` and
       `resolve_run(&failed.id, "fixed", None, None).await.unwrap_err()
       .to_string().contains("run exceeded max step count (1)")` — proving the
       deleted set falls back to `default` limit `1` (careful's `2` would have
       completed both). Then **reload each run** and assert the exact durable
       state produced by the source mutation order — `apply_step_record` writes
       the answer/manual-resolution record, advances `current_step` to `done`
       and sets a new head, *then* the live `default.max_steps_per_run == 1`
       rejects executing `done` and the runner persists `Failed` (the budget
       check at `engine.rs:151` fires before `increment_budget`, so
       `steps_executed` does not advance). For **each** reloaded run assert all
       of: `matches!(loaded.status, RunStatus::Failed { .. })`; `loaded.status !=
       RunStatus::Completed`; `loaded.current_step == "done"`; `loaded.steps_
       executed == 1`; and `loaded.head != pre_operation_head` where the new
       `loaded.head` resolves to the answer record (answer run) / the
       `manual_resolution` record (resolve run) — capture each run's head before
       calling `answer_run`/`resolve_run` to compare. Remove all
       `.config_set.limits` field access. Run
       `cargo test -p cowboy-workflow-engine deleted_set_answer_and_resolve_fall_back_to_default_limits`.
    3. Update start-time assertions at `runtime.rs:2856-2864` to keep `.name`
       checks and drop `.limits` checks.
  - Expected result: both renamed tests print `1 passed; 0 failed`. Assertion 1
    proves a lowered live `max_steps_per_run` blocks **both** the stepped (run A)
    and resumed (run B) run with reloaded `Failed` / retained `current_step ==
    "second"` / unadvanced `steps_executed == 1` durable state; assertion 2
    proves a deleted set uses `default` fallback limits, with each run reloaded
    as `Failed`, `current_step == "done"`, `steps_executed == 1`, and a new head
    resolving to the answer / `manual_resolution` record (never `Completed`). No
    reference to a removed `.config_set.limits` field remains anywhere in the
    module.
  - Implementer observed result: `cargo test -p cowboy-workflow-engine changed_config_set_limits_apply_live_on_resume_and_step` and `deleted_set_answer_and_resolve_fall_back_to_default_limits` each printed `1 passed; 0 failed`; start-time assertions keep `.name` only; no `.config_set.limits` reference remains in the module.

- [x] TODO-16: Document the breaking persisted-shape change and store reset
  allowance.
  - Procedure: Update all five authoritative locations to describe name-only
    persistence with live limit resolution (replacing snapshot-of-limits prose):
    `README.md` (~244-255, ~322-327), `AGENTS.md` (~248-250, ~376, ~473-477),
    `docs/architecture.md` (~70-78, ~121-125, ~233-236),
    `docs/module-map.md` (~84, ~99-106, ~129), and
    `docs/workflow-authoring.md` (~205-236). Explicitly exclude
    `docs/plans/workflow_config_sets.md` as historical. State the breaking
    persisted-shape change (no migration) and the exact operator reset action as
    three separate ordered steps: (1) stop all Cowboy processes; (2) delete the
    configured `workflow_store` file (default
    `${XDG_STATE_HOME:-~/.local/state}/cowboy/workflow.redb`, which MAY be
    configured to a path outside `state_dir`); (3) delete the
    `<state_dir>/events` directory (always under `state_dir`, default
    `${XDG_STATE_HOME:-~/.local/state}/cowboy/events`, and NOT necessarily
    beside the store). Do not phrase this as deleting the store "plus its
    `events/` directory". Verify by re-reading each edited section and grepping
    for stale "snapshots ... effective limits" phrasing.
  - Expected result: all five docs describe name-only persistence + live
    resolution + `default` fallback + restart caveat; none still claim the run
    snapshots effective limits; the reset action names the exact store path; a
    Grep-tool search for `snapshots the selected` / `snapshots effective limits`
    over those five files returns zero matches.
  - Implementer observed result: `README.md`, `AGENTS.md`, `docs/architecture.md`, `docs/module-map.md`, and `docs/workflow-authoring.md` now describe name-only persistence, live resolution, `default` fallback, and the restart caveat; each names the store reset as three ordered steps with the exact `workflow_store` and `<state_dir>/events` paths; a Grep-tool search for `snapshots the selected` / `snapshots effective limits` over the five files returns zero matches.

- [x] TODO-17: Add the deleted-set fallback resume test with an accurate subject
  and test name.
  - Procedure: Add `#[tokio::test]`
    `resume_of_failed_run_uses_default_limits_when_selected_set_is_deleted`.
    Start a run under a **named non-default** set (e.g. `careful`) whose
    `max_retries_per_step = 2`, drive it to `Failed` at `2/2` (assert
    `step_retries_used["start"] == 2`). Resume through a second runtime whose
    `config_sets` **omits** `careful` but keeps `default` with
    `max_retries_per_step = 20`, whose scripted agent succeeds after one retry.
    Run
    `cargo test -p cowboy-workflow-engine resume_of_failed_run_uses_default_limits_when_selected_set_is_deleted`.
  - Expected result: `1 passed; 0 failed`. The run completes
    (`report.run.status == RunStatus::Completed`) using the `default` fallback
    limit `20`; `report.run.config_set.name` is still the persisted `careful`
    (the durable pointer is unchanged); and
    `report.run.step_retries_used.get("start") == Some(3)` (counters advanced,
    not reset). Proves deleted-set fallback to `default` without panic and with
    correct persisted-name provenance.
  - Implementer observed result: `cargo test -p cowboy-workflow-engine resume_of_failed_run_uses_default_limits_when_selected_set_is_deleted` printed `1 passed; 0 failed`; the run completes, `config_set.name` is still `careful`, and `step_retries_used["start"] == 3`.

- [x] TODO-18: Add a current-format store round trip for the name-only config-set
  reference.
  - Procedure: In `crates/workflow/store/src/redb_store.rs` tests, add
    `#[test] fn stores_and_loads_run_with_named_config_set_ref`: build a
    `WorkflowRun` (reuse the `run()` fixture) whose `config_set` is a non-default
    `ConfigSetRef { name: "careful".into() }`, `save_run`, then `load_run` and
    assert full equality and specifically `loaded.config_set.name == "careful"`.
    Run `cargo test -p cowboy-workflow-store stores_and_loads_run_with_named_config_set_ref`.
  - Expected result: `1 passed; 0 failed`; the name-only reference round-trips
    through redb in the current format (no `limits` field involved).
  - Implementer observed result: `cargo test -p cowboy-workflow-store stores_and_loads_run_with_named_config_set_ref` printed `1 passed; 0 failed`; the `ConfigSetRef { name: "careful" }` run round-trips through redb with full equality.

- [x] TODO-19: Prove exhaustion diagnostics name the effective fallback policy,
  not the persisted pointer.
  - Procedure: Add a runner-level `#[tokio::test]`
    `exhaustion_error_names_effective_default_policy_not_persisted_pointer` in
    `crates/workflow/engine/src/runner.rs` tests. Construct a `WorkflowRunner`
    whose run persists `config_set = ConfigSetRef { name: "careful" }` but whose
    required `ResolvedRuntimePolicy` is `{ name: "default", limits }` (the
    effective fallback identity), with `max_retries_per_step` small enough to
    force per-step retry exhaustion via a `FlakyDispatcher` that always fails
    recoverably (mirror the existing exhaustion tests at `runner.rs:1109-1279`).
    Drive the step to exhaustion and capture the error string. Run
    `cargo test -p cowboy-workflow-engine exhaustion_error_names_effective_default_policy_not_persisted_pointer`.
  - Expected result: `1 passed; 0 failed`. The exhaustion error
    `.to_string()` contains `config set "default"` and does **not** contain
    `"careful"` — proving the runner attributes the enforced budget to the
    effective policy name (`policy.name`), never the stale persisted pointer.
  - Implementer observed result: `cargo test -p cowboy-workflow-engine exhaustion_error_names_effective_default_policy_not_persisted_pointer` printed `1 passed; 0 failed`; the exhaustion error string contains `config set "default"` and not `"careful"`.

### Retired TODOs (superseded; IDs not reused)

- **TODO-01** ("Add `refresh_resumed_config_set` helper …") — obsolete: no
  snapshot exists to refresh; limits are resolved live (TODO-14).
- **TODO-02** ("Wire the helper into the `Failed` branch of `resume_with` …") —
  obsolete: resolution is unconditional at `run_existing_with_events` (TODO-14).
- **TODO-04** ("Confirm preserved-snapshot semantics guards stay green …") —
  obsolete: those guards asserted snapshot retention and are rewritten to
  live-resolution (TODO-15).
- **TODO-05** ("Add the config-set-deleted resume guard with strong
  assertions.") — retired unchanged: its established test name
  (`resume_of_failed_run_keeps_snapshot_when_config_set_deleted`) asserts
  snapshot retention, which no longer exists. Replaced by TODO-17 with an
  accurate fallback subject/test name; TODO-05's ID is not reused.
- **TODO-12** ("Add a `WaitingForInput`-through-`resume_with` snapshot-retention
  guard …") — obsolete: `WaitingForInput` runs also resolve live; deleted-set
  answer/resolve fallback is covered by TODO-15.
