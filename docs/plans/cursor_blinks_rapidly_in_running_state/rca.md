## Bug behavior

While a workflow run is in the `running` state, the composer input cursor blinks
very rapidly instead of at a normal, steady blink rate. When the TUI is idle (no
active run) the same cursor blinks normally. Per the cumulative user direction,
the required behavior is that the cursor does not blink while the run is
`running`.

The blink is not a single wrong final cursor state; it is the application
repeatedly hiding and then re-showing the cursor on every animation-driven
production redraw. The behavior is grounded by a focused test that puts
`AppState` into the `running` state, drives the status animation through the
production redraw path across multiple consecutive frames, and records the
cursor visibility transitions the backend receives. Before the fix the recorded
transitions are `Hidden -> Shown` on every frame — the toggle that constitutes
the reported rapid blink.

## Root cause

Three existing behaviors combine only in the `running` state:

1. `crates/tui/app/src/app/controls/composer.rs::render` unconditionally requests
   a visible terminal cursor whenever the composer accepts edits:

   ```rust
   if state.composer_accepts_edits() {
       set_input_cursor(frame, area, visible_height, &rendered);
   }
   ```

   `crates/tui/app/src/app/state.rs::composer_accepts_edits` always returns
   `true`, so every completed frame requests a cursor position, which Ratatui
   turns into a terminal cursor shown at the composer input position.

2. The configured cursor shape is a blinking block:
   `crates/tui/terminal/src/lib.rs::tui_input_cursor_style` returns
   `SetCursorStyle::BlinkingBlock`, set once on terminal entry.

3. In the `running` state the status animation is active
   (`crates/tui/app/src/app/state.rs::status_animation_active` returns `true`
   when `pending_prompt` is `None` and `run_state == "running"`). The main loop
   in `crates/tui/app/src/app.rs::run_loop` therefore marks the frame dirty on
   every ~100 ms idle poll tick via `tick_status_animation`, so the TUI redraws
   roughly ten times per second.

Each of those running-state redraws goes through
`crates/tui/app/src/app.rs::draw_cursor_safe_production_frame`, which was added
to fix a separate Windows cursor-flash bug
(`docs/plans/windows_cursor_flashes_during_running_animation/`). It calls
`terminal.hide_cursor()` before painting; then Ratatui's post-frame path
re-shows and repositions the cursor because the composer requested a cursor
position. In `ratatui-core-0.1.2` `terminal/render.rs::apply_buffer_with_cursor`
(lines 288-303) the per-frame sequence is:

```rust
match cursor_position {
    None => self.hide_cursor()?,
    Some(position) => {
        self.show_cursor()?;
        self.set_cursor_position(position)?;
    }
}
```

So every running-state production frame performs `hide_cursor` (our pre-draw
seam) then `show_cursor` + `set_cursor_position` (Ratatui, because the composer
requested a cursor). Across the ~10 Hz animation redraws this is a continuous
`Hidden -> Shown -> Hidden -> Shown ...` toggle on top of the terminal's own
blink timer; many terminals additionally reset the blink phase whenever the
cursor is shown or moved. Both effects make the blinking-block composer cursor
blink very rapidly. Idle state performs no redraws (the draw scheduler stays
clean between events), so the cursor sits stable there — which is why the symptom
is specific to the `running` state.

The fix boundary is the running-state animation frame: while
`status_animation_active()` holds, no production frame may re-show the cursor.
Because the configured style is `BlinkingBlock`, merely avoiding the toggle while
leaving the cursor visible would still blink at the terminal's rate and would
reintroduce the Windows flash that the pre-draw hide fixed; therefore the
intended product behavior is that the cursor stays hidden throughout running
animation frames. See Fix constraints for the explicit product decision, the
state gate, the pure placement/layout tests that must keep passing, the required
reconciliation with `draw_places_cursor_in_active_run_draft_input` (the "draft
editable while a run is active" affordance), and the separate Windows anti-flash
contract (`status_animation_redraw_hides_cursor_before_painting_changed_cell`).

## Reproduction steps

Manual:

1. Start a Cowboy TUI run so the run state becomes `running` and the status
   animation is active.
2. Keep focus in the composer input box.
3. Observe the composer input cursor: it blinks very rapidly compared with the
   normal, steady blink seen when the TUI is idle.

Repository-grounded (mirrors the production `run_loop` redraw path and records
visibility transitions, not just the final state):

1. Construct `AppState` and apply a `RunStarted` workflow event so `run_state`
   becomes `running` and `status_animation_active()` is `true`.
2. Render an initial production frame through
   `draw_cursor_safe_production_frame` using a cursor-visibility probe backend
   that records every `show_cursor`/`hide_cursor` call and each visibility
   transition.
3. Repeatedly advance the running status animation with `tick_status_animation`
   and re-render through `draw_cursor_safe_production_frame`, exactly as the
   100 ms idle poll path does, for several consecutive frames.
4. Inspect the recorded transitions: each animation-driven frame emits a
   `Hidden -> Shown` transition (`show_cursor` is called once per frame). That
   repeated toggle is the rapid blink. A fix must drive the recorded
   `show_cursor` count to `0` across all running animation frames.

## Regression test

- Test file path: `crates/tui/app/src/app/tests.rs`
- Test name: `running_state_does_not_toggle_cursor_visibility_across_animation_frames`
- Command:
  `cargo test -p cowboy --lib app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames`
- Expected failure before the fix: the test enters the running state, renders one
  initial production frame plus three animation-driven production frames through
  `draw_cursor_safe_production_frame`, and records cursor visibility via the
  extended `CursorVisibilityProbeBackend`. It asserts `cursor_show_calls == 0`
  and zero `Hidden -> Shown` transitions across those frames. Before the fix the
  first assertion fails with `cursor_show_calls == 4` (left `4`, right `0`) and
  the recorded transitions are
  `[Hidden, Shown, Hidden, Shown, Hidden, Shown, Hidden, Shown]`, proving the
  cursor is re-shown on every animation redraw. The expected fixed behavior is
  `cursor_show_calls == 0` with no `Hidden -> Shown` transition while the running
  animation is active.

## Current failing result

Command run:

```text
cargo test -p cowboy --lib app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames
```

Observed output:

```text
running 1 test
test app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames ... FAILED

failures:

---- app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames stdout ----

thread 'app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames' (1531193) panicked at crates/tui/app/src/app/tests.rs:471:5:
assertion `left == right` failed: running-state frames re-showed the cursor 4 time(s) across 4 animation-driven redraws (transitions: [Hidden, Shown, Hidden, Shown, Hidden, Shown, Hidden, Shown]); each show after the pre-draw hide is one blink cycle, so the blinking-block cursor blinks rapidly. Running-state frames must keep the composer cursor hidden throughout the animation.
  left: 4
 right: 0
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 311 filtered out; finished in 0.01s

error: test failed, to rerun pass `-p cowboy --lib`
```

Source-labeled evidence provenance. Repository-relative paths are used
throughout; `<repo-root>` is the local checkout root, generalized to avoid
leaking a private absolute path. The tested source is uncommitted, so it is
identified by a content-addressed workspace snapshot (base revision plus the
dirty-file list and per-file content/diff SHA-256), not by a committed revision.

The authoritative machine-readable copy of these records is published as a
repository-relative file inside this work directory:
`docs/plans/cursor_blinks_rapidly_in_running_state/investigator_records.json`.
That file is reviewer-addressable across sessions (plain filesystem path, not a
session-scoped `local://` artifact). The JSON reproduced below is byte-consistent
with that file and included here for readability.

```json
{
  "workspace_snapshot": {
    "id": "SNAP-01",
    "base_revision": "8818e9e2c6dce8e618f5da848b70fd62b9e6be12",
    "dirty": true,
    "dirty_files": [
      { "status": "M", "path": "crates/tui/app/src/app/tests.rs" },
      { "status": "??", "path": "docs/plans/cursor_blinks_rapidly_in_running_state/" }
    ],
    "tested_source": {
      "path": "crates/tui/app/src/app/tests.rs",
      "content_sha256": "29062dcdda67e8a922247640e43e53001311a059642a282cbda56ea0649e58cb",
      "content_sha256_verified_by": "INV-CMD-05",
      "unstaged_diff_sha256": "4d92a6b5f7e39648ff47d08190c0b823c30f15137b363ce17b7d0f45133ac36c",
      "unstaged_diff_sha256_verified_by": "INV-CMD-06",
      "canonical_diff_command": "bash -o pipefail -c 'git --no-pager diff --no-color -- crates/tui/app/src/app/tests.rs | sha256sum'"
    },
    "rca_doc": {
      "path": "docs/plans/cursor_blinks_rapidly_in_running_state/rca.md",
      "note": "content hash omitted; hashing this file would be self-referential"
    }
  },
  "investigator_command_records": [
    {
      "id": "INV-CMD-01",
      "source": "investigator",
      "command": "cargo test -p cowboy --lib app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames",
      "cwd": "<repo-root>",
      "source_snapshot": "SNAP-01",
      "timestamp_utc": "2026-07-21T07:38:41Z",
      "exit_status": 101,
      "exit_meaning": "expected: libtest exits non-zero (101) because the regression test fails before the fix",
      "purpose": "Run the strengthened regression test and confirm it fails before any product fix."
    },
    {
      "id": "INV-CMD-02",
      "source": "investigator",
      "command": "git status --porcelain",
      "cwd": "<repo-root>",
      "source_snapshot": "SNAP-01",
      "timestamp_utc": "2026-07-21T07:38:41Z",
      "exit_status": 0,
      "purpose": "Confirm only the test file and the new RCA folder changed; no product code was modified."
    },
    {
      "id": "INV-CMD-03",
      "source": "investigator",
      "command": "cargo clippy -p cowboy --tests",
      "cwd": "<repo-root>",
      "source_snapshot": "SNAP-01",
      "timestamp_utc": "2026-07-21T07:38:31Z",
      "exit_status": 0,
      "purpose": "Confirm Clippy completes successfully with no warnings or errors from the strengthened test and extended probe backend."
    },
    {
      "id": "INV-CMD-04",
      "source": "investigator",
      "command": "cargo test -p cowboy --lib app::tests::",
      "cwd": "<repo-root>",
      "source_snapshot": "SNAP-01",
      "timestamp_utc": "2026-07-21T07:38:41Z",
      "exit_status": 101,
      "exit_meaning": "expected: libtest exits non-zero (101) because the one new regression test fails; all 34 sibling tests pass",
      "purpose": "Confirm the new regression test is the only failure and no sibling cursor/animation test regressed."
    },
    {
      "id": "INV-CMD-05",
      "source": "investigator",
      "command": "sha256sum crates/tui/app/src/app/tests.rs",
      "cwd": "<repo-root>",
      "source_snapshot": "SNAP-01",
      "timestamp_utc": "2026-07-21T07:38:16Z",
      "exit_status": 0,
      "purpose": "Executable verification of the tested-source content digest recorded in SNAP-01.tested_source.content_sha256."
    },
    {
      "id": "INV-CMD-06",
      "source": "investigator",
      "command": "bash -o pipefail -c 'git --no-pager diff --no-color -- crates/tui/app/src/app/tests.rs | sha256sum'",
      "cwd": "<repo-root>",
      "source_snapshot": "SNAP-01",
      "timestamp_utc": "2026-07-21T07:44:33Z",
      "exit_status": 0,
      "exit_meaning": "run under `bash -o pipefail` so the overall status is nonzero if EITHER git diff or sha256sum fails; observed overall exit 0. A cross-checked run without the pipe (INV-CMD-06b, explicit PIPESTATUS) confirmed git_status=0 and sha256sum_status=0.",
      "purpose": "Executable, failure-propagating verification of the canonical unstaged-diff digest recorded in SNAP-01.tested_source.unstaged_diff_sha256, hashing the bytes of the pinned canonical diff command."
    },
    {
      "id": "INV-CMD-06b",
      "source": "investigator",
      "command": "git --no-pager diff --no-color -- crates/tui/app/src/app/tests.rs | sha256sum; echo \"git_status=${PIPESTATUS[0]} sha256sum_status=${PIPESTATUS[1]}\"",
      "cwd": "<repo-root>",
      "source_snapshot": "SNAP-01",
      "timestamp_utc": "2026-07-21T07:44:33Z",
      "exit_status": 0,
      "purpose": "Cross-check that captures each pipeline stage status explicitly, so INV-EV-06 can assert per-stage success rather than only sha256sum's status."
    },
    {
      "id": "INV-CMD-06c",
      "source": "investigator",
      "command": "bash -c 'bash -o pipefail -c \"git --no-pager diff --no-color --this-option-does-not-exist -- crates/tui/app/src/app/tests.rs 2>/dev/null | sha256sum >/dev/null\"; pf=$?; git --no-pager diff --no-color --this-option-does-not-exist -- crates/tui/app/src/app/tests.rs 2>/dev/null | sha256sum >/dev/null; bare=$?; echo \"pipefail_variant_exit=$pf non_pipefail_variant_exit=$bare\"'",
      "cwd": "<repo-root>",
      "source_snapshot": "SNAP-01",
      "timestamp_utc": "2026-07-21T07:49:54Z",
      "exit_status": 0,
      "exit_meaning": "single negative-control command that runs BOTH variants against an intentionally failing git-diff first stage and reports each status: the pipefail variant exits 129 (failure propagated); the bare-pipe variant exits 0 (failure masked). Both outcomes now have command provenance from this one execution.",
      "purpose": "Prove, from one recorded command, that the pipefail form propagates a first-stage failure the bare pipe masks — giving INV-EV-06c full provenance for both reported statuses."
    }
  ],
  "investigator_evidence_records": [
    {
      "id": "INV-EV-01",
      "source": "investigator",
      "matches_command": "INV-CMD-01",
      "source_snapshot": "SNAP-01",
      "assertion_site": "crates/tui/app/src/app/tests.rs:471",
      "observed_result": "FAILED: cursor_show_calls == 4 (left 4, right 0) across 4 animation-driven production frames; recorded transitions [Hidden, Shown, Hidden, Shown, Hidden, Shown, Hidden, Shown].",
      "test_outcome_line": "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 311 filtered out; finished in 0.01s",
      "observed_exit_status": 101,
      "conclusion": "fails_before_fix"
    },
    {
      "id": "INV-EV-02",
      "source": "investigator",
      "matches_command": "INV-CMD-02",
      "source_snapshot": "SNAP-01",
      "observed_result": " M crates/tui/app/src/app/tests.rs / ?? docs/plans/cursor_blinks_rapidly_in_running_state/",
      "observed_exit_status": 0,
      "conclusion": "tests_and_docs_only_no_product_code_changed"
    },
    {
      "id": "INV-EV-03",
      "source": "investigator",
      "matches_command": "INV-CMD-03",
      "source_snapshot": "SNAP-01",
      "observed_result": "Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.23s; zero warning/error lines emitted.",
      "observed_exit_status": 0,
      "conclusion": "clippy_completed_successfully_no_warnings"
    },
    {
      "id": "INV-EV-04",
      "source": "investigator",
      "matches_command": "INV-CMD-04",
      "source_snapshot": "SNAP-01",
      "observed_result": "test result: FAILED. 34 passed; 1 failed; 0 ignored; 0 measured; 277 filtered out; finished in 0.08s",
      "observed_exit_status": 101,
      "conclusion": "only_new_regression_test_fails_siblings_pass"
    },
    {
      "id": "INV-EV-05",
      "source": "investigator",
      "matches_command": "INV-CMD-05",
      "source_snapshot": "SNAP-01",
      "observed_result": "29062dcdda67e8a922247640e43e53001311a059642a282cbda56ea0649e58cb  crates/tui/app/src/app/tests.rs",
      "observed_exit_status": 0,
      "conclusion": "content_digest_matches_SNAP-01"
    },
    {
      "id": "INV-EV-06",
      "source": "investigator",
      "matches_command": "INV-CMD-06",
      "source_snapshot": "SNAP-01",
      "observed_result": "4d92a6b5f7e39648ff47d08190c0b823c30f15137b363ce17b7d0f45133ac36c  -",
      "observed_exit_status": 0,
      "exit_propagation": "bash -o pipefail; overall nonzero if git diff OR sha256sum fails",
      "conclusion": "canonical_diff_digest_matches_SNAP-01_with_failure_propagation"
    },
    {
      "id": "INV-EV-06b",
      "source": "investigator",
      "matches_command": "INV-CMD-06b",
      "source_snapshot": "SNAP-01",
      "observed_result": "4d92a6b5f7e39648ff47d08190c0b823c30f15137b363ce17b7d0f45133ac36c  -\\ngit_status=0 sha256sum_status=0",
      "observed_exit_status": 0,
      "conclusion": "both_pipeline_stages_succeeded_individually"
    },
    {
      "id": "INV-EV-06c",
      "source": "investigator",
      "matches_command": "INV-CMD-06c",
      "source_snapshot": "SNAP-01",
      "observed_result": "pipefail_variant_exit=129 non_pipefail_variant_exit=0",
      "observed_exit_status": 0,
      "both_variants_from_one_command": true,
      "conclusion": "pipefail_propagates_first_stage_failure_129_bare_pipe_masks_it_0"
    }
  ],
  "implementation_command_records": [],
  "implementation_evidence_records": [],
  "tester_command_records": [],
  "tester_evidence_records": [],
  "validator_command_records": [],
  "validator_evidence_records": [],
  "reviewer_command_records": [],
  "reviewer_evidence_records": [],
  "reviewer_soundness_assessments": []
}
```

## Fix constraints

- Product decision (acceptance boundary): while the running status animation is
  active (`AppState::status_animation_active()`), the composer cursor must be
  hidden for the entire duration of every animation-driven production frame. The
  observable contract is zero application-driven `show_cursor` calls and zero
  `Hidden -> Shown` visibility transitions across consecutive running frames.
  "Keep the cursor visible without toggling" is explicitly rejected: the
  configured style is `SetCursorStyle::BlinkingBlock`, so a visible cursor blinks
  at the terminal's own rate regardless of application toggles, and leaving it
  visible during running would reintroduce the Windows flash that
  `draw_cursor_safe_production_frame` was added to prevent. Hidden-throughout is
  the boundary that satisfies the user's "prevent cursor from blinking in this
  case."
- Do not stop, slow, or special-case the running status animation to hide the
  symptom; the animation must keep advancing and dirtying frames through
  `tick_status_animation`. The fix changes cursor visibility, not animation
  cadence.
- Gate the suppression precisely on `status_animation_active()` (run state
  `running` with no pending prompt). Preserve the existing steady-blink cursor at
  the composer input position in every other state, including idle and
  `WaitingForInput` (where `run_state` is `waiting`, the animation is inactive,
  and the user is being prompted).
- Keep terminal cursor-style policy in the terminal seam. Do not add
  `SetCursorStyle`, `EnableBlinking`, or `DisableBlinking` to per-frame draw
  code, and leave `cowboy_tui_terminal::tui_input_cursor_style()` returning
  `SetCursorStyle::BlinkingBlock`. Hiding the cursor is a visibility operation,
  not a style reset.
- Do not regress the pure cursor placement/layout math, which is exercised
  outside the running-animation state through the raw `draw` helper:
  `draw_places_cursor_at_input_end`,
  `draw_places_cursor_at_moved_single_line_position`,
  `draw_places_cursor_at_wrapped_input_end`, and
  `draw_places_cursor_at_moved_wrapped_input_position` must still pass.
- Reconcile the product decision with `draw_places_cursor_in_active_run_draft_input`.
  That test runs in the `running` state (a background workflow task sets
  `run_state = "running"`) and asserts a visible cursor position, encoding the
  "draft editable while a run is active" affordance from
  `docs/plans/allow_typing_while_run_active.md`. Under the product decision above
  the cursor is hidden during running animation frames, which is in direct
  tension with that affordance. This is a conscious product call, not a
  test-seam trick: the planner must decide how the two coexist — for example, by
  scoping the visible-editable-cursor affordance to states where the running
  animation is not active (idle drafts, `WaitingForInput`), and updating
  `draw_places_cursor_in_active_run_draft_input` accordingly if the fix suppresses
  the cursor during running. Note that this test renders through the raw `draw`
  helper rather than the production `draw_cursor_safe_production_frame` seam, so a
  fix applied only at the production seam would leave it green without proving the
  production affordance either way; the planner must make the product intent
  explicit rather than relying on which helper a test happens to call.
- Do not regress the Windows anti-flash contract
  `status_animation_redraw_hides_cursor_before_painting_changed_cell`, which
  shares the extended `CursorVisibilityProbeBackend`.
- The fix must make
  `crates/tui/app/src/app/tests.rs::running_state_does_not_toggle_cursor_visibility_across_animation_frames`
  pass without weakening its assertions: `cursor_show_calls == 0` and zero
  `Hidden -> Shown` transitions across the running animation frames.
