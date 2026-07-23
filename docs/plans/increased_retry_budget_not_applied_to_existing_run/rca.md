# RCA: A raised `max_retries_per_step` in config is not applied — two independent staleness layers

> **Feedback provenance (kept distinct throughout).** This RCA cites two raw
> **user-feedback** items and nothing more:
> 1. the initial report (the `2/2` error + "I changed to 20 in the config but
>    doesn't seem to work"), and
> 2. the follow-up "So you mean the config is working? The issue I found is
>    because of I didn't restart cowboy? That's not what I saw I think".
>
> These are **user** statements. Nothing here is a reviewer objection, reviewer
> assessment, or reviewer rerun evidence. Everything below that is not a direct
> quote of those two items is **investigator inference from source**, and is
> labeled as such. The user's message does **not** state which command or TUI
> action followed the config edit, so this RCA does **not** assert which path
> the user took; both candidate paths are presented as branch-dependent,
> source-grounded explanations.

## Bug behavior

A run fails with the per-step recoverable retry budget exhausted:

```
error: invalid action: config set "default" exhausted retry budget
for step "plan": 2/2 retries used; last recoverable error: recoverable
action failure: agent reply did not contain a workflow result
```

The user raised `max_retries_per_step` from the default `2` to `20` in
`config.toml` and reports the ceiling stayed `2/2`. Three separate facts:

1. **Where `2` comes from.** `2` is the default of `max_retries_per_step`,
   defined identically in `ConfigSetConfig::default()`
   (`crates/tui/app/src/config.rs:63`) and `RunnerLimits::default()`
   (`crates/workflow/core/src/engine.rs:29`). When `config.toml` omits the
   field (or the whole `[config_sets.default]` table), the loader materializes
   the built-in `default` set carrying this `2`
   (`deserialize_config_sets`, `config.rs:76-87`).

2. **The config edit is wired correctly for a new run in a fresh process.**
   `runtime_config` copies each set's `max_retries_per_step` into the runtime
   (`config.rs:252`), and `resolve_config_set` reads it from live config at
   run-start (`crates/workflow/engine/src/runtime.rs:685-707`). So the value is
   wired end-to-end; the edit is not ignored *in general*.

3. **Why the ceiling can still read `2/2` after the edit.** Two independent
   staleness layers, each of which is sufficient on its own to reproduce the
   symptom. The user's report does not say which one they encountered; both are
   documented so a fix addresses whichever applies.

## Root cause

`max_retries_per_step` is captured into per-run durable state exactly once, at
run start, from a process-wide config that is itself loaded exactly once at
process start. Neither the run's snapshot nor the process's in-memory config is
refreshed afterward.

**Layer A — durable per-run snapshot (branch: user resumed/stepped/resolved an
existing failed run).**

- `start_catalog_workflow` resolves the set from live config and stores it on
  the run: `let config_set = self.resolve_config_set(&definition)?;`
  (`runtime.rs:638`) → written to `WorkflowRun.config_set` (`runtime.rs:655`) →
  persisted (`store.save_run(&run)?`, `runtime.rs:669`). `config_set` is a
  durable `ResolvedConfigSet` (`crates/workflow/core/src/state.rs`).
- `resume_with` loads the run and re-executes the retained step **without**
  calling `resolve_config_set` (`runtime.rs:752-803`); `step_run`/`resume_run`
  both funnel through it (`runtime.rs:742-750`). Retry enforcement reads
  `run.config_set.limits` (`crates/workflow/engine/src/runner.rs:213`) and trips
  exhaustion against the snapshot's `max_retries_per_step`
  (`runner.rs:280-284`). `resolve`/`resolution_options` likewise use the
  snapshot (`runtime.rs:872-945`).

This snapshot-over-live behavior is deliberate for an in-flight run (tests
`step_and_resume_use_snapshot_after_config_set_changes`, `runtime.rs:2898-2953`;
`answer_resolve_and_options_use_snapshot_after_set_deletion`,
`runtime.rs:2955+`). The defect is that it leaves **no supported path** to widen
the budget of an existing run: resuming reuses the snapshot, and the run retains
the failing step, so the edit cannot benefit that run. This branch is
independent of any process restart — a fresh process that correctly loaded `20`
still resumes the persisted `2`.

**Layer B — process-lifetime in-memory config in the long-lived TUI (branch:
user started another run without restarting the TUI).**

- `main.rs` loads config once: `let config = cowboy::load_config(&config_path)?;`
  (`crates/tui/app/src/main.rs:16`), then passes it to `run_tui`.
- `run_tui` builds exactly one runtime from that config and never rebuilds it:
  `let runtime = WorkflowRuntime::new(config.runtime_config(cwd));`
  (`crates/tui/app/src/app.rs:32`). The run loop dispatches every command
  against this single long-lived `runtime` (`commands.rs:181-243`;
  `spawn_start_run` at `commands.rs:245-260`). There is no `load_config` /
  `runtime_config` call anywhere in the loop, input handler, or state — grep of
  `app.rs` and `app/*.rs` finds the production `load_config` only at
  `main.rs:16` (all other matches are in `#[cfg(test)]` modules). So the
  in-memory config is immutable for the process lifetime, and
  `resolve_config_set` reads that frozen copy for every new run too. An edit to
  `config.toml` made while the TUI is running does not reach a new run until the
  process is restarted.

The CLI (`cowboy run` / `cowboy resume`) is a fresh process per invocation, so
Layer B does not apply there; Layer A still does.

## Root cause evidence

The report contains no logs and does not state the exact post-edit command, so
this evidence is grounded in source locations (per the RCA rubric's
logs-unavailable fallback), and is presented per branch. It does **not** assert
which branch the user hit.

**Branch A walkthrough — resuming an existing failed run.** The captured failure
below is the regression test's panic, which drives exactly this path.

1. **Run created at limit = 2.** `start_catalog_workflow` resolves from live
   config (`runtime.rs:638`) and persists
   `run.config_set.limits.max_retries_per_step = 2` (`runtime.rs:655,669`).

2. **Step fails recoverably to 2/2 and the run gives up.** The agent step
   returns replies with no parseable workflow result
   (`Error::NoWorkflowResult`, `crates/workflow/agent/src/error.rs:17-18`;
   `recoverable()` true at `error.rs:48-65`). `retry_step` reads the snapshot
   limit `2` (`runner.rs:213-217`), increments `run.step_retries_used["plan"]`
   per retry (`runner.rs:226-231`), and on the second retry trips
   `retry_exhaustion_error` (`runner.rs:280-284`):
   `config set "default" exhausted retry budget for step "plan": 2/2 retries
   used`. The run persists `Failed`, retaining the failing step
   (`execute_one`, `runner.rs:169-184`).

3. **Config edited to 20; run resumed.** `resume_with` loads the run
   (`runtime.rs:756`), flips `Failed`→`Running` (`runtime.rs:766-785`), and
   re-executes the retained step **without** re-resolving the config set. The
   snapshot's `max_retries_per_step` is still the persisted `2`.

4. **Resume re-fails at the stale 2/2.** The fresh attempt fails recoverably
   again; `run.step_retries_used["plan"]` is already `2` and the snapshot limit
   is `2`, so `retry_exhaustion_error` trips before any retry
   (`runner.rs:279-284`) and the run re-fails with the identical `2/2` message,
   independent of whether the resuming process reloaded config.

Captured failure from the regression test — a **second, freshly-configured
runtime** (a fresh-runtime/config analog, **not** an OS-level process restart)
is built with `max_retries_per_step = 20`, yet the resumed run re-fails:

```
raising max_retries_per_step should let the resumed run retry instead of
re-failing at the stale 2/2: InvalidAction("config set \"default\" exhausted
retry budget for step \"start\": 2/2 retries used; last recoverable error:
recoverable action failure: agent reply did not contain a workflow result")
```

The message matches the report (`step "plan"` in the report vs. `step "start"`
in the minimal workflow; both are the snapshot-exhaustion path at
`runner.rs:280-284`).

**Branch B — source-established behavior (no manual/automated reproduction is
submitted for this branch).** This branch is asserted purely from source, not
from an executed procedure:

- `main.rs:16` is the sole production `load_config` call.
- `app.rs:32` constructs one `WorkflowRuntime` for the whole TUI session.
- The run loop reuses that runtime for every `/run` and `/resume`
  (`commands.rs:181-260`); no code path re-invokes `load_config` or
  `runtime_config` after startup.

From these three facts it follows that a config edit made while the TUI process
is alive cannot reach `resolve_config_set` for any subsequent run until the
process restarts. This is a control-flow conclusion from the cited lines; it is
**not** backed by a submitted end-to-end TUI reproduction, and is labeled
source-established only.

## Reproduction steps

Only **Branch A** is submitted as an executed, deterministic reproduction (the
automated regression test). Branch B is source-established and has no submitted
manual procedure.

Branch A, via the engine's mocked agent backend so no live agent process is
needed:

1. Start a run whose single agent step always returns a reply with no workflow
   result, under a runtime configured with `max_retries_per_step = 2`. The run
   fails: `... exhausted retry budget for step "start": 2/2 retries used`.
2. Construct a **second** `WorkflowRuntime` (a fresh-runtime/config analog)
   pointed at the **same** `state_dir` / `workflow_store`, configured with
   `max_retries_per_step = 20`, whose agent backend would succeed after one
   retry.
3. `resume_run` the failed run through the second runtime.
4. Observe the resume returns `Err(InvalidAction("... 2/2 retries used ..."))`
   instead of completing — the raised budget is ignored because the run reuses
   its durable `config_set` snapshot. This is a same-process two-runtime analog;
   it does not itself execute a new OS process, `load_config`, or a changed
   `config.toml` file.

Captured as the automated regression test below.

## Regression test

- **Test file:** `crates/workflow/engine/src/runtime.rs`
- **Test name:** `runtime::tests::resume_applies_increased_step_retry_budget_to_existing_failed_run`
- **Command:**
  ```
  cargo test -p cowboy-workflow-engine resume_applies_increased_step_retry_budget_to_existing_failed_run
  ```
- **Expected result before the fix:** FAIL. Resuming the already-failed run
  through a **second, freshly-configured** runtime with
  `max_retries_per_step = 20` re-fails with `config set "default" exhausted
  retry budget for step "start": 2/2 retries used`; the test asserts the resumed
  run completes.
- **Scope of what this test proves:** it soundly demonstrates Layer A — a
  second runtime configured for `20`, sharing the persisted store, still resumes
  run state carrying the snapshot `2`. It is a **fresh-runtime/config analog**;
  it does **not** spawn a new Cowboy OS process, call `load_config`, or read a
  changed `config.toml`, and therefore does not by itself exercise Layer B.

## Current failing result

```
running 1 test
test runtime::tests::resume_applies_increased_step_retry_budget_to_existing_failed_run ... FAILED

---- runtime::tests::resume_applies_increased_step_retry_budget_to_existing_failed_run stdout ----
thread '...' panicked at crates/workflow/engine/src/runtime.rs:4798:53:
raising max_retries_per_step should let the resumed run retry instead of
re-failing at the stale 2/2: InvalidAction("config set \"default\" exhausted
retry budget for step \"start\": 2/2 retries used; last recoverable error:
recoverable action failure: agent reply did not contain a workflow result")

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 130 filtered out
```

## Fix constraints

- **Do not resolve this as "just restart."** Restarting refreshes only Layer B
  (new runs in a fresh process). It does **not** rescue an existing run
  (Layer A); the durable snapshot re-fails at `2/2` even in a fresh
  `cowboy resume` process. A correct fix must provide a deliberate way to widen
  an **existing** run's budget.
- **Test/doc only during investigation.** No product code changed; the only
  code edit is the added regression test in `runtime.rs` plus this document.
- **Preserve the intentional in-flight snapshot semantics.** The snapshot exists
  on purpose so a mid-run config change does not perturb a run already advancing
  (`step_and_resume_use_snapshot_after_config_set_changes`, `runtime.rs:2898`;
  `answer_resolve_and_options_use_snapshot_after_set_deletion`,
  `runtime.rs:2955`). A Layer-A fix (e.g. re-resolve the snapshot from live
  config when *resuming a `Failed` run*, or an explicit re-snapshot on
  `resolve`) must not regress those tests.
- **Compare against cumulative counters, don't reset them.** `step_retries_used`
  / `retries_used` are durable and cumulative (`runner.rs:226-231`). A
  budget-widening fix must compare the new limit against the already-accumulated
  counters so the extra budget is genuinely additional, not a reset.
- **Keep the `2` default coherent.** Any change must keep
  `ConfigSetConfig::default()` (`config.rs:63`) and `RunnerLimits::default()`
  (`engine.rs:29`) in agreement — both are the source of the reported `2`.
- **Layer B decision is separate.** Whether the long-lived TUI should hot-reload
  `config.toml` (or surface "config changed; restart to apply") is a distinct UX
  choice from the Layer-A snapshot fix and should be decided explicitly. If a
  Layer-B change is pursued, it should ship with its own end-to-end
  reproduction, since this RCA establishes Layer B from source only.
- **Scope.** This RCA explains only *why the config change is not applied*.
  Whether no-result agent replies should be retried at all is a separate concern
  (sibling RCA
  `docs/plans/missing_frontmatter_errors_from_incomplete_agent_turns/rca.md`).
