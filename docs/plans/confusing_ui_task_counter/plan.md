## Plan

Fix the confusing `◷ 1` status-strip UI by removing the internal background task count from user-facing workflow metadata. The RCA at `docs/plans/confusing_ui_task_counter/rca.md` shows that `◷ 1` is `AppState::background_task_count()`, an internal Tokio task count, not workflow progress. The investigator-added repro test `crates/tui/app/src/app/controls/status.rs::app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count` must remain the input contract for the fix and must not be rewritten or replaced.

The safe fix is a clean UI cutover: preserve the useful state icon, step, run id, and workflow metadata; stop rendering `background_task_count()` as a metadata part. Keep `background_task_count()` itself because command conflict handling, cancellation, composer state, and tests still use it as internal app state.

## Changes

- In `crates/tui/app/src/app/controls/chrome.rs`, remove the `Tasks` metadata part and the `status_metadata_text` branch that appends `format!("◷ {}", state.background_task_count())`.
- In `crates/tui/app/src/app/controls/status.rs`, leave `workflow_status_omits_ambiguous_background_task_count` unchanged as the regression input, and update the older contradictory background-task status test so active workflow status omits `◷`.
- In `crates/tui/app/src/app/controls/chrome.rs` tests, update `metadata_uses_shared_icons_and_separator` so it asserts the retained metadata icons and explicitly rejects `◷`/`tasks=`.
- In `crates/tui/app/src/app/card.rs`, remove the test-only `CardMetadata::tasks` helper if it has no product caller, and update the card rendering test to exercise real product metadata only.
- In `crates/tui/app/src/app/tests.rs`, update the full TUI rendering expectation that currently looks for `● · ◷ 1`; it should keep proving the composer/suggestion UI works while no longer expecting the ambiguous counter.
- Do not add runtime logic to TUI rendering and do not derive a replacement counter from `AppState::background_task_count()`.

## Tests to be added/updated

- Keep unchanged: `crates/tui/app/src/app/controls/status.rs::app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count`.
- Update: `crates/tui/app/src/app/controls/status.rs::app::controls::status::tests::background_task_status_adds_task_count` to assert no `◷` suffix for a pending background workflow task; renaming the test is acceptable because this is not the investigator-added repro.
- Update: `crates/tui/app/src/app/controls/chrome.rs::app::controls::chrome::tests::metadata_uses_shared_icons_and_separator` to reject the removed task-count glyph while still checking state, step, run, workflow, and separator behavior.
- Update: `crates/tui/app/src/app/card.rs` card metadata rendering test to remove `CardMetadata::tasks(1)` and the `◷ 1` assertion.
- Update: `crates/tui/app/src/app/tests.rs::app::tests::draw_active_run_composer_keeps_allowed_slash_suggestions` so it asserts the active/running status without `◷ 1` and keeps the existing composer suggestion assertions.
- Do not add broad snapshot tests; the focused status/chrome/card/app tests cover the changed contract.

## How to verify

Run these commands from the repository root after implementation:

1. `cargo test -p cowboy app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count -- --exact`
   - Expected result: the preserved repro test passes and the rendered status line contains `● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature` with no `◷`.
2. `cargo test -p cowboy app::controls::status::tests`
   - Expected result: all status control tests pass with no expectation of `◷ 1`.
3. `cargo test -p cowboy app::controls::chrome::tests`
   - Expected result: chrome metadata tests pass and no test expects `◷`.
4. `cargo test -p cowboy app::tests::draw_active_run_composer_keeps_allowed_slash_suggestions -- --exact`
   - Expected result: the composer rendering path still shows slash suggestions and does not render the ambiguous task counter.
5. `cargo test -p cowboy app::card::tests`
   - Expected result: card rendering tests pass using only retained metadata types.

## TODO

- [x] TODO-01: Remove the background task count from status metadata rendering.
  - Procedure: Edit `crates/tui/app/src/app/controls/chrome.rs` so `status_metadata_text` never appends a `◷`/task-count part from `state.background_task_count()`, and remove the now-unused `MetadataPartKind::Tasks` variant if the compiler reports it unused.
  - Expected result: Rendering an active workflow with one pending background workflow task produces `● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature` and contains no `◷`.
  - Observed result: `crates/tui/app/src/app/controls/chrome.rs` no longer has `MetadataPartKind::Tasks` or the `state.background_task_count()`/`◷` append in `status_metadata_text`; affected status rendering now produces `● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature` without `◷`.

- [x] TODO-02: Update status and chrome metadata tests for the new UI contract.
  - Procedure: Keep `workflow_status_omits_ambiguous_background_task_count` unchanged, update the older contradictory status/chrome tests to reject `◷`, then run `cargo test -p cowboy app::controls::status::tests` and `cargo test -p cowboy app::controls::chrome::tests`.
  - Expected result: both commands pass, and no status/chrome test asserts that `◷ 1` is rendered.
  - Observed result: `cargo test -p cowboy app::controls::status::tests` passed with 6 tests, `cargo test -p cowboy app::controls::chrome::tests` passed with 3 tests, the older status test now rejects `◷`, and `grep` found no `◷ 1`, `MetadataPartKind::Tasks`, or `background_task_status_adds_task_count` in the affected status/chrome/card/app test files.

- [x] TODO-03: Remove obsolete test-only task metadata from card tests.
  - Procedure: In `crates/tui/app/src/app/card.rs`, remove `CardMetadata::tasks` if it is only a test helper, update the card rendering test metadata list and assertion, then run `cargo test -p cowboy app::card::tests`.
  - Expected result: the card tests pass and the card test output no longer includes `◷ 1`.
  - Observed result: `CardMetadata::tasks` and the card test's task metadata entry were removed, `cargo test -p cowboy app::card::tests` passed with 9 tests, and the card assertion now rejects `◷`.

- [x] TODO-04: Update the full TUI composer rendering expectation.
  - Procedure: In `crates/tui/app/src/app/tests.rs`, update `draw_active_run_composer_keeps_allowed_slash_suggestions` so it no longer searches for `● · ◷ 1`, then run `cargo test -p cowboy app::tests::draw_active_run_composer_keeps_allowed_slash_suggestions -- --exact`.
  - Expected result: the test passes, slash command suggestions still render, and the status strip does not include the ambiguous task counter.
  - Observed result: `draw_active_run_composer_keeps_allowed_slash_suggestions` now asserts the running status glyph and rejects `◷`; `cargo test -p cowboy app::tests::draw_active_run_composer_keeps_allowed_slash_suggestions -- --exact` passed with 1 test while retaining slash suggestion assertions.

- [x] TODO-05: Run the preserved regression test as the final narrow proof.
  - Procedure: Run `cargo test -p cowboy app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count -- --exact`.
  - Expected result: the command passes without changing or replacing the investigator-added repro test.
  - Observed result: `cargo test -p cowboy app::controls::status::tests::workflow_status_omits_ambiguous_background_task_count -- --exact` passed with 1 test; the investigator-added `workflow_status_omits_ambiguous_background_task_count` test body was read back unchanged after implementation.
