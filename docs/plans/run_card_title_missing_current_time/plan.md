# Plan: Show current wall-clock time on all card title UIs

Bug-fix plan for the reviewed RCA at
`docs/plans/run_card_title_missing_current_time/rca.md`.

Investigator-added regression test (input to this fix, kept byte-for-byte
unchanged — MUST NOT be rewritten or replaced):
`crates/tui/app/src/app/commands.rs::all_production_card_uis_show_current_wall_clock_time`
(`cargo test -p cowboy --lib all_production_card_uis_show_current_wall_clock_time -- --nocapture`).

## Plan

The RCA establishes there is no shared "stamp the current time on a card"
policy: only the workflow-event renderer (`events.rs::workflow_title_prefix`)
computes a clock value. Four other production card-construction paths omit it —
Run submission cards emit a hardcoded `"00:00:00"` literal, and every other card
emits an empty `title_prefix`.

Fix strategy (per RCA "Fix constraints", all within `crates/tui/app` only):

1. Add one shared current-time helper so every card path agrees on a single
   wall-clock format. The unified production format is exactly `%H:%M`, matching
   the existing workflow-event path (`events.rs::format_workflow_title_prefix`,
   whose `wall_clock` segment is `local_timestamp.format("%H:%M")`). Both paths
   MUST derive their format from a single shared source so they cannot drift. No
   placeholder is reintroduced, and no seconds-based format is accepted anywhere.
2. Route all four in-scope production card paths through the helper:
   - Action helpers in `commands.rs` (replace the `"00:00:00"` literals and the
     empty `[]` prefixes across all eight action paths).
   - `AppState::push_card` (`state.rs`).
   - `AppState::spawn_runs_list_task` (`state.rs`).
   - `render_pending_prompt_lines` direct `Card::new` (`state.rs`).
3. Prove the fix with focused behavioral tests at the acceptance boundary
   (rendered card title lines), not shell text searches. Every card assertion
   MUST require a leading title prefix, validate it as exactly the current
   `%H:%M` wall-clock value, and compare the exact non-time title remainder.

`chrono` is already a dependency of the `cowboy` crate; no new dependency is
required. No runtime-crate changes: card timestamping stays in the TUI.

### Test placement and helper accessibility

`assert_card_title_current_time` is a `#[cfg(test)]` helper private to the
`crates/tui/app/src/app/commands.rs` test module (the same module that owns the
investigator repro test). To keep every card test able to call it **without
introducing a new cross-module visibility seam**, all new card tests
(TODO-02, TODO-03, TODO-04) live in the `commands.rs` test module, matching the
investigator test's access pattern. The three `state.rs` render paths are still
exercised from there: `push_card` and `spawn_runs_list_task` are called through a
constructed `AppState` (the `commands.rs` tests already build one via
`test_runtime_state()`), and the pending-prompt card is rendered by calling
`crate::app::state::render_pending_prompt_lines(prompt, width)` — already invoked
this way from the `commands.rs` module in the investigator test (Category 4). No
test is placed in `state.rs`, so the private helper is always in scope.

### Ownership boundary between TODO-02 and TODO-04 (no duplicated work)

- The shared strict assertion helper `assert_card_title_current_time` is
  **created once, in TODO-02**, together with the production `commands.rs`
  changes and one new parameterized action-boundary test covering all eight
  action paths. TODO-02 does **not** touch the three pre-existing card tests.
- **TODO-04 consumes** the helper created in TODO-02 (it does not re-implement
  it). TODO-04 owns exactly two things: migrating the three pre-existing card
  tests to the helper, and adding the negative strict-helper-contract test. No
  card test or helper edit is claimed by both TODOs.

### TODO-09 is a guard spanning the whole change

`TODO-09` is not a normal sequential step; it is a **byte-for-byte guard** around
the investigator repro test. It has three phases: (1) it starts **first** and
records the pre-edit baseline digest; (2) it remains open while `TODO-01`..
`TODO-05` execute; (3) it **completes last**, after the post-edit digest is
compared and the temporary artifacts are removed. It is listed first in the TODO
section to make its "start first" phase explicit, but it is only checked done
after TODO-05.

### Revision history

- R1 → R2: replaced `cargo build`/shell-`grep` checks with behavioral tests.
- R2 → R3: single-filter cargo commands; corrected pending-prompt icon to `◔`.
- R3 → R4: restored exact backticked `TODO-01`..`TODO-03` texts; retired the
  duplicate `TODO-06`/`TODO-07`/`TODO-08`; tightened to exactly `%H:%M`.
- R4 → R5: clean TODO-02/TODO-04 ownership boundary; TODO-09 ordered first.
- R5 → R6: reframed `TODO-09` accurately as a start-first / complete-last guard
  and replaced its shell-substitution/`&&` procedure with raw `:raw` source
  extraction, fixed artifact names, and `sha256sum` hashing; pinned the
  placement of every new card test to the `commands.rs` test module so the
  private helper is always accessible (TODO-03).
- R6 → R7 (this revision): replaced the forbidden `rm`/`ls` cleanup in TODO-09
  with supported tools — an Eval (Python) cell using `Path.unlink(missing_ok=True)`
  to remove both temporary artifacts and `Path.exists()` to assert their absence;
  the authoritative stage-evidence arrays are emitted under their
  renderer-consumed field names in the returned frontmatter.
- R7 → R8 (this revision): corrected the pending-prompt path to store an
  event-derived wall-clock prefix in `PendingPrompt` (render no longer recomputes
  time per redraw), made the pending-prompt test assert the exact full remainder,
  added `TODO-10` for a deterministic repeated-render stability test, and
  unchecked `TODO-03`/`TODO-05` for rework. See "Evidence procedure fidelity".
- R8 → R9 (this revision): reopened the byte-for-byte guard as a new distinct id
  `TODO-11` (the checked `TODO-09` did not cover the R8/R9 `commands.rs` edits);
  gave `event_wall_clock_prefix` explicit creation + verification ownership under
  `TODO-03` (not silently under the checked `TODO-01`); corrected the false
  no-allocation claim (`.to_string()` still clones per render — the win is
  removing the live clock lookup/format/drift, not the clone); and made TODO-10's
  oracle independent of the production helper (expected `%H:%M` computed inline
  via plain chrono from a fixed event timestamp).

### Evidence procedure fidelity

Re-run each TODO's evidence using its plan procedure **exactly** — no broader or
shell substitutions (the prior submission was rejected for these):
- TODO-09: raw `Read`/`Write` extraction plus single `sha256sum`/`cmp` calls and
  Eval cleanup; do not use `sed`, redirection, or chained shell pipelines.
- TODO-01: the **Grep tool** for the `"%H:%M"` count, not shell `grep`.
- TODO-02 / TODO-03 / TODO-04: the exact focused test names named in each
  procedure, not the broad `cargo test -p cowboy --lib card_wall_clock_` group.
- Record every executed command as a command record with the correct
  `procedure_index`; never cite a command (e.g. `git show HEAD`) that has no
  recorded procedure step.

## Changes

### New shared helper (production code)

- `crates/tui/app/src/app/events.rs`:
  - Introduce a single wall-clock format source, exactly
    `const CARD_WALL_CLOCK_FORMAT: &str = "%H:%M";`, and a private
    `format_wall_clock(ts: DateTime<FixedOffset>) -> String` that both the event
    path and the new card helper call.
  - Refactor `format_workflow_title_prefix` so its `wall_clock` segment uses
    `format_wall_clock(local_timestamp)`; the `(+elapsed)` suffix branch and the
    event-time source (`event.timestamp.with_timezone(&Local)`) are unchanged.
  - Add `pub(in crate::app) fn current_wall_clock_prefix() -> String` returning
    `format_wall_clock(chrono::Local::now().fixed_offset())`.
  - Add `pub(in crate::app) fn event_wall_clock_prefix(event: &WorkflowEvent) -> String`
    returning `format_wall_clock(event.timestamp.with_timezone(&Local).fixed_offset())`
    — the wall-clock prefix derived once from the `WaitingForInput` event's own
    timestamp (stable), not from `now()`. This is the value the pending-prompt
    path stores and renders.

### `crates/tui/app/src/app/commands.rs` (action-submission cards, Path 1)

Use `current_wall_clock_prefix` and replace the prefix argument in every
`spawn_card_report_task` call:

- `spawn_start_run` (~L251): `["00:00:00".to_string()]` →
  `[current_wall_clock_prefix()]`.
- `spawn_start_run_stepwise` (~L270): same replacement.
- `spawn_start_run_with_workflow` (~L295): same replacement.
- `spawn_start_run_with_workflow_stepwise` (~L320): same replacement.
- `spawn_step_run` (~L339): `[]` → `[current_wall_clock_prefix()]`.
- `spawn_resume_run` (~L358): `[]` → `[current_wall_clock_prefix()]`.
- `spawn_answer_task` (~L384): `[]` → `[current_wall_clock_prefix()]`.
- `resolve_run` `Some(status)` branch (~L465): `[]` →
  `[current_wall_clock_prefix()]`.

### `crates/tui/app/src/app/state.rs` (Paths 2, 3, 4)

- `AppState::push_card` (~L949): `title_prefix: Vec::new()` →
  `title_prefix: vec![current_wall_clock_prefix()]`.
- `AppState::spawn_runs_list_task` (~L1106): `title_prefix: Vec::new()` →
  `title_prefix: vec![current_wall_clock_prefix()]`.
- Pending-prompt card (stored, not recomputed per render): add a
  `title_prefix: String` field to `PendingPrompt` and a `title_prefix()`
  accessor; populate it in the `WaitingForInput` event handler (`state.rs`
  ~L1370) with `event_wall_clock_prefix(event)` so the prefix is computed once
  from the event timestamp. Change `render_pending_prompt_lines` (~L114-122) to
  `.title_prefix(prompt.title_prefix().to_string())`. This removes the render-time
  **clock lookup, `%H:%M` formatting, and timestamp drift** — the displayed time is
  fixed by the stored event value and is byte-identical across repeated renders.
  It does **not** eliminate all allocation: `.to_string()` still clones the stored
  prefix each render (the `Card` builder owns a `String`), matching the existing
  `TranscriptEntry::Card` render path, which also materializes owned prefix
  strings per render. Eliminating that clone would require reworking the `Card`
  title-prefix API to borrow, which is out of scope for this bug fix; the
  correctness win is removing the live clock, not the clone. The card keeps its
  `status_icon("waiting")` = `◔`, so the rendered title is
  `<HH:MM> · ◔ Waiting for input · …`. Rationale:
  `controls/transcript.rs::bounded_tail_visual_rows` (~L381-388) calls
  `render_pending_prompt_lines` on every redraw; a live `current_wall_clock_prefix()`
  there would drift across a minute boundary and re-read/re-format the clock each
  frame, unlike the stored `TranscriptEntry::Card` paths.
- Add the needed `use` for `current_wall_clock_prefix` from the `events` module.

## Tests to be added/updated

All new tests live in the `crates/tui/app/src/app/commands.rs` test module (see
"Test placement and helper accessibility") so the private
`assert_card_title_current_time` helper is in scope.

Design note — shared strict assertion. The helper
`assert_card_title_current_time(rendered, before, after, expected_remainder)` —
**created in TODO-02, consumed by TODO-04** — that:

1. Takes the rendered card's first line and splits on the first `" · "`.
2. Fails if there is no leading prefix segment (prefix is **required**, never
   optional).
3. Fails unless the prefix equals exactly the current wall-clock time — one of
   the two `%H:%M` candidates formed from the `before`/`after` `chrono::Local`
   instants captured around the action (only these two minute-granularity values
   are accepted; this covers a minute rollover without accepting a seconds-based
   or otherwise non-`%H:%M` value). Because accepted values are 5-character
   `HH:MM`, the 8-character `00:00:00` placeholder is deterministically rejected
   at any time of day.
4. Fails unless the remainder after the prefix exactly equals
   `expected_remainder` (e.g. `● Run · submitted run`).

All newly added tests share the filterable name prefix `card_wall_clock_` so a
single positional `cargo test` filter can select the whole group.

Tests by owning TODO:

- Keep the investigator repro test
  `all_production_card_uis_show_current_wall_clock_time` unchanged; primary
  acceptance signal (Categories 1-4 must flip FAIL→PASS; Category 0 control stays
  PASS). It independently accepts `%H:%M`/`%H:%M:%S`; the production `%H:%M`
  output is one of its accepted candidates.
- TODO-02 (new): add the strict helper and one new **parameterized**
  action-boundary test `card_wall_clock_action_cards_show_current_time` that
  drives all eight action paths (plain Run, `--step`, `--workflow`,
  `--step --workflow`, Step, Resume, Answer, Resolve — the Answer path seeded via
  a `WaitingForInput` prompt as in
  `pending_prompt_answer_fallback_spawns_answer_task_and_clears_target`, ~L1764)
  and asserts each rendered card title via `assert_card_title_current_time`.
  TODO-02 does not modify the three pre-existing tests.
- TODO-03 (new): three state-path behavioral tests, all in the `commands.rs` test
  module:
  - `card_wall_clock_push_card_shows_current_time`: build an `AppState` via
    `test_runtime_state()`, call `push_card` (e.g. `push_card("Notice", …)`),
    render the last transcript entry, assert a present, exact-`%H:%M` prefix.
  - `card_wall_clock_runs_loading_shows_current_time`: submit `/runs`, render the
    loading card (before background drain), assert a present, exact-`%H:%M`
    prefix with remainder `● Runs · loading runs`.
  - `card_wall_clock_pending_prompt_shows_current_time`: seed a `WaitingForInput`
    event (run id `pending-run`, step `approve`, one choice), take the resulting
    `PendingPrompt`, render via `crate::app::state::render_pending_prompt_lines`,
    and assert via `assert_card_title_current_time` a present, exact-`%H:%M`
    prefix with the **exact full remainder**
    `◔ Waiting for input · ↳ approve · ▶ pending-run` (not a `starts_with`
    check — a missing, corrupted, or extended `↳ step · ▶ run` metadata tail must
    fail). Because the prefix is now stored from the event timestamp, capture the
    `before`/`after` candidates around applying the event, not around the render.
  - `card_wall_clock_pending_prompt_prefix_is_stable_across_repeated_renders`
    (TODO-10): construct the `WaitingForInput` event with an explicit fixed UTC
    timestamp via `WorkflowEvent::with_timing(run_id, fixed_utc, None, None, kind)`,
    apply it to build the `PendingPrompt`, and render it twice — deterministic, no
    sleep. Derive the expected `%H:%M` **independently of the production helper**:
    inline in the test compute `fixed_utc.with_timezone(&chrono::Local).format("%H:%M").to_string()`
    (plain chrono, not `event_wall_clock_prefix`). Then assert (1) the first
    render's title prefix equals that independently computed expected prefix and
    the exact full remainder `◔ Waiting for input · ↳ approve · ▶ pending-run`;
    (2) the second render's full title line is byte-identical to the first. A live
    `now()` implementation in the render path would render the current minute
    instead of the fixed event minute and fail assertion (1); a per-render clock
    read that ticks across a boundary would fail assertion (2). Using a fixed
    timestamp (not `now()`) keeps the independent oracle deterministic regardless
    of when the test runs.
- TODO-04 (migrate + negative): migrate the three pre-existing card tests to
  consume the helper, updating their expected titles to the time-free remainder:
  - `plain_request_submission_renders_initial_input_as_card` (~L652) — remainder
    `● Run · submitted run`; via the helper-wired `assert_last_entry_is_card`.
  - `slash_run_variants_render_initial_input_as_cards` (~L892) — the four Run
    variant remainders.
  - `run_control_submissions_render_action_cards` (~L930) — the Step, Resume, and
    Resolve remainders.
  Plus add `card_wall_clock_helper_rejects_missing_and_nontime_prefix`: a unit
  test of the helper's negative contract using `std::panic::catch_unwind` (or an
  inner `Result`-returning core the panic wrapper calls), asserting the helper
  rejects (a) a title with no leading time prefix (e.g. `◔ Notice`) and (b) a
  title whose prefix is a non-`%H:%M` value (the deterministic placeholder
  `00:00:00` and a status-icon prefix `● Runs`).
- TODO-01 behavioral non-regression proof — the existing event-formatting tests
  must remain passing, unchanged:
  `formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed`
  (asserts `06:23 (+00:04:56)` from a fixed event time — proves event path uses
  event time and keeps the `(+elapsed)` suffix), plus
  `run_completed_title_uses_active_elapsed_duration`,
  `run_completed_title_falls_back_to_wall_clock_elapsed_duration`,
  `run_completed_title_clamps_negative_wall_clock_elapsed_duration`, and
  `run_completed_title_omits_elapsed_when_no_elapsed_source_exists`.

## How to verify

1. `cargo test -p cowboy --lib all_production_card_uis_show_current_wall_clock_time -- --nocapture`
   — repro test passes; per-category report shows Categories 0-4 all PASS, no panic.
2. `cargo test -p cowboy --lib events::tests` — the five event-formatting tests
   above pass unchanged (behavioral non-regression for the event path).
3. `cargo test -p cowboy --lib card_wall_clock_` — the whole new behavioral group
   (single positional filter, shared prefix) passes.
4. `cargo test -p cowboy --lib` — full `cowboy` unit suite passes, including the
   migrated existing card tests.
5. `cargo clippy -p cowboy --all-targets -- -D warnings` and
   `cargo build -p cowboy` — no compiler or Clippy warnings.
6. Manual smoke (optional, matches RCA reproduction): `cargo run`, submit a plain
   request, confirm the title begins with the current wall-clock time (e.g.
   `14:07 · ● Run · submitted run`) not `00:00:00`; confirm `/help`, `/runs`, and
   a `WaitingForInput` prompt card titles also begin with the current time.

## TODO

Two byte-for-byte guards protect the investigator repro test. `TODO-09`
(completed) guarded the R1-R7 edit round. `TODO-11` is the **active** guard for
this R9 rework: because R8/R9 edit `commands.rs` again (the pending-prompt test's
exact remainder and the new `TODO-10` test), a fresh baseline must be captured
**before** those edits and compared **after** `TODO-05`. `TODO-11`'s phase 1 runs
first in this rework; the remaining unchecked TODOs execute in numeric order; and
`TODO-11`'s phase 3 runs last, after the new `TODO-05` execution.

- [x] TODO-09: Prove the investigator repro test is byte-for-byte unchanged via checksum.
  - Procedure (guard; phase 1 runs before TODO-01, phase 3 runs after TODO-05):
    - Phase 1 — baseline, before any edit:
      1. Read `crates/tui/app/src/app/commands.rs` to find the complete syntactic
         bounds (opening `#[tokio::test]`/`async fn` line through the matching
         closing brace) of `all_production_card_uis_show_current_wall_clock_time`;
         note that exact line range.
      2. Re-read that exact line range with the `:raw` selector
         (`crates/tui/app/src/app/commands.rs:<start>-<end>:raw`) to obtain
         verbatim source bytes with no snapshot header or line-number prefixes.
      3. Write those raw bytes to the fixed temporary artifact
         `/tmp/cowboy_repro_before.rs` using the Write tool.
      4. Record the baseline digest with a single Bash call:
         `sha256sum /tmp/cowboy_repro_before.rs`.
    - Phases 1→3 gap: execute TODO-01 through TODO-05.
    - Phase 3 — compare and clean up, after TODO-05:
      5. Re-locate the function by name (line numbers may have shifted from
         inserted tests) and re-read its complete range with `:raw`, exactly as
         in steps 1-2.
      6. Write those raw bytes to the fixed temporary artifact
         `/tmp/cowboy_repro_after.rs` with the Write tool, then run
         `sha256sum /tmp/cowboy_repro_after.rs`.
      7. Compare the two hash fields (equal ⇒ unchanged). Optionally corroborate
         with a single Bash call `cmp /tmp/cowboy_repro_before.rs /tmp/cowboy_repro_after.rs`.
      8. Remove both artifacts through an Eval (Python) cell:
         `from pathlib import Path` then
         `for p in ("/tmp/cowboy_repro_before.rs", "/tmp/cowboy_repro_after.rs"): Path(p).unlink(missing_ok=True)`.
      9. Verify absence in the same/an Eval cell:
         `assert not Path("/tmp/cowboy_repro_before.rs").exists() and not Path("/tmp/cowboy_repro_after.rs").exists()`.
  - Expected result: The phase-1 and phase-3 SHA-256 digests of the raw
    repro-test function bytes are identical (and `cmp` reports no difference),
    proving the investigator test was not modified; and after the Eval
    `Path.unlink` removal, the Eval `Path.exists` check confirms **both**
    `/tmp/cowboy_repro_before.rs` and `/tmp/cowboy_repro_after.rs` are absent
    (the assertion holds).
  - Observed result: Phase-1 baseline `sha256sum /tmp/cowboy_repro_before.rs`
    = `15dc9add9071de85d83ab0217b861aaaf611733aa6f9668d2b4d7a7e2c09160b`
    (repro-test bounds `commands.rs:669-889`). Phase-3, after TODO-05, the
    function moved to `commands.rs:727-947`; `sha256sum /tmp/cowboy_repro_after.rs`
    = `15dc9add9071de85d83ab0217b861aaaf611733aa6f9668d2b4d7a7e2c09160b` — identical
    digests, and `cmp` reported no difference (`IDENTICAL`). The Eval
    `Path.unlink(missing_ok=True)` removed both artifacts and the `Path.exists`
    assertion held (`both artifacts removed`). Investigator test unchanged.

- [x] TODO-01: Add shared `current_wall_clock_prefix()` helper and unify the wall-clock format.
  - Procedure:
    1. In `crates/tui/app/src/app/events.rs`, add
       `const CARD_WALL_CLOCK_FORMAT: &str = "%H:%M"` and a private
       `format_wall_clock`, route `format_workflow_title_prefix`'s wall-clock
       segment through it, and add
       `pub(in crate::app) fn current_wall_clock_prefix() -> String`.
    2. Source-structure inspection (separate from tests): using the Grep tool,
       search `crates/tui/app/src/app/events.rs` for the literal `"%H:%M"` and
       count matches.
    3. Behavioral non-regression: run
       `cargo test -p cowboy --lib events::tests`.
  - Expected result: Step 2 returns exactly one `"%H:%M"` match — the shared
    `CARD_WALL_CLOCK_FORMAT` const — establishing one format source consumed by
    both `format_workflow_title_prefix` and `current_wall_clock_prefix` (source
    evidence, not inferred from tests). Step 3 passes all existing
    event-formatting tests unchanged:
    `formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed` still
    asserts `06:23 (+00:04:56)` (event path uses the event timestamp, not `now`,
    and preserves the `(+elapsed)` suffix) and the four `run_completed_title_*`
    tests still pass (behavioral evidence).
  - Observed result: Grep for `"%H:%M"` in `events.rs` returned exactly one match
    — `const CARD_WALL_CLOCK_FORMAT: &str = "%H:%M";` (line 415) — the single
    format source consumed by `format_wall_clock`, which both
    `format_workflow_title_prefix` and `current_wall_clock_prefix` call.
    `cargo test -p cowboy --lib events::tests` passed 31/0 unchanged, including
    `formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed` and
    the four `run_completed_title_*` tests.

- [x] TODO-02: Timestamp all action-submission cards in `commands.rs`.
  - Procedure: Replace the four `["00:00:00".to_string()]` prefixes and the four
    empty `[]` prefixes (per Changes) with `[current_wall_clock_prefix()]`.
    Create the shared `assert_card_title_current_time` helper (exact-`%H:%M`
    contract) in the `commands.rs` test module and add one new parameterized
    action-boundary test `card_wall_clock_action_cards_show_current_time`
    covering all eight action paths. Do **not** modify the three pre-existing
    card tests here (they are migrated in TODO-04). Run:
    `cargo test -p cowboy --lib card_wall_clock_action_cards_show_current_time`.
  - Expected result: The new parameterized test passes and asserts, for each of
    the eight action cards (plain Run, `--step`, `--workflow`,
    `--step --workflow`, Step, Resume, Answer, Resolve), a **present** leading
    prefix equal to exactly the captured current `%H:%M` wall clock (never
    `00:00:00`, never a seconds value, never absent) plus the exact non-time
    remainder.
  - Observed result: All eight `spawn_card_report_task` prefixes now use
    `[current_wall_clock_prefix()]` and `assert_card_title_current_time` was
    created in the `commands.rs` test module.
    `cargo test -p cowboy --lib card_wall_clock_action_cards_show_current_time`
    passed (1/0); each of the eight action cards (plain Run, `--step`,
    `--workflow`, `--step --workflow`, Step, Resume, Answer, Resolve) asserted a
    present current-`%H:%M` prefix plus its exact remainder.

- [x] TODO-03: Timestamp `push_card`, `spawn_runs_list_task`, and the pending-prompt card in `state.rs`.
  - Procedure:
    1. Create the new event helper (this TODO owns it; do not modify the completed
       `TODO-01`): add `pub(in crate::app) fn event_wall_clock_prefix(event: &WorkflowEvent) -> String`
       in `events.rs` returning
       `format_wall_clock(event.timestamp.with_timezone(&Local).fixed_offset())`,
       reusing the shared `format_wall_clock`/`CARD_WALL_CLOCK_FORMAT` from
       `TODO-01`. Verify it with the Grep tool: confirm `events.rs` still has
       exactly one `"%H:%M"` literal (the helper adds no second format source) and
       that `event_wall_clock_prefix` routes through `format_wall_clock`.
    2. Edit `state.rs`: `push_card` and `spawn_runs_list_task` use
       `vec![current_wall_clock_prefix()]`.
    3. Edit the pending-prompt path so its prefix is stored, not recomputed per
       render: add a `title_prefix: String` field (plus `title_prefix()` accessor)
       to `PendingPrompt`; populate it in the `WaitingForInput` handler
       (`state.rs` ~L1370) with `event_wall_clock_prefix(event)`; change
       `render_pending_prompt_lines` to `.title_prefix(prompt.title_prefix().to_string())`
       with the `◔` icon retained and no clock lookup in the render path. Update
       existing `PendingPrompt { .. }` literals (e.g. the `state.rs` markdown
       test) to set the new field.
    4. Add `card_wall_clock_push_card_shows_current_time`,
       `card_wall_clock_runs_loading_shows_current_time`, and
       `card_wall_clock_pending_prompt_shows_current_time` to the `commands.rs`
       test module. The pending-prompt test asserts the **exact full remainder**
       `◔ Waiting for input · ↳ approve · ▶ pending-run` via
       `assert_card_title_current_time` (candidates captured around applying the
       event). Run these three commands separately:
       `cargo test -p cowboy --lib card_wall_clock_push_card_shows_current_time`;
       `cargo test -p cowboy --lib card_wall_clock_runs_loading_shows_current_time`;
       `cargo test -p cowboy --lib card_wall_clock_pending_prompt_shows_current_time`.
  - Expected result: All three tests pass; each asserts a present, exact
    current-`%H:%M` prefix on the `push_card` card, the `/runs` loading card
    (remainder `● Runs · loading runs`), and the pending-prompt card (exact
    remainder `◔ Waiting for input · ↳ approve · ▶ pending-run`). The
    pending-prompt prefix is stored on `PendingPrompt` and the render path
    performs no clock lookup or `%H:%M` formatting (it still clones the stored
    prefix via `.to_string()`, matching the other `Card` render paths).
  - Observed result: Added `pub(in crate::app) fn event_wall_clock_prefix(event)`
    in `events.rs` routing through the shared `format_wall_clock`; Grep confirmed
    `events.rs` still has exactly one `"%H:%M"` literal (line 415,
    `CARD_WALL_CLOCK_FORMAT`). `push_card` and `spawn_runs_list_task` use
    `vec![current_wall_clock_prefix()]`. `PendingPrompt` gained a
    `title_prefix: String` field + `title_prefix()` accessor; the
    `WaitingForInput` handler populates it with `event_wall_clock_prefix(event)`
    and `render_pending_prompt_lines` now uses
    `.title_prefix(prompt.title_prefix().to_string())` (no render-time clock
    lookup, `◔` icon retained). Ran each focused test separately:
    `card_wall_clock_push_card_shows_current_time` (1/0),
    `card_wall_clock_runs_loading_shows_current_time` (remainder
    `● Runs · loading runs`, 1/0), and
    `card_wall_clock_pending_prompt_shows_current_time` (exact remainder
    `◔ Waiting for input · ↳ approve · ▶ pending-run`, 1/0).

- [x] TODO-04: Update existing card tests to tolerate the dynamic wall-clock prefix.
  - Procedure:
    1. Consume the `assert_card_title_current_time` helper created in TODO-02 (do
       not re-implement it): wire `assert_last_entry_is_card` to it and migrate
       the three pre-existing card tests
       (`plain_request_submission_renders_initial_input_as_card`,
       `slash_run_variants_render_initial_input_as_cards`,
       `run_control_submissions_render_action_cards`) to pass the time-free
       remainder plus captured `before`/`after` instants.
    2. Add `card_wall_clock_helper_rejects_missing_and_nontime_prefix` proving the
       helper's negative contract using deterministic invalid fixtures
       (`◔ Notice` = missing prefix; `00:00:00` and `● Runs` = non-`%H:%M`
       prefix); under the exact-`%H:%M` rule `00:00:00` can never equal a
       candidate at any time, including midnight.
    3. Run the negative test and the three migrated existing tests:
       `cargo test -p cowboy --lib card_wall_clock_helper_rejects_missing_and_nontime_prefix`;
       `cargo test -p cowboy --lib plain_request_submission_renders_initial_input_as_card`;
       `cargo test -p cowboy --lib slash_run_variants_render_initial_input_as_cards`;
       `cargo test -p cowboy --lib run_control_submissions_render_action_cards`.
  - Expected result: The strict helper rejects both a missing prefix (`◔ Notice`)
    and a non-time prefix (`00:00:00`, `● Runs`) — a title with no timestamp
    cannot pass — and all three migrated existing card tests pass with the
    dynamic exact-`%H:%M` prefix.
  - Observed result: `assert_last_entry_is_card` was rewired to the strict helper
    and the three pre-existing card tests migrated to the time-free remainder plus
    captured `before`/`after` instants (further callers at the resolve/answer/error
    sites and the `state.rs` helper were migrated too, since they now carry a time
    prefix). `card_wall_clock_helper_rejects_missing_and_nontime_prefix` passed,
    rejecting `◔ Notice` (missing prefix), `00:00:00`, and `● Runs` (non-`%H:%M`).
    The three migrated tests passed with the dynamic exact-`%H:%M` prefix.

- [x] TODO-10: Prove the pending-prompt title timestamp is stable across repeated renders.
  - Procedure:
    1. Add `card_wall_clock_pending_prompt_prefix_is_stable_across_repeated_renders`
       to the `commands.rs` test module: construct a `WaitingForInput`
       `WorkflowEvent` with an explicit fixed UTC `timestamp` via
       `WorkflowEvent::with_timing(run_id, fixed_utc, None, None, kind)`, apply it
       to build the `PendingPrompt`, and render it twice via
       `crate::app::state::render_pending_prompt_lines` with no sleep between.
    2. Compute the expected prefix **independently of the production helper**:
       inline `let expected = fixed_utc.with_timezone(&chrono::Local).format("%H:%M").to_string();`
       (plain chrono, not `event_wall_clock_prefix`). Assert (a) the first render's
       title prefix equals `expected` and its full remainder is exactly
       `◔ Waiting for input · ↳ approve · ▶ pending-run`; (b) the second render's
       full title line is byte-identical to the first.
    3. Run
       `cargo test -p cowboy --lib card_wall_clock_pending_prompt_prefix_is_stable_across_repeated_renders`.
  - Expected result: The test passes: render 1's prefix equals the independently
    computed `%H:%M` with the exact full remainder, and render 2 is byte-identical.
    A live `current_wall_clock_prefix()` in the render path (recomputing `now()`)
    would render the current minute instead of the fixed event minute and fail
    assertion (a); a per-render clock read that ticks across a boundary would fail
    assertion (b). The oracle is derived from the fixed timestamp by plain chrono,
    so a consistently wrong `event_wall_clock_prefix` cannot make both sides agree.
  - Observed result: Added
    `card_wall_clock_pending_prompt_prefix_is_stable_across_repeated_renders`
    using `WorkflowEvent::with_timing("pending-run", fixed_utc, None, None, kind)`
    with `fixed_utc` = epoch 1_600_000_000, rendered twice with no sleep. The
    oracle `fixed_utc.with_timezone(&chrono::Local).format("%H:%M")` is computed
    inline (not via `event_wall_clock_prefix`).
    `cargo test -p cowboy --lib card_wall_clock_pending_prompt_prefix_is_stable_across_repeated_renders`
    passed (1/0): render 1's prefix equals the independent `%H:%M` with exact
    remainder `◔ Waiting for input · ↳ approve · ▶ pending-run`, and render 2's
    title line is byte-identical to render 1's.

- [x] TODO-05: Verify the fix end to end.
  - Procedure: Run
    `cargo test -p cowboy --lib all_production_card_uis_show_current_wall_clock_time -- --nocapture`,
    then `cargo test -p cowboy --lib`, then
    `cargo clippy -p cowboy --all-targets -- -D warnings`.
  - Expected result: The repro test passes with Categories 0-4 all PASS and no
    panic; the full unit suite passes (including all migrated and new card tests,
    the exact-remainder pending-prompt test, and the TODO-10 stability test);
    Clippy reports no warnings. (After this, TODO-11 phase 3 runs to close the
    active unchanged-repro guard for this rework.)
  - Observed result:
    `cargo test -p cowboy --lib all_production_card_uis_show_current_wall_clock_time -- --nocapture`
    passed with Categories 0-4 all PASS (prefix `17:53`), no panic.
    `cargo test -p cowboy --lib` passed 336/0 (2 ignored), including the migrated
    card tests, the exact-remainder pending-prompt test, and the TODO-10 stability
    test. `cargo clippy -p cowboy --all-targets -- -D warnings` reported no
    warnings. `cargo fmt` is clean for the edited lines.

- [x] TODO-11: Prove the investigator repro test is byte-for-byte unchanged across the R9 rework via a fresh checksum guard.
  - Procedure (active guard for the R9 edits; phase 1 runs first, phase 3 last):
    - Phase 1 — baseline, before any R9 edit:
      1. Read `crates/tui/app/src/app/commands.rs` to find the complete syntactic
         bounds (opening `#[tokio::test]`/`async fn` line through the matching
         closing brace) of `all_production_card_uis_show_current_wall_clock_time`;
         note that exact line range.
      2. Re-read that exact range with the `:raw` selector
         (`crates/tui/app/src/app/commands.rs:<start>-<end>:raw`) for verbatim
         bytes with no snapshot header or line-number prefixes.
      3. Write those raw bytes to the fixed temporary artifact
         `/tmp/cowboy_repro_r9_before.rs` using the Write tool.
      4. Record the baseline digest with a single Bash call:
         `sha256sum /tmp/cowboy_repro_r9_before.rs`.
    - Phases 1→3 gap: execute TODO-03, TODO-10, and TODO-05 (all edits to
      `commands.rs`/`state.rs`/`events.rs`).
    - Phase 3 — compare and clean up, after TODO-05:
      5. Re-locate the function by name (line numbers may have shifted) and
         re-read its complete range with `:raw`, exactly as in steps 1-2.
      6. Write those raw bytes to `/tmp/cowboy_repro_r9_after.rs` with the Write
         tool, then run `sha256sum /tmp/cowboy_repro_r9_after.rs`.
      7. Compare the two hash fields (equal ⇒ unchanged). Optionally corroborate
         with a single Bash call
         `cmp /tmp/cowboy_repro_r9_before.rs /tmp/cowboy_repro_r9_after.rs`.
      8. Remove both artifacts through an Eval (Python) cell:
         `from pathlib import Path` then
         `for p in ("/tmp/cowboy_repro_r9_before.rs", "/tmp/cowboy_repro_r9_after.rs"): Path(p).unlink(missing_ok=True)`.
      9. Verify absence in the same/an Eval cell:
         `assert not Path("/tmp/cowboy_repro_r9_before.rs").exists() and not Path("/tmp/cowboy_repro_r9_after.rs").exists()`.
  - Expected result: The phase-1 and phase-3 SHA-256 digests of the raw
    repro-test function bytes are identical (and `cmp` reports no difference),
    proving the R9 rework did not modify the investigator test; after the Eval
    `Path.unlink` removal, the `Path.exists` check confirms both
    `/tmp/cowboy_repro_r9_before.rs` and `/tmp/cowboy_repro_r9_after.rs` are absent.
  - Observed result: Phase-1 baseline `sha256sum /tmp/cowboy_repro_r9_before.rs`
    = `15dc9add9071de85d83ab0217b861aaaf611733aa6f9668d2b4d7a7e2c09160b`
    (repro-test bounds `commands.rs:727-947`). Phase-3, after TODO-05, the bounds
    were unchanged (new tests inserted after the function);
    `sha256sum /tmp/cowboy_repro_r9_after.rs`
    = `15dc9add9071de85d83ab0217b861aaaf611733aa6f9668d2b4d7a7e2c09160b` —
    identical digests, and `cmp` reported no difference (`IDENTICAL`). The Eval
    `Path.unlink(missing_ok=True)` removed both artifacts and the `Path.exists`
    assertion held (`both R9 artifacts removed`). Investigator test unchanged.
