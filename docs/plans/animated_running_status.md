# Plan

Make the TUI status strip show a live running animation instead of the static `●` while a workflow is actively running. Keep workflow runtime, command parser, store, Lua, ACP, and engine behavior unchanged.

Design choice: add a small internal workspace crate at `crates/tui/animation` with package name `cowboy-tui-animation`. This crate should own reusable terminal animation primitives: frame sequences, deterministic advancement, wraparound, and reset. It should not own TUI state, redraw scheduling, styling, status metadata composition, terminal lifecycle, or workflow behavior. The app crate remains responsible for deciding when the running animation is active and when the terminal needs a redraw.

This crate split is justified by repository structure, not by any additional preserved user feedback. The workspace already has narrow app-facing support crates under `crates/tui`, such as `cowboy-tui-terminal`, and the root manifest exposes those support crates through `[workspace.dependencies]` path entries. A similarly narrow `cowboy-tui-animation` crate keeps animation state reusable without making `crates/tui/app/src/app/controls/chrome.rs` the owner of generic animation primitives.

The current status strip is rendered through `crates/tui/app/src/app/controls/status.rs`, which delegates text construction to `status_metadata_text` in `crates/tui/app/src/app/controls/chrome.rs`. `chrome::status_icon("running")` currently returns the static `●`; status tests assert that exact glyph for active runs. The event loop in `crates/tui/app/src/app.rs` redraws only after workflow events, background-task changes, input, mouse, resize, or initial draw. It polls input every 100ms but does not mark the frame dirty on idle poll timeouts, so a real animation needs a small redraw tick while the displayed state is running.

Use a deterministic, terminal-safe spinner for active running state: a single-display-width Unicode frame sequence such as `⠋`, `⠙`, `⠹`, `⠸`, `⠼`, `⠴`, `⠦`, `⠧`, `⠇`, `⠏`. The idle, waiting, retrying, completed, failed, and cancelled icons keep their existing semantic glyphs unless tests expose a direct conflict. Width-based metadata dropping must continue to treat the status icon as a fixed one-cell part, so compact status rows such as `↳ step`, `▶ run`, and `⎇ workflow` retain their current priority order.

The TODO list preserves the previously reviewed `TODO-01` through `TODO-06` task text. Newly introduced crate-split work is appended as `TODO-07` and `TODO-08`; implementers may complete `TODO-07` and `TODO-08` before the app integration TODOs when following dependency order.

# Changes

- Add the new `cowboy-tui-animation` crate under `crates/tui/animation`.
  - Add `crates/tui/animation/Cargo.toml` using workspace edition, Rust version, and license fields, matching sibling TUI crate conventions.
  - Add `crates/tui/animation/src/lib.rs` with a narrow crate-level responsibility comment: reusable terminal animation primitives for Cowboy UI surfaces.
  - Keep the crate independent from `ratatui`, `crossterm`, workflow crates, app state, config, and terminal lifecycle code.
  - Expose a small deterministic API, for example a `Spinner`/`FrameCycle` type plus a `RUNNING_STATUS_FRAMES` constant or constructor, with methods to read the current frame, advance one step with wraparound, and reset.
  - Keep frames as `&'static str` and avoid allocation during steady-state frame reads/advances.
- Wire the crate into the workspace.
  - Add `crates/tui/animation` to `[workspace].members` and `[workspace].default-members` in the root `Cargo.toml`.
  - Add `cowboy-tui-animation = { path = "crates/tui/animation" }` under `[workspace.dependencies]`.
  - Add `cowboy-tui-animation.workspace = true` to `crates/tui/app/Cargo.toml`.
  - Prefer moving the existing `unicode-width = "0"` app dependency to a workspace dependency if both the new animation crate tests and the app need it; otherwise keep width checks in crate tests without broadening runtime dependencies unnecessarily.
- Update `crates/tui/app/src/app/state.rs` with app-owned animation state for the status strip.
  - Store a `cowboy_tui_animation` frame cycle or a frame index compatible with the crate API in `AppState`, initialized to the first running frame.
  - Add methods such as `status_animation_active()`, `advance_status_animation() -> bool`, and `status_animation_frame() -> &'static str` or equivalent app-private accessors so rendering stays deterministic in unit tests.
  - Return `true` from `advance_status_animation()` only when `display_state()` is `running`; if the state is not running, reset or leave the frame stable and return `false` so the event loop does not redraw needlessly.
  - Keep `display_state()` semantics unchanged: pending prompts still display `waiting`, active workflow execution still displays `running`, and terminal run states still display completed/failed/cancelled.
- Update `crates/tui/app/src/app/controls/chrome.rs` so status metadata can render an animated running icon.
  - Keep `status_icon(status: &str)` as the static semantic icon helper for non-animated contexts and existing card/status contracts.
  - Add a focused app helper such as `status_icon_for_state(status: &str, running_frame: &str) -> &str` or have `status_metadata_text` choose `state.status_animation_frame()` only for `running`.
  - Use the new animation crate as the source of running frames; do not duplicate the frame sequence locally in `chrome.rs`.
  - Use the existing static helper for every non-running state.
- Update `crates/tui/app/src/app/controls/status.rs` tests for the animated contract.
  - Replace exact `●` assertions for active running rows with deterministic first-frame expectations or frame-advance assertions.
  - Preserve existing assertions that waiting renders `◔`, idle renders `○`, retrying renders `↻`, and the compact metadata omits ambiguous background-task counts.
  - Add narrow-width coverage proving metadata is still dropped in the order workflow, run, step, leaving the animated icon intact.
- Update `crates/tui/app/src/app.rs` to drive redraws while the status animation is active.
  - Reuse the existing 100ms `event::poll(Duration::from_millis(100))` cadence or name a constant such as `STATUS_ANIMATION_TICK` near the run loop.
  - When the poll times out with no terminal event, call `state.advance_status_animation()` and mark the `DrawScheduler` dirty only when it returns `true`.
  - Do not redraw on every idle poll when the app is idle, waiting for input, completed, failed, or cancelled.
  - Preserve existing draw triggers for workflow events, background-task completion, input, mouse, resize, and initial render.
- Keep styling unchanged.
  - `status::line` should still style the whole status strip via `style_for_run_state(&state.display_state())`.
  - Do not introduce color cycling, background flashing, or transcript cards for this feature; the request is specifically about making the running status bar indicator more vivid.

# Tests to be added/updated

- Add unit coverage in `crates/tui/animation/src/lib.rs`.
  - The running frame sequence is nonempty and contains at least two distinct frames.
  - The frame cycle returns the first frame initially, advances deterministically, and wraps back to the first frame after a full cycle.
  - Reset returns the cycle to the first frame.
  - Every selected running frame has display width one.
  - The crate has no dependency on `ratatui`, `crossterm`, or Cowboy workflow crates.
- Add manifest/workspace coverage where this repository already uses source-level manifest assertions.
  - Verify the root workspace lists `crates/tui/animation` as a member and default member.
  - Verify `[workspace.dependencies]` exposes `cowboy-tui-animation` by path.
  - Verify `crates/tui/app/Cargo.toml` depends on `cowboy-tui-animation.workspace = true`.
- Add unit coverage in `crates/tui/app/src/app/state.rs` for the app-owned animation state.
  - Running state advances through at least two different crate-provided frames.
  - Non-running states do not advance and do not request redraw.
  - Returning from running to waiting/completed stops further animation redraw requests.
- Update or add unit coverage in `crates/tui/app/src/app/controls/chrome.rs`.
  - `status_icon("running")` keeps the static semantic `●` contract for non-animated callers.
  - `status_metadata_text` uses the state's current crate-provided running frame while preserving non-running icons.
  - The status metadata path does not duplicate the spinner frame sequence locally in the app crate.
- Update `crates/tui/app/src/app/controls/status.rs` tests.
  - `active_run_status_renders_compact_metadata` should assert the first animation frame plus `↳ implement`, `▶ 170dc431`, and `⎇ agent/00-feature`.
  - Add a second assertion after advancing the frame that the same metadata renders with a different running glyph.
  - Keep the tests that assert waiting status, background-task-count omission, and narrow metadata dropping.
- Add or update render-loop coverage in `crates/tui/app/src/app/tests.rs` only if the current test seams can exercise the idle-poll timeout without real terminal input.
  - Preferred coverage: a focused `DrawScheduler` or run-loop helper test proving no-event poll ticks mark dirty while running and not while idle/waiting.
  - If the run loop cannot be tested without a larger refactor, keep the automated proof at the animation crate, `AppState`, and status-renderer level and rely on the manual smoke check for the live redraw cadence.

# How to verify

- Run `cargo test -p cowboy-tui-animation`. Expected result: the new animation crate's frame-cycle, wraparound, reset, width, and dependency-boundary tests pass.
- Run `cargo test -p cowboy app::state::tests::status_animation_advances_only_while_running -- --exact` after adding the focused `AppState` animation test. Expected result: the test passes and proves running advances frames while idle/waiting states do not request redraw.
- Run `cargo test -p cowboy app::controls::chrome::tests`. Expected result: static icon contracts pass, status metadata uses the crate-provided running frame, and the app crate does not define a duplicate spinner sequence.
- Run `cargo test -p cowboy app::controls::status::tests`. Expected result: status-strip rendering shows deterministic animated running frames and preserves compact metadata behavior.
- Run `cargo test -p cowboy app::tests` if `app.rs` or render-loop seams receive automated coverage. Expected result: app-level rendering/event-loop tests pass without new flakes or sleeps.
- Run `cargo test -p cowboy` as the focused app regression pass.
- Run `cargo test --workspace --no-run` after the workspace membership change. Expected result: every workspace package, including `cowboy-tui-animation`, resolves and builds as part of the workspace.
- Manual TUI smoke check:
  1. Build the binary with `cargo build -p cowboy`.
  2. Launch `cargo run -p cowboy -- --config demo-config.toml` or another non-sensitive local config.
  3. Start a workflow that stays active long enough to observe the status strip, for example a workflow with a command step that sleeps for several seconds.
  4. Expected result while the run is active: the leftmost status-strip glyph visibly cycles through the selected spinner frames at a steady cadence, while the step, short run id, and workflow metadata remain stable and readable.
  5. Let the run finish or cancel it with `Ctrl+C`.
  6. Expected result after completion/cancellation/waiting-for-input: the status-strip glyph stops animating and shows the existing semantic final-state icon.

# TODO

- [x] TODO-01: Add deterministic status animation state to `AppState`.
  - Procedure:
    1. Add an `AppState` field for the status animation frame index or crate-backed frame cycle.
    2. Add `status_animation_active()`, `status_animation_frame()`, and `advance_status_animation() -> bool` or equivalent app-private methods.
    3. Add a focused `AppState` unit test that starts a run, records the initial frame, advances the animation twice, then switches to a waiting or completed event and advances again.
  - Expected result: while `display_state()` is `running`, frame values change and `advance_status_animation()` returns `true`; after the state is waiting/completed, the method returns `false` and no further frame changes are rendered.
  - Observed result: `AppState` now owns a `FrameCycle`, exposes active/frame/advance methods, advances only for running display state, resets outside running state, and `cargo test -p cowboy app::state::tests::status_animation_advances_only_while_running -- --exact` passed with 1 test.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-01","subject":"Add deterministic status animation state to `AppState`.","source":"implementer","procedure":{"kind":"manual","steps":["Add `status_animation: FrameCycle` to `AppState` and initialize it with `FrameCycle::running_status()`.","Add `status_animation_active()`, `status_animation_frame()`, and `advance_status_animation() -> bool` on `AppState`.","Add `status_animation_advances_only_while_running` covering initial frame, two running advances, and waiting-state reset/no-redraw behavior.","Run `cargo test -p cowboy app::state::tests::status_animation_advances_only_while_running -- --exact`."]},"expected_result":"while `display_state()` is `running`, frame values change and `advance_status_animation()` returns `true`; after the state is waiting/completed, the method returns `false` and no further frame changes are rendered.","observed_result":"The state test passed with 1 test; the implementation advances through distinct frames while running and returns false/reset when waiting.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-02: Add animated running icon helpers in `chrome.rs`.
  - Procedure:
    1. Add a helper in `crates/tui/app/src/app/controls/chrome.rs` that selects the current animated running frame for `running` and delegates all other states to the existing `status_icon` helper.
    2. Source running frames from `cowboy-tui-animation` rather than duplicating the frame sequence in the app crate.
    3. Add unit tests for running-frame selection, non-running fallback, and one-cell display width for the crate-provided frames.
  - Expected result: the static `status_icon("running") == "●"` test still passes, the animated helper returns deterministic crate-provided frames for running state only, and non-running status icons keep their existing glyphs.
  - Observed result: `status_icon_for_state` returns the supplied running frame only for `running`, `status_icon("running")` remains `●`, chrome tests import `RUNNING_STATUS_FRAMES` from the animation crate, and `cargo test -p cowboy app::controls::chrome::tests` passed with 6 tests.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-02","subject":"Add animated running icon helpers in `chrome.rs`.","source":"implementer","procedure":{"kind":"manual","steps":["Add `status_icon_for_state(status, running_frame)` in `chrome.rs`.","Source running frames from `cowboy_tui_animation::RUNNING_STATUS_FRAMES` in tests and keep `chrome.rs` free of a local spinner sequence.","Add tests for running-frame selection, non-running fallback, one-cell frame width, and static icon contracts.","Run `cargo test -p cowboy app::controls::chrome::tests`."]},"expected_result":"the static `status_icon(\"running\") == \"●\"` test still passes, the animated helper returns deterministic crate-provided frames for running state only, and non-running status icons keep their existing glyphs.","observed_result":"Chrome tests passed with 6 tests; static icons stayed unchanged and animated selection uses crate-provided frames only for running.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-03: Render the animated frame in status metadata without changing metadata priority.
  - Procedure:
    1. Update `status_metadata_text(state, width)` to use the state's current animation frame for the fixed status part when `state.display_state()` is `running`.
    2. Keep step, run, and workflow metadata construction and `remove_lowest_priority_part` order unchanged.
    3. Update status/chrome tests for full-width and narrow-width active run status text.
  - Expected result: active running status text starts with the expected spinner frame and still includes `↳ step`, `▶ short-run-id`, and `⎇ workflow` when width allows; narrow rows still drop workflow before run before step, not the spinner.
  - Observed result: `status_metadata_text` now feeds `state.status_animation_frame()` through `status_icon_for_state`; full-width status renders the first spinner frame with step/run/workflow metadata, and narrow status keeps the spinner while dropping workflow first. `cargo test -p cowboy app::controls::status::tests` passed with 6 tests.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-03","subject":"Render the animated frame in status metadata without changing metadata priority.","source":"implementer","procedure":{"kind":"manual","steps":["Update `status_metadata_text(state, width)` to use `state.status_animation_frame()` for the fixed running status part.","Leave step, run, workflow metadata construction and `remove_lowest_priority_part` priority order unchanged.","Update status and chrome tests for full-width and narrow-width active run status text.","Run `cargo test -p cowboy app::controls::status::tests`."]},"expected_result":"active running status text starts with the expected spinner frame and still includes `↳ step`, `▶ short-run-id`, and `⎇ workflow` when width allows; narrow rows still drop workflow before run before step, not the spinner.","observed_result":"Status tests passed with 6 tests; full-width rows include spinner, step, short run id, and workflow, while width 42 keeps spinner, step, and run after dropping workflow.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-04: Drive animation redraw ticks from the TUI event loop.
  - Procedure:
    1. In `crates/tui/app/src/app.rs`, after the existing `event::poll` timeout returns `false`, call `state.advance_status_animation()`.
    2. Mark the `DrawScheduler` dirty only when the method returns `true`.
    3. Preserve all existing terminal-event branches and existing workflow/background-task drain behavior.
  - Expected result: with no user input and no new workflow events, an active running workflow still redraws the status strip at the poll cadence; idle, waiting, completed, failed, and cancelled states do not redraw only for animation.
  - Observed result: the no-event poll branch now calls `tick_status_animation`, which marks the draw scheduler dirty only when `advance_status_animation()` returns true. The app test `status_animation_tick_marks_dirty_only_while_running` passed inside `cargo test -p cowboy app::tests`.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-04","subject":"Drive animation redraw ticks from the TUI event loop.","source":"implementer","procedure":{"kind":"manual","steps":["Add `tick_status_animation(state, draw_scheduler)` in `app.rs`.","Call the helper when `event::poll(Duration::from_millis(100))` returns `false`.","Leave existing workflow-event, background-task, paste, key, mouse, resize, and ignored-event branches intact.","Run `cargo test -p cowboy app::tests`."]},"expected_result":"with no user input and no new workflow events, an active running workflow still redraws the status strip at the poll cadence; idle, waiting, completed, failed, and cancelled states do not redraw only for animation.","observed_result":"App tests passed with 33 tests; `status_animation_tick_marks_dirty_only_while_running` proves idle/waiting states stay clean and running marks dirty on a no-event tick.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-05: Update focused status and app tests for the animated behavior.
  - Procedure:
    1. Update exact running-glyph expectations in `crates/tui/app/src/app/controls/status.rs` from `●` to the deterministic initial spinner frame.
    2. Add an assertion that advancing the animation changes the rendered running glyph while preserving the metadata suffix.
    3. Add app-level redraw coverage if a small test seam is available without sleeps or terminal I/O; otherwise document the live redraw proof in the manual smoke result.
  - Expected result: focused tests fail against the old static `●` implementation and pass only when the status strip can render at least two running frames with stable metadata.
  - Observed result: status tests now assert `RUNNING_STATUS_FRAMES[0]` and `[1]` with stable metadata; app-level redraw coverage was added through `tick_status_animation`. An initial `cargo test -p cowboy app::tests` run failed on one remaining static `●` assertion, then after updating that app test the focused app tests passed with 33 tests.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-05","subject":"Update focused status and app tests for the animated behavior.","source":"implementer","procedure":{"kind":"manual","steps":["Update status-control exact running-glyph expectations from `●` to `RUNNING_STATUS_FRAMES[0]`.","Add a status-control assertion that `advance_status_animation()` changes the rendered running glyph to `RUNNING_STATUS_FRAMES[1]` while preserving metadata.","Add app-level redraw coverage through `status_animation_tick_marks_dirty_only_while_running` without sleeps or terminal I/O.","Run `cargo fmt`.","Run `cargo test -p cowboy app::tests` and observe the stale static-glyph assertion failure.","Update the stale app test expectation and run `cargo fmt`.","Run `cargo test -p cowboy app::tests`."]},"expected_result":"focused tests fail against the old static `●` implementation and pass only when the status strip can render at least two running frames with stable metadata.","observed_result":"The first app-focused run failed on a stale `●` assertion in `draw_active_run_composer_keeps_allowed_slash_suggestions`; after updating it to `RUNNING_STATUS_FRAMES[0]`, app tests passed with 33 tests and status tests prove two running frames with stable metadata.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-06: Run automated and manual verification.
  - Procedure:
    1. Run `cargo test -p cowboy-tui-animation`.
    2. Run `cargo test -p cowboy app::state::tests::status_animation_advances_only_while_running -- --exact`.
    3. Run `cargo test -p cowboy app::controls::chrome::tests`.
    4. Run `cargo test -p cowboy app::controls::status::tests`.
    5. Run `cargo test -p cowboy app::tests` if app-level coverage changed.
    6. Run `cargo test -p cowboy`.
    7. Run `cargo test --workspace --no-run`.
    8. Perform the manual TUI smoke check from `How to verify` with a long-enough running workflow.
  - Expected result: all focused tests pass; all workspace packages resolve after adding `cowboy-tui-animation`; the manual TUI run shows a visibly cycling status glyph while running and a stable semantic glyph after the run leaves running state.
  - Observed result: all automated checks passed, LSP diagnostics reported a clean workspace, and the manual TUI smoke run with a `sleep 6` command workflow showed the status glyph cycling through `⠙`, `⠹`, `⠸`, `⠼`, `⠴`, `⠦`, `⠧`, `⠇`, `⠏`, `⠋` while running, then settling on `✓ · ↳ wait · ▶ 3ec678d8 · ⎇ sleep-status` after completion.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-06","subject":"Run automated and manual verification.","source":"implementer","procedure":{"kind":"manual","steps":["Run `cargo test -p cowboy-tui-animation`.","Run `cargo test -p cowboy app::state::tests::status_animation_advances_only_while_running -- --exact`.","Run `cargo test -p cowboy app::controls::chrome::tests`.","Run `cargo test -p cowboy app::controls::status::tests`.","Run `cargo test -p cowboy app::tests`.","Run `cargo test -p cowboy`.","Run `cargo test --workspace --no-run`.","Run LSP workspace diagnostics.","Run `cargo build -p cowboy`.","Launch `target/debug/cowboy --config /tmp/cowboy-animation-smoke/config.toml` with a temporary `sleep 6` workflow.","Submit `/run --workflow sleep-status observe animation` in the TUI.","Observe the status glyph cycle through spinner frames while the command workflow is running.","Observe the final status glyph stop on `✓` after the workflow completes."]},"expected_result":"all focused tests pass; all workspace packages resolve after adding `cowboy-tui-animation`; the manual TUI run shows a visibly cycling status glyph while running and a stable semantic glyph after the run leaves running state.","observed_result":"Focused tests, `cargo test -p cowboy`, workspace no-run, and LSP diagnostics passed; manual TUI logs showed repeated spinner frames while running and `✓ · ↳ wait · ▶ 3ec678d8 · ⎇ sleep-status` after completion.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-07: Create the `cowboy-tui-animation` workspace crate.
  - Procedure:
    1. Add `crates/tui/animation/Cargo.toml` using package name `cowboy-tui-animation`, workspace edition, workspace Rust version, workspace license, and a concise description.
    2. Add `crates/tui/animation/src/lib.rs` with the animation-frame API and crate responsibility documentation.
    3. Add focused unit tests for frame sequence non-emptiness, multiple distinct frames, deterministic advance, wraparound, reset, and one-cell display width.
    4. Run `cargo test -p cowboy-tui-animation`.
  - Expected result: the new crate builds and its unit tests pass; it exposes reusable animation primitives without depending on `ratatui`, `crossterm`, workflow crates, app state, config, or terminal lifecycle code.
  - Observed result: `cowboy-tui-animation` now exposes `RUNNING_STATUS_FRAMES` and `FrameCycle`; crate tests cover nonempty/distinct frames, deterministic advance, wraparound, reset, one-cell width, dependency boundaries, and manifest registration. `cargo test -p cowboy-tui-animation` passed with 6 tests.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-07","subject":"Create the `cowboy-tui-animation` workspace crate.","source":"implementer","procedure":{"kind":"manual","steps":["Add `crates/tui/animation/Cargo.toml` using package name `cowboy-tui-animation`, workspace edition, workspace Rust version, workspace license, and a concise description.","Add `crates/tui/animation/src/lib.rs` with `RUNNING_STATUS_FRAMES`, `FrameCycle`, current/advance/reset/frame accessors, and crate responsibility documentation.","Add unit tests for frame sequence non-emptiness, multiple distinct frames, deterministic advance, wraparound, reset, one-cell display width, and dependency boundaries.","Run `cargo test -p cowboy-tui-animation`."]},"expected_result":"the new crate builds and its unit tests pass; it exposes reusable animation primitives without depending on `ratatui`, `crossterm`, workflow crates, app state, config, or terminal lifecycle code.","observed_result":"Animation crate tests passed with 6 tests and the crate contains no production dependency on `ratatui`, `crossterm`, workflow crates, app state, config, or terminal lifecycle code.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-08: Wire `cowboy-tui-animation` into the workspace and app manifests.
  - Procedure:
    1. Add `crates/tui/animation` to root `[workspace].members` and `[workspace].default-members`.
    2. Add `cowboy-tui-animation = { path = "crates/tui/animation" }` to root `[workspace.dependencies]`.
    3. Add `cowboy-tui-animation.workspace = true` to `crates/tui/app/Cargo.toml`.
    4. Add or update manifest/source assertions that prove the workspace and app dependency wiring.
    5. Run `cargo test -p cowboy-tui-animation` and `cargo test --workspace --no-run`.
  - Expected result: Cargo resolves the new internal crate as a workspace member and the app can depend on it through the workspace dependency table.
  - Observed result: root workspace members/default-members and workspace dependencies now include `crates/tui/animation`/`cowboy-tui-animation`; the app manifest depends on `cowboy-tui-animation.workspace = true`; source manifest assertions verify the wiring; `cargo test --workspace --no-run` completed successfully.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-08","subject":"Wire `cowboy-tui-animation` into the workspace and app manifests.","source":"implementer","procedure":{"kind":"manual","steps":["Add `crates/tui/animation` to root `[workspace].members` and `[workspace].default-members`.","Add `cowboy-tui-animation = { path = \"crates/tui/animation\" }` to root `[workspace.dependencies]`.","Add `cowboy-tui-animation.workspace = true` to `crates/tui/app/Cargo.toml`.","Add manifest/source assertions proving root workspace and app dependency wiring.","Run `cargo test --workspace --no-run`."]},"expected_result":"Cargo resolves the new internal crate as a workspace member and the app can depend on it through the workspace dependency table.","observed_result":"Workspace no-run completed successfully and manifest assertions in `cowboy-tui-animation` prove the root/app dependency wiring.","applicability":"applicable","match":"matched","comparisons":[]}
