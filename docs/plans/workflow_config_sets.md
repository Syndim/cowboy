# Plan

Introduce named workflow config sets as the single source of runner policy. `AppConfig.config_sets` will be a TOML table keyed by set name. Every set exposes exactly four `u32` fields: `max_steps_per_run`, `max_visits_per_step`, `max_retries_per_run`, and `max_retries_per_step`. Each field is optional in TOML and independently inherits the built-in values `100`, `20`, `200`, and `2`; omitting `config_sets` materializes the built-in `default` set, and declaring only custom sets still retains that built-in `default`. A partial `[config_sets.default]` overrides only the fields present. Apply `deny_unknown_fields` to each set, reject blank set names, reject `max_steps_per_run = 0` and `max_visits_per_step = 0`, and accept zero for either retry field to disable that retry scope. TOML map keys provide duplicate-name rejection.

```toml
[config_sets.default]
max_steps_per_run = 100
max_visits_per_step = 20
max_retries_per_run = 200
max_retries_per_step = 2

[config_sets.careful]
# Omitted step/visit fields inherit 100 and 20.
max_retries_per_run = 20
max_retries_per_step = 4
```

A Lua workflow selects a set with `workflow(name, head, { config_set = "careful" })`. The compiled `WorkflowDefinition` retains the optional name; omission selects `default`. Blank names fail Lua conversion. An unknown name fails when a new run is started, after source compilation but before any `WorkflowRun` is persisted, with an error naming the workflow, requested set, and available sets.

Snapshot the resolved set name and all four effective limits into the new run’s durable state. Resume, single-step, ask-user answer, manual resolution, and resolution-option listing must read that snapshot instead of re-resolving `RuntimeConfig`. Existing runs deserialize to the built-in `default` snapshot. Therefore editing or deleting a named set affects only runs created afterward; already-created runs remain executable and retain their original limits.

`max_retries_per_run` caps recoverable retry dispatches across the durable run. `max_retries_per_step` caps recoverable retries accumulated for one step id across every visit to that step. Initial attempts do not count as retries, non-recoverable failures consume no retry budget, and retries still do not consume step/visit budgets. The numeric default `2` is preserved, but its behavior deliberately changes from two retries per step visit to two retries per step id for the entire run; repeated visits share what remains.

Keep `StepRetrying.attempt` visit-local because the same value feeds the retry prompt: the initial dispatch is attempt `1`, then retry events are attempts `2..=max_attempts`. After the initial recoverable failure, compute one fixed visit allowance as `min(remaining run retries, remaining retries for the current step)` and set `max_attempts = 1 + allowance` for every retry event in that visit. Before each retry, check the run ceiling first and then the step ceiling; if both are exhausted, report run-budget exhaustion. Reserve the retry by incrementing both counters and saving the run before emitting `StepRetrying` and dispatching the backend call, so a crash cannot replay an uncounted retry. Exhaustion errors must identify the selected set, exhausted scope, used/allowed counts, step id when applicable, and last recoverable error.

Use a clean configuration cutover: remove the three top-level runner-limit keys in favor of `config_sets`; `AppConfig`’s existing `deny_unknown_fields` makes old keys fail with a migration-oriented parse error. Update every in-repository constructor and authoritative example rather than maintaining two precedence paths.

# Changes

- In `crates/tui/app/src/config.rs`, add a `ConfigSetConfig` with per-field serde defaults and `deny_unknown_fields`, merge the built-in `default` entry with configured overrides/custom entries, validate names and nonzero step/visit limits, remove top-level limits, and convert the complete map into engine runtime values. Include the exact TOML contract above in type/field documentation where appropriate.
- In `crates/workflow/engine/src/runtime.rs` and `lib.rs`, add the runtime config-set model and one resolver used only while creating a run. Replace `RuntimeConfig.limits` with named sets, resolve explicit or `default` selection before `save_run`, and snapshot the resolved set into the run. `run_existing_with_events` must construct `WorkflowRunner` from the run snapshot, not current process configuration.
- In `crates/workflow/core/src/definition.rs`, add serde-defaulted optional `config_set` metadata to `WorkflowDefinition`. In `crates/workflow/lua/src/convert.rs`, parse a nonblank string from the workflow config table while preserving description shorthand and config-table behavior.
- In `crates/workflow/core/src/engine.rs` and `state.rs`, extend `RunnerLimits` with `max_retries_per_run`, make it serializable/defaultable, add a durable resolved-config snapshot to `WorkflowRun`, and add serde-defaulted total/per-step retry counters. Update every `WorkflowRun`, `WorkflowDefinition`, and `RunnerLimits` constructor affected by the new fields.
- In `crates/workflow/engine/src/runner.rs`, calculate the visit-local allowance from the snapshotted cumulative budgets, enforce run-before-step exhaustion precedence, reserve and persist counters before each retry event/dispatch, preserve local `attempt` numbering and fixed `max_attempts`, and produce distinct run/step exhaustion errors without retrying non-recoverable failures.
- Route lifecycle operations deliberately: `start_run`, `start_run_stepwise`, `start_run_with_workflow`, and `start_run_with_workflow_stepwise` continue through `start_catalog_workflow` and its single config-set resolver; `resume_run`, `step_run`, `answer_run`, `resolve_run`, and `resolution_options` use the persisted snapshot and remain functional if the source set is changed or removed.
- Update `crates/workflow/engine/src/bin/engine-cli.rs` and all other direct `RuntimeConfig` callers/test helpers to construct a named `default` set. Update store/TUI fixtures for the new backward-compatible run fields and keep retry-card rendering compatible with unchanged event fields.
- After behavior passes, update `README.md`, `demo-config.toml`, `docs/workflow-authoring.md`, `docs/architecture.md`, `docs/module-map.md`, and repository `AGENTS.md`. Document the exact TOML syntax/defaulting and zero-value rules, Lua selection/fallback and unknown-set behavior, snapshotted lifecycle policy, cumulative retry semantics, event numbering, clean-cutover migration, and module ownership.

# Tests to be added/updated

- Extend `crates/tui/app/src/config.rs` tests for no `config_sets`, a partial default override, a partial custom set inheriting all omitted fields, multiple sets, runtime conversion, blank names, unknown set fields, zero retry limits, rejected zero step/visit limits, and rejection of removed top-level keys. Parse the documented TOML example as a fixture-level assertion.
- Extend `crates/workflow/lua/src/loader.rs` tests for explicit selection, omission, coexistence with `description`, blank strings, non-string values, and serde compatibility for definitions created before `config_set` existed.
- Extend core state/engine tests for the new default limits, old-run deserialization into the built-in resolved snapshot and zero counters, retry-counter serialization, and continued independence from step/visit budgets.
- Extend `crates/workflow/engine/src/runner.rs` tests for successful retries, zero retry limits, non-recoverable failures, and separate run/step exhaustion messages. Add a repeated-visit test where the first visit consumes one retry and succeeds, then the next visit can use only the one remaining per-step retry. Assert visit-local `attempt` and fixed `max_attempts` when the run ceiling is tighter than the step ceiling, plus run-first precedence when both are exhausted.
- Add a store-backed reload/resume runner test that observes counters saved before retry dispatch, reloads the run, and proves the consumed run and step budgets cannot be reused after process reconstruction.
- Add runtime integration coverage around the centralized start resolver: explicit selection uses its effective values, omission snapshots `default`, and an unknown name leaves no persisted run. Then cover every persisted path with representative states: `resume_run` and `step_run` after set modification, `answer_run` after set deletion, `resolve_run` after set deletion, and `resolution_options` after set deletion; each must use the snapshot and remain available.
- Update TUI retry-event tests to retain visit-local `attempt/max_attempts` rendering, and update engine CLI/test-app fixtures for the new configuration shape. Treat `demo-config.toml` parsing plus a documented-workflow smoke run as the executable documentation check; manually verify the six authoritative documents use the same names, defaults, cumulative semantics, and lifecycle policy.

# How to verify

- Run `cargo test -p cowboy-workflow-lua`.
- Run `cargo test -p cowboy-workflow-core` and `cargo test -p cowboy-workflow-store`.
- Run `cargo test -p cowboy-workflow-engine runner::tests` and `cargo test -p cowboy-workflow-engine runtime::tests`.
- Run `cargo test -p cowboy config::tests` and the affected TUI event tests, including the `demo-config.toml` parsing coverage.
- Run `cargo run -p cowboy -- --config demo-config.toml runs` to prove the shipped example config loads, then run a temporary workflow once with an explicit non-default set and once without `config_set`; confirm the persisted snapshots and unknown-set pre-persistence error match the contract.
- Review `README.md`, `docs/workflow-authoring.md`, `docs/architecture.md`, `docs/module-map.md`, repository `AGENTS.md`, and `demo-config.toml` together for the same field names, values, migration note, cumulative retry definition, and snapshot policy.
- Run `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-lua -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings`.

# TODO

- [x] Implement the complete TOML config-set parsing/defaulting/validation contract, exact example, and clean removal of top-level limits.
- [x] Add optional validated `config_set` metadata to core workflow definitions and Lua conversion.
- [x] Add serializable runner limits, a backward-compatible durable resolved-config snapshot, and aggregate/per-step retry counters to `WorkflowRun`.
- [x] Resolve explicit/default config sets once before new-run persistence and make all subsequent lifecycle operations use the snapshot.
- [x] Enforce cumulative run and step retry budgets with run-first exhaustion precedence and no step/visit-budget consumption.
- [x] Preserve visit-local retry event numbering, compute fixed `max_attempts` from both ceilings, and reserve counters durably before event emission/dispatch.
- [x] Migrate every direct config, runtime, definition, limits, and run constructor plus engine CLI, store, and TUI fixtures.
- [x] Add config parsing, Lua conversion, core compatibility, runner boundary/event, durable reload, and distinct exhaustion-reason tests.
- [x] Add integration coverage for explicit/default/unknown start resolution and snapshot stability through resume, step, answer, resolve, and resolution-option paths.
- [x] Update README, demo config, workflow authoring, architecture, module map, and repository agent guidance with one authoritative contract.
- [x] Run all focused tests, executable documentation smoke checks, consistency review, and Clippy; fix every failure or warning.
