# Plan

Add a visible transcript/main-section indication whenever the TUI starts a background task. The current state model has two background task kinds in `crates/tui/app/src/app/state.rs`: `WorkflowExecution` and `RunsList`. Workflow execution tasks already push a submission card through `spawn_card_report_task`, but most of those cards render as neutral or submitted rather than visibly running; `/runs` uses `spawn_runs_list_task` and only updates the status line until the task completes. That leaves at least one background action with no main-section feedback, and the workflow submission cards do not consistently communicate that a task is still running.

Implement the feature in the TUI layer only. Keep workflow runtime, command parser, store, Lua, and agent crates unchanged. The main-section signal should be a transcript card that appears synchronously when a background task is spawned, before the async work finishes. Status-strip metadata can remain compact and should not reintroduce ambiguous task counts that existing tests intentionally reject.

# Changes

- In `crates/tui/app/src/app/state.rs`, make background-task spawning require an immediate transcript presentation for every `BackgroundTaskKind`.
  - Keep the existing `WorkflowExecution` card path, but make the initial submission card visibly communicate running state instead of only neutral submission state.
  - Add the missing immediate card for `RunsList` tasks so `/runs` shows a main-section message like `Loading runs` or `Background task running` while the list operation is still pending.
  - Prefer one shared helper for started-task presentation so future background task kinds cannot skip the main-section indicator accidentally.
- In `crates/tui/app/src/app/state.rs`, update `app_card_status_and_tone` or the nearby card-rendering path to generalize the running tone/icon rule for submitted background-task cards.
  - Today only `Resolve` with `submitted resolve` gets the running icon/tone special case.
  - Extend that contract to the workflow-execution submission cards that represent pending background work: `Run`, `Step`, `Resume`, `Answer`, and `Resolve` when their title suffix indicates a submitted background operation.
  - Do not change completed result cards or `/runs` result-summary cards that do not represent a pending task.
- In `crates/tui/app/src/app/commands.rs`, keep existing spawn semantics and labels, but pass or create presentation text that is safe for transcript rendering.
  - Plain-text run requests may be shown as user-provided command content as they are today, but do not add secrets, absolute private paths, or environment values to new diagnostic copy.
  - `/runs` should get a concise started message without needing a run id.
- Preserve active-run composer behavior from the recent draft-while-running work.
  - The composer should remain editable while workflow execution is active.
  - Plain `Enter` should still be blocked when no prompt is pending.
  - Prompt-answer submission should still clear the prompt and show that the answer task is running.
- Keep the status strip unchanged unless required for consistency.
  - Existing status tests assert that ambiguous background task counts are omitted; the new feature should satisfy the request through the main transcript section instead.

# Tests to be added/updated

- Add `AppState` unit coverage in `crates/tui/app/src/app/state.rs` for workflow-execution background tasks:
  - spawning `spawn_card_report_task` creates one background task and appends a transcript card whose plain text includes a running/submitted-running message;
  - the card uses a running icon/tone contract through the shared card renderer;
  - draining a completed task still appends/applies the final report without removing the started card.
- Add `AppState` unit coverage for `spawn_runs_list_task`:
  - spawning the task immediately appends a main-section transcript card;
  - the card text clearly says the runs list is loading/running;
  - when the future completes, the final `Runs`/`Run` result cards still render as they do today.
- Update card-rendering tests in `crates/tui/app/src/app/state.rs` or adjacent card tests so `Run`, `Step`, `Resume`, `Answer`, and `Resolve` submitted background cards all use the running status icon/tone, while ordinary run-summary cards remain neutral.
- Add or update render-level coverage in `crates/tui/app/src/app/tests.rs`:
  - an active workflow background task renders a visible running/submitted-running message in the transcript/main section;
  - a pending `/runs` background task renders a visible loading/running message in the transcript/main section;
  - the composer and status strip still render without the old ambiguous task-count marker.
- Keep existing composer/status tests passing, especially tests that cover editable active-run drafts, prompt-answer submit behavior, and omission of background-task counts from the status line.

# How to verify

- Run `cargo test -p cowboy app::state::tests`.
- Run `cargo test -p cowboy app::controls::status::tests`.
- Run `cargo test -p cowboy app::controls::composer::tests`.
- Run `cargo test -p cowboy app::tests`.
- Manual TUI smoke test with a controlled local workflow:
  1. In a shell, create an isolated smoke workflow and config:
     ```sh
     SMOKE_ROOT="${TMPDIR:-/tmp}/cowboy-bg-smoke"
     rm -rf "$SMOKE_ROOT"
     mkdir -p "$SMOKE_ROOT/workflows" "$SMOKE_ROOT/state"
     cat > "$SMOKE_ROOT/workflows/slow.lua" <<'LUA'
     local wait = step("wait")
     wait.run = function(ctx)
       return action.command {
         program = "sleep",
         args = { "15" },
         success_status = "slept",
         failure_status = "failed",
         timeout_ms = 20000,
       }
     end
     return workflow("slow-smoke", wait, { description = "Sleeps for TUI background-task smoke tests" })
     LUA
     cat > "$SMOKE_ROOT/config.toml" <<TOML
     state_dir = "$SMOKE_ROOT/state"
     workflow_store = "$SMOKE_ROOT/state/workflow.redb"
     workflow_dirs = ["$SMOKE_ROOT/workflows"]
     TOML
     ```
  2. Launch `cargo run -p cowboy -- --config "$SMOKE_ROOT/config.toml"`.
  3. In the TUI, enter `/run --workflow slow smoke-main-message`.
  4. Expected result while `sleep 15` is still running: the transcript/main section contains the initial workflow task card with a running indicator and `submitted run --workflow slow`; the status strip still omits an ambiguous background-task count.
  5. While the workflow task is still running, type `draft while running` and press plain `Enter`.
  6. Expected result: the draft remains in the composer, no second run starts, and the transcript/main running card from step 4 remains visible in the event history.
  7. Delete the draft text, enter `/runs`, and wait for any result cards.
  8. Expected result: the transcript/main section contains a `Loading runs` or equivalent runs-list background card ordered before the `/runs` result cards, and the status strip still omits an ambiguous background-task count.

# TODO

- [x] TODO-01: Inventory TUI background task spawn sites.
  - Procedure: run `rg "spawn_(card_report|runs_list)_task|spawn_report_task_with_entry|BackgroundTaskKind" crates/tui/app/src/app` and inspect every match.
  - Expected result: every current background task path is accounted for as either workflow execution or runs-list loading, with no unplanned background spawn site left without a main-section presentation path.
  - Observed result: built-in code search for the same pattern found workflow-execution spawn sites in `commands.rs` for run, step, resume, answer, and resolve; the runs-list spawn site in `commands.rs`; and the state-owned spawn/drain/cancel paths in `state.rs`. The implementation routes both `BackgroundTaskKind` values through a shared started-card helper, leaving no discovered background spawn path without main-section presentation.

- [x] TODO-02: Add a shared started-card presentation for workflow execution tasks.
  - Procedure: add or update a focused state/card test such as `workflow_background_task_records_running_card`, then run `cargo test -p cowboy app::state::tests::workflow_background_task_records_running_card`.
  - Expected result: the test passes and proves that `spawn_card_report_task` appends a transcript card whose rendered/plain text communicates a running or submitted-running background task before the async future completes.
  - Observed result: added `workflow_background_task_records_running_card`; `cargo test -p cowboy app::state::tests::workflow_background_task_records_running_card` passed and verified a synchronous `● Run · submitted run --workflow slow` card remains before and after the completed report is drained.

- [x] TODO-03: Add a started-card presentation for runs-list background tasks.
  - Procedure: add or update a focused state/card test such as `runs_list_background_task_records_loading_card`, then run `cargo test -p cowboy app::state::tests::runs_list_background_task_records_loading_card`.
  - Expected result: the test passes and proves that `spawn_runs_list_task` immediately appends a transcript card saying the runs list is loading/running while `background_task_count()` is still nonzero.
  - Observed result: added `runs_list_background_task_records_loading_card`; `cargo test -p cowboy app::state::tests::runs_list_background_task_records_loading_card` passed and verified a synchronous `● Runs · loading runs` card with `Loading runs` body while one background task is pending, followed by the existing final `/runs` result card.

- [x] TODO-04: Generalize running tone/icon rendering for submitted task cards.
  - Procedure: add or update card-rendering assertions for `Run`, `Step`, `Resume`, `Answer`, and `Resolve` cards with submitted-task title suffixes, then run `cargo test -p cowboy app::state::tests` or the narrower card test filter if the assertions live elsewhere.
  - Expected result: submitted background-operation cards render with the running status icon/tone, while ordinary run-summary/result cards without submitted-task suffixes keep their existing non-running rendering.
  - Observed result: added `submitted_background_task_cards_use_running_status`; `cargo test -p cowboy app::state::tests::submitted_background_task_cards_use_running_status` passed and verified submitted `Run`, `Step`, `Resume`, `Answer`, and `Resolve` cards use `●`, while ordinary `Run` and `Resolve` cards keep their previous non-running icons.

- [x] TODO-05: Add render-level coverage for transcript running messages.
  - Procedure: add or update render tests such as `draw_active_background_task_shows_running_message_in_transcript` and `draw_runs_list_background_task_shows_loading_message_in_transcript`, then run `cargo test -p cowboy app::tests`.
  - Expected result: both active workflow execution and pending `/runs` render a visible main-section running/loading message, and existing render assertions still show no ambiguous task-count marker in the status line.
  - Observed result: added `draw_active_background_task_shows_running_message_in_transcript` and `draw_runs_list_background_task_shows_loading_message_in_transcript`; `cargo test -p cowboy app::tests` passed and verified both transcript cards render visibly without the ambiguous `◷` status marker.

- [x] TODO-06: Verify composer and status behavior remain unchanged.
  - Procedure:
    1. Run `cargo test -p cowboy app::controls::composer::tests`.
    2. Run `cargo test -p cowboy app::controls::status::tests`.
  - Expected result: both commands pass, confirming the new main-section indication did not regress active-run draft editing, prompt-answer affordances, or compact status metadata.
  - Observed result: `cargo test -p cowboy app::controls::composer::tests` and `cargo test -p cowboy app::controls::status::tests` passed; after the final warning fix, the same focused composer/status checks were rerun together and passed again.

- [x] TODO-07: Run focused TUI verification and manual smoke check.
  - Procedure:
    1. Run `cargo test -p cowboy app::state::tests` and `cargo test -p cowboy app::tests`.
    2. Create the controlled `slow.lua` smoke workflow and `config.toml` exactly as shown in `How to verify`.
    3. Launch `cargo run -p cowboy -- --config "$SMOKE_ROOT/config.toml"`.
    4. In the TUI, enter `/run --workflow slow smoke-main-message`.
    5. Before the configured `sleep 15` command exits, observe the transcript/main section.
    6. Type `draft while running`, press plain `Enter`, then delete the draft text.
    7. Enter `/runs` and wait for any result cards.
  - Expected result: focused automated tests pass; during the controlled slow workflow, the transcript/main section shows a running workflow task card with `submitted run --workflow slow`, the draft remains after plain `Enter` and no second run starts, `/runs` adds a `Loading runs` or equivalent runs-list background card before its result cards, and the status strip never shows an ambiguous background-task count.
  - Observed result: `cargo test -p cowboy app::state::tests && cargo test -p cowboy app::tests` passed after the final warning fix. The controlled smoke workflow was created under `/tmp/cowboy-bg-smoke`; the TUI showed `● Run · submitted run --workflow slow` before workflow events, retained the `draft while running` composer content after plain `Enter`, then showed `● Runs · loading runs` with `Loading runs` before run-summary cards while the status strip used compact run metadata and did not show `◷`. After `cargo fmt -p cowboy`, final focused state, app, composer, and status tests passed together.
