# Plan

Fix the rapid composer-cursor blink that occurs only while a run is in the
`running` state. This plan builds directly on the reviewed RCA
(`docs/plans/cursor_blinks_rapidly_in_running_state/rca.md`) and keeps the
investigator regression test
`crates/tui/app/src/app/tests.rs::running_state_does_not_toggle_cursor_visibility_across_animation_frames`
as an unchanged input to the fix — the fix must make that test pass without
weakening its assertions (`cursor_show_calls == 0` and zero `Hidden -> Shown`
transitions across the running animation frames).

Root cause (from the RCA, re-verified against source): the composer requests a
terminal cursor position on every completed frame. `composer::render`
(`crates/tui/app/src/app/controls/composer.rs:91-93`) calls `set_input_cursor`
whenever `state.composer_accepts_edits()` is true, and
`AppState::composer_accepts_edits` (`crates/tui/app/src/app/state.rs:527-529`)
always returns `true`. `set_input_cursor` calls `frame.set_cursor_position(..)`
(`crates/tui/app/src/app/controls/composer.rs:952`). In the `running` state the
status animation is active (`AppState::status_animation_active`,
`crates/tui/app/src/app/state.rs:544-546`), so the main loop redraws roughly ten
times per second through `draw_cursor_safe_production_frame`
(`crates/tui/app/src/app.rs:116-132`). That helper calls `terminal.hide_cursor()`
before painting; then Ratatui's post-frame path
(`ratatui-core-0.1.2` `terminal/render.rs::apply_buffer_with_cursor`, lines
297-303) sees `Some(position)` and calls `show_cursor()` + `set_cursor_position()`.
The per-frame `hide -> show` toggle on a `SetCursorStyle::BlinkingBlock` cursor is
the reported rapid blink.

Fix approach (aligned with the RCA "Fix constraints"): suppress the cursor
*request* precisely while the running status animation is active, so no
production frame re-shows the cursor and the pre-draw `hide_cursor()` becomes the
final cursor state for the whole animation. This is a visibility decision kept in
the state/composer layer:

- Add one intention-revealing state predicate,
  `AppState::composer_shows_cursor()`, defined as
  `self.composer_accepts_edits() && !self.status_animation_active()`.
- Gate the single cursor-request site in `composer::render` on that predicate
  instead of on `composer_accepts_edits()` (a clean cutover; `composer_accepts_edits`
  is subsumed into the new predicate and keeps its other callers).

Why this satisfies the constraints:

- The gate is exactly `status_animation_active()` (run state `running` with no
  pending prompt). Every other state — idle, `WaitingForInput` (`run_state` is
  `waiting`, prompt pending), `retrying`, `completed`, `failed`, `cancelled` —
  keeps `status_animation_active()` false, so the steady-blink cursor at the
  composer input position is preserved.
- The animation cadence is untouched: `tick_status_animation` /
  `advance_status_animation` still advance and dirty frames. Only cursor
  visibility changes.
- The terminal seam is untouched: no `SetCursorStyle` / `EnableBlinking` /
  `DisableBlinking` in per-frame draw code, and
  `cowboy_tui_terminal::tui_input_cursor_style()` still returns
  `SetCursorStyle::BlinkingBlock`. Hiding is a visibility operation, not a style
  reset.
- `set_input_cursor` (hence `frame.set_cursor_position`) is the only cursor
  request in the crate (verified: the sole `set_cursor_position` caller is
  `composer::render`), so gating this one site fully covers all running frames.

Reconciliation with the "draft editable while a run is active" affordance
(`docs/plans/allow_typing_while_run_active.md`): the RCA requires an explicit
product decision because `draw_places_cursor_in_active_run_draft_input`
(`crates/tui/app/src/app/tests.rs:483-500`) currently asserts a *visible* cursor
in the `running` state. Under this fix the composer cursor is hidden throughout
running animation frames, so the visible-editable-cursor affordance is
intentionally scoped to states where the running animation is not active (idle
drafts and `WaitingForInput`). Draft editing itself is unchanged and still
covered by `composer_accepts_edits()` and its tests
(`paste_appends_to_active_run_draft_input`,
`composer_edit_and_submit_gates_track_background_prompt_and_terminal_states`, and
the `app::input` active-run editing tests); only the *visible cursor* during the
running animation is removed. That affordance test is a pre-existing test, not
the investigator repro test, so it is updated here to encode the new decision.

# Changes

- `crates/tui/app/src/app/state.rs`: add
  `pub(in crate::app) fn composer_shows_cursor(&self) -> bool`, returning
  `self.composer_accepts_edits() && !self.status_animation_active()`. Place it
  next to `composer_accepts_edits` / `composer_accepts_submit`
  (around `crates/tui/app/src/app/state.rs:527-534`). Do not modify
  `composer_accepts_edits` or `status_animation_active`.
- `crates/tui/app/src/app/controls/composer.rs`: in `render`
  (`crates/tui/app/src/app/controls/composer.rs:91-93`), change the cursor
  guard from `if state.composer_accepts_edits()` to
  `if state.composer_shows_cursor()`. No other lines in `render` change.
- Do not touch `crates/tui/app/src/app.rs` (`draw_cursor_safe_production_frame`,
  `tick_status_animation`), the status animation, or
  `crates/tui/terminal/src/lib.rs` (`tui_input_cursor_style`).

# Tests to be added/updated

- Keep unchanged (input to the fix, must go from red to green):
  `crates/tui/app/src/app/tests.rs::running_state_does_not_toggle_cursor_visibility_across_animation_frames`.
- Update and rename the affordance test to encode the product decision:
  rename `draw_places_cursor_in_active_run_draft_input`
  (`crates/tui/app/src/app/tests.rs:483-500`) to
  `draw_hides_composer_cursor_during_active_run_animation`. Keep the same setup
  (`spawn_test_card_report_task` drives `run_state = "running"` with no pending
  prompt, so `status_animation_active()` is true). Replace the visible-position
  assertion `assert_cursor_position(Position::new(6, 8))` with an assertion that
  the cursor is hidden after the draw: `assert!(!terminal.backend().cursor_visible())`
  (the composer no longer calls `frame.set_cursor_position`, so
  `apply_buffer_with_cursor(None)` hides the cursor). Keep the trailing
  `state.cancel_background_tasks();`. Update the test's intent comment to state
  that draft typing remains enabled while the visible cursor is suppressed during
  the running animation.
- Add a focused predicate test in the `state.rs` test module (near
  `status_animation_advances_only_while_running`,
  `crates/tui/app/src/app/state.rs:1488-1527`):
  `composer_hides_cursor_only_during_running_animation`. Assert `composer_shows_cursor()`
  is `true` for a fresh idle `test_state()`, becomes `false` after applying a
  `RunStarted` event (running animation active), and is `true` again after
  applying a `WaitingForInput` event (prompt pending, animation inactive).
- Must continue to pass unchanged: the Windows anti-flash contract
  `status_animation_redraw_hides_cursor_before_painting_changed_cell`
  (`crates/tui/app/src/app/tests.rs:394-420`) and the pure placement/layout tests
  `draw_places_cursor_at_input_end`,
  `draw_places_cursor_at_moved_single_line_position`,
  `draw_places_cursor_at_wrapped_input_end`,
  `draw_places_cursor_at_moved_wrapped_input_position`.

# How to verify

- Regression (the investigator repro test, unchanged) now passes:
  `cargo test -p cowboy --lib app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames`.
- Windows anti-flash contract still passes:
  `cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell`.
- Updated affordance and new predicate tests pass:
  `cargo test -p cowboy --lib app::tests::draw_hides_composer_cursor_during_active_run_animation`
  and
  `cargo test -p cowboy --lib app::state::tests::composer_hides_cursor_only_during_running_animation`.
- No new regressions across the TUI app tests, proven against an isolated
  committed baseline rather than a bare current-worktree run: run
  `cargo test -p cowboy --lib app::` on the working tree and again in a detached
  baseline worktree at `HEAD`, then compare the sets of failing test names (see
  TODO-05 for the exact ordered baseline-comparison and restoration procedure).
- No new warnings: `cargo clippy -p cowboy --tests`.
- Manual smoke (optional): `cargo run -p cowboy`, start a run so the state is
  `running`, keep focus in the composer, and confirm the input cursor no longer
  blinks rapidly during the animation; after the run blocks/finishes, confirm the
  normal steady cursor returns.

# TODO

- [x] TODO-01: Add `AppState::composer_shows_cursor` predicate returning `self.composer_accepts_edits() && !self.status_animation_active()`.
  - Procedure (ordered):
    1. In `crates/tui/app/src/app/state.rs`, add
       `pub(in crate::app) fn composer_shows_cursor(&self) -> bool { self.composer_accepts_edits() && !self.status_animation_active() }`
       adjacent to `composer_accepts_edits`/`composer_accepts_submit`
       (around lines 527-534).
    2. Run `cargo build -p cowboy`.
    3. Run `grep -n "fn composer_shows_cursor" crates/tui/app/src/app/state.rs`.
  - Expected result: step 2 (`cargo build -p cowboy`) completes successfully;
    step 3 prints exactly one match line.
  - Observed result: `cargo build -p cowboy` finished successfully (`Finished dev profile`, exit 0); `grep -n "fn composer_shows_cursor" crates/tui/app/src/app/state.rs` printed exactly one match line (`536:    pub(in crate::app) fn composer_shows_cursor(&self) -> bool {`).
- [x] TODO-02: Gate the composer cursor request on `composer_shows_cursor()`.
  - Procedure: In `crates/tui/app/src/app/controls/composer.rs::render`
    (lines 91-93), replace the guard `if state.composer_accepts_edits()` with
    `if state.composer_shows_cursor()`, leaving the `set_input_cursor(..)` call
    unchanged. Then run
    `cargo test -p cowboy --lib app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames`.
  - Expected result: the command reports `test result: ok. 1 passed; 0 failed`
    (previously it failed with `cursor_show_calls == 4`). The RCA repro test is
    not modified.
  - Observed result: after changing the guard to `if state.composer_shows_cursor()`, `cargo test -p cowboy --lib app::tests::running_state_does_not_toggle_cursor_visibility_across_animation_frames` reported `test result: ok. 1 passed; 0 failed`. The investigator repro test was not modified.
- [x] TODO-03: Update and rename the active-run affordance test to assert the cursor is hidden during the running animation.
  - Procedure (ordered):
    1. In `crates/tui/app/src/app/tests.rs`, rename
       `draw_places_cursor_in_active_run_draft_input` (lines 483-500) to
       `draw_hides_composer_cursor_during_active_run_animation`, keeping the
       `spawn_test_card_report_task` setup and the trailing
       `state.cancel_background_tasks();`.
    2. Replace
       `terminal.backend_mut().assert_cursor_position(Position::new(6, 8));` with
       `assert!(!terminal.backend().cursor_visible(), "running-animation frames must hide the composer cursor");`,
       and update the test's intent comment to say draft typing stays enabled
       while the visible cursor is suppressed during the running animation.
    3. Run
       `cargo test -p cowboy --lib app::tests::draw_hides_composer_cursor_during_active_run_animation`.
    4. Run the absence check
       `grep -rn "draw_places_cursor_in_active_run_draft_input" crates/tui/app/src/app/tests.rs; echo "exit=$?"`.
  - Expected result: step 3 reports `test result: ok. 1 passed; 0 failed`; step 4
    prints no match lines and reports `exit=1` (grep's "no lines selected"
    status), confirming the old test name is fully gone.
  - Observed result: step 3 (`cargo test -p cowboy --lib app::tests::draw_hides_composer_cursor_during_active_run_animation`) reported `test result: ok. 1 passed; 0 failed`; step 4, the exact approved compound command `grep -rn "draw_places_cursor_in_active_run_draft_input" crates/tui/app/src/app/tests.rs; echo "exit=$?"`, printed no grep match lines followed by `exit=1` (grep's no-match status echoed) and the compound command itself exited 0, confirming the old test name is fully gone.
- [x] TODO-04: Add a focused unit test for `composer_shows_cursor()` across idle, running-animation, and `WaitingForInput` states.
  - Procedure: In the `state.rs` test module (near
    `status_animation_advances_only_while_running`, lines 1488-1527), add
    `composer_hides_cursor_only_during_running_animation`: build a `test_state()`
    and assert `state.composer_shows_cursor()` is `true`; apply a
    `WorkflowEventKind::RunStarted` event (as in
    `crates/tui/app/src/app/tests.rs:398-405`) and assert
    `composer_shows_cursor()` is `false` and `status_animation_active()` is `true`;
    apply a `WorkflowEventKind::WaitingForInput` event and assert
    `composer_shows_cursor()` is `true` again. Run
    `cargo test -p cowboy --lib app::state::tests::composer_hides_cursor_only_during_running_animation`.
  - Expected result: the command reports `test result: ok. 1 passed; 0 failed`.
  - Observed result: `cargo test -p cowboy --lib app::state::tests::composer_hides_cursor_only_during_running_animation` reported `test result: ok. 1 passed; 0 failed`.
- [x] TODO-05: Verify the full TUI app test set and lints are green with no warnings.
  - Procedure (ordered):
    1. Run the Windows anti-flash contract:
       `cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell`.
    2. Run `cargo clippy -p cowboy --tests`.
    3. Run the single self-contained baseline-comparison script below from the
       repository root with `bash` (it uses `comm` and process substitution). It
       is ONE command, so the `BASE` worktree path and the captured pre-state
       snapshots survive across every step. Cleanup is performed EXPLICITLY before
       the verdict is printed and its outcome affects the verdict; an EXIT `trap`
       remains only as fallback if the script aborts early. The script never runs
       `git stash`:

    ```bash
    set -u
    cd "$(git rev-parse --show-toplevel)"

    # Pre-state snapshots (proves the run leaves the working tree and foreign stash intact).
    status_before="$(git status --porcelain)"
    stash_before="$(git stash list)"

    # Scope gate: exactly the three fix files under crates/tui/app, never transcript.rs.
    echo "=== tracked file scope (crates/tui/app) ==="
    git status --porcelain -- crates/tui/app

    # Isolated read-only baseline at committed HEAD; stable $BASE in this one shell.
    # Fallback-only EXIT trap: removes the worktree only if explicit cleanup did not.
    BASE="$(mktemp -d)/cowboy-baseline"
    removed=0
    trap '[ "$removed" = 1 ] || git worktree remove --force "$BASE" >/dev/null 2>&1 || true' EXIT
    git worktree add --detach "$BASE" HEAD >/dev/null

    # Same app:: suite on the working tree and on the clean baseline (one lib target
    # => exactly one `test result:` summary each).
    wt_out="$(mktemp)"; base_out="$(mktemp)"
    cargo test -p cowboy --lib app:: >"$wt_out" 2>&1; wt_status=$?
    cargo test -p cowboy --manifest-path "$BASE/Cargo.toml" --lib app:: >"$base_out" 2>&1; base_status=$?

    extract() { grep -E '^test .+ \.\.\. FAILED$' "$1" | sed -E 's/^test (.+) \.\.\. FAILED$/\1/' | sort -u; }

    # Suite-completion oracle: prove each run actually REACHED libtest and its
    # summary/exit status is consistent with its failure set. Rejects compile /
    # manifest / environment failures that never produce a `test result:` line.
    suite_complete() {  # <output-file> <exit-status> <label>
        local f="$1" st="$2" label="$3" n
        n="$(grep -cE '^test result:' "$f")"
        if [ "$n" != 1 ]; then
            echo "SUITE INCOMPLETE ($label): expected exactly one 'test result:' summary, got $n (suite never reached libtest)"
            return 1
        fi
        if [ -z "$(extract "$f")" ]; then
            { grep -qE '^test result: ok\.' "$f" && [ "$st" = 0 ]; } \
                || { echo "SUITE STATUS MISMATCH ($label): empty failure set but summary/exit not clean ok/0 (exit=$st)"; return 1; }
        else
            { grep -qE '^test result: FAILED\.' "$f" && [ "$st" = 101 ]; } \
                || { echo "SUITE STATUS MISMATCH ($label): non-empty failure set but summary/exit not FAILED/101 (exit=$st)"; return 1; }
        fi
        return 0
    }

    # Placement oracle: the four required tests must be present and `... ok` in the
    # working-tree run (a missing / ignored / filtered-out / failed one is rejected).
    check_placement() {  # <working-tree-output-file>
        local f="$1" t prc=0
        for t in input_end moved_single_line_position wrapped_input_end moved_wrapped_input_position; do
            grep -qE "^test app::tests::draw_places_cursor_at_${t} \.\.\. ok\$" "$f" \
                || { echo "PLACEMENT MISSING/NOT-OK: app::tests::draw_places_cursor_at_${t}"; prc=1; }
        done
        return $prc
    }

    # Failing-test sets and the BIDIRECTIONAL difference (exact-equality oracle).
    wt_fail="$(extract "$wt_out")"; base_fail="$(extract "$base_out")"
    new_fail="$(comm -23 <(printf '%s\n' "$wt_fail") <(printf '%s\n' "$base_fail") | sed '/^$/d')"   # in working tree, not baseline
    gone_fail="$(comm -13 <(printf '%s\n' "$wt_fail") <(printf '%s\n' "$base_fail") | sed '/^$/d')"  # in baseline, not working tree

    # EXPLICIT verdict-affecting cleanup BEFORE printing the result.
    cleanup_rc=0
    git worktree remove --force "$BASE" >/dev/null 2>&1 && removed=1 || cleanup_rc=1
    git worktree list | grep -qF "$BASE" && cleanup_rc=1   # $BASE must be absent afterward
    status_after="$(git status --porcelain)"
    stash_after="$(git stash list)"

    echo "=== working-tree failures ==="; printf '%s\n' "$wt_fail"
    echo "=== baseline failures ===";     printf '%s\n' "$base_fail"
    echo "=== working-only failures (regressions) ==="; printf '%s\n' "$new_fail"
    echo "=== baseline-only failures (disappeared) ==="; printf '%s\n' "$gone_fail"
    echo "=== required placement tests (working tree) ==="
    grep -E '^test app::tests::draw_places_cursor_at_.+ \.\.\. (ok|FAILED|ignored)$' "$wt_out" || echo "(none printed)"
    echo "wt_status=$wt_status base_status=$base_status cleanup_rc=$cleanup_rc"

    rc=0
    suite_complete "$wt_out"   "$wt_status"   working-tree || rc=1
    suite_complete "$base_out" "$base_status" baseline     || rc=1
    check_placement "$wt_out" || rc=1
    [ -n "$new_fail" ]  && { echo "REGRESSION: new app:: failure(s) introduced"; rc=1; }
    [ -n "$gone_fail" ] && { echo "SET MISMATCH: baseline failure(s) absent from working tree"; rc=1; }
    [ "$cleanup_rc" != 0 ]                  && { echo "CLEANUP FAILED: baseline worktree not removed"; rc=1; }
    [ "$status_before" != "$status_after" ] && { echo "WORKTREE STATE CHANGED"; rc=1; }
    [ "$stash_before"  != "$stash_after"  ] && { echo "STASH MUTATED"; rc=1; }
    [ "$rc" = 0 ] && echo "PASS: both suites completed; failure sets identical; four placement tests ok; cleanup verified; stash unchanged"
    exit $rc
    ```
  - Expected result:
    - Step 1 anti-flash test passes (`test result: ok. 1 passed; 0 failed`).
    - Step 2 `cargo clippy -p cowboy --tests` finishes with no warnings or errors.
    - Step 3 script: the `tracked file scope` block lists exactly
      ` M crates/tui/app/src/app/controls/composer.rs`,
      ` M crates/tui/app/src/app/state.rs`, and
      ` M crates/tui/app/src/app/tests.rs`, and never `transcript.rs`. The
      `working-tree failures` and `baseline failures` blocks are IDENTICAL — each
      either empty or exactly the single pre-existing, unrelated failure
      `app::controls::transcript::tests::overflowing_content_uses_full_width_without_scrollbar_chrome`
      (see below for the placement-test assertion). Each suite is proven to have
      REACHED libtest: the `suite_complete` oracle requires exactly one
      `test result:` summary per run and a summary/exit status consistent with
      that run's failure set (empty set => `test result: ok.` and exit `0`;
      the single-transcript-failure set => `test result: FAILED.` and exit `101`).
      Output that never reaches libtest (compile, manifest, or environment error,
      giving zero `test result:` lines) makes `suite_complete` emit
      `SUITE INCOMPLETE`/`SUITE STATUS MISMATCH` and forces `rc=1`, so two failed
      Cargo invocations can no longer be mistaken for two clean empty failure sets.
      The `required placement tests (working tree)` block prints one
      `test app::tests::draw_places_cursor_at_<...> ... ok` line for each of
      `input_end`, `moved_single_line_position`, `wrapped_input_end`, and
      `moved_wrapped_input_position`; `check_placement` sets `rc=1` with
      `PLACEMENT MISSING/NOT-OK` if any of the four is absent, ignored,
      filtered-out, or `FAILED`.
      Both the `working-only failures` and `baseline-only failures` blocks are
      empty (exact-equality oracle: neither a new working-tree failure nor a
      disappeared baseline failure is allowed), `cleanup_rc=0`, and the working
      tree and stash are unchanged, so the script prints
      `PASS: both suites completed; failure sets identical; four placement tests ok; cleanup verified; stash unchanged`
      and exits `0`. Any of `SUITE INCOMPLETE`, `SUITE STATUS MISMATCH`,
      `PLACEMENT MISSING/NOT-OK`, `REGRESSION`, `SET MISMATCH`, `CLEANUP FAILED`,
      `WORKTREE STATE CHANGED`, or `STASH MUTATED` prints and the script exits
      non-zero; the item must not be checked until the script exits `0`. Because
      explicit cleanup runs before the verdict and a failed removal sets `rc=1`,
      the script cannot print `PASS`/exit `0` while leaving the detached worktree
      behind.
    - Scope note: the transcript failure exercises character-level transcript
      word-wrap in `crates/tui/app/src/app/controls/transcript.rs`, which this
      cursor fix does not touch; fixing it is out of scope ("avoid unrelated
      cleanup").
  - Observed result: all three steps passed.
    - Step 1: `cargo test -p cowboy --lib app::tests::status_animation_redraw_hides_cursor_before_painting_changed_cell`
      reported `test result: ok. 1 passed; 0 failed` (exit 0).
    - Step 2: `cargo clippy -p cowboy --tests` finished with no warnings or
      errors (exit 0).
    - Step 3: three commands were executed and are each recorded in
      `implementation_commands` under `procedure_index: 3`. First, the durable
      script at `docs/plans/cursor_blinks_rapidly_in_running_state/todo05_baseline.sh`
      was proven byte-identical to the plan's step-3 ```bash fence (plan.md lines
      215-302, 4-space Markdown indent stripped) with
      `diff <(sed -n '215,302p' docs/plans/cursor_blinks_rapidly_in_running_state/plan.md | sed -E 's/^    //') docs/plans/cursor_blinks_rapidly_in_running_state/todo05_baseline.sh`
      (exit 0, no differences). Second, its syntax was checked with
      `bash -n docs/plans/cursor_blinks_rapidly_in_running_state/todo05_baseline.sh`
      (exit 0, syntax clean). Third, it was executed as ONE invocation from the
      repository root with the exact command
      `bash docs/plans/cursor_blinks_rapidly_in_running_state/todo05_baseline.sh`.
      It printed
      `PASS: both suites completed; failure sets identical; four placement tests ok; cleanup verified; stash unchanged`,
      exiting `0`. The `tracked file scope` block listed exactly
      ` M crates/tui/app/src/app/controls/composer.rs`,
      ` M crates/tui/app/src/app/state.rs`, and
      ` M crates/tui/app/src/app/tests.rs` (never `transcript.rs`). The
      `working-tree failures` and `baseline failures` blocks were IDENTICAL, each
      the single pre-existing, unrelated failure
      `app::controls::transcript::tests::overflowing_content_uses_full_width_without_scrollbar_chrome`;
      both `working-only failures` and `baseline-only failures` blocks were empty.
      `wt_status=101 base_status=101` (each `test result: FAILED.` consistent with
      its single-failure set, so both `suite_complete` checks passed). All four
      required placement tests printed `test app::tests::draw_places_cursor_at_<...> ... ok`
      (`input_end`, `moved_single_line_position`, `wrapped_input_end`,
      `moved_wrapped_input_position`). `cleanup_rc=0` and a follow-up
      `git worktree list` showed no leftover baseline worktree; `git status`
      afterward showed only the three fix files plus this work-dir, and the
      foreign `[cowboy2]` stash entry was unchanged. No `SUITE INCOMPLETE`,
      `SUITE STATUS MISMATCH`, `PLACEMENT MISSING/NOT-OK`, `REGRESSION`,
      `SET MISMATCH`, `CLEANUP FAILED`, `WORKTREE STATE CHANGED`, or
      `STASH MUTATED` was printed. The fix therefore introduces no new `app::`
      failure beyond the documented pre-existing baseline failure.
