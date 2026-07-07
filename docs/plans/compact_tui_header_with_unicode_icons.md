# Plan

Make the TUI's top header/title more compact by replacing verbose metadata labels and textual run-status words with Unicode symbols. The concrete target is `crates/tui/app/src/app/controls/header.rs`, where the current header builds text like `running`, `step:...`, `run:...`, `workflow:...`, `tasks:...`, and `no active run`.

Use plain text Unicode symbols rather than emoji-style glyphs where possible, so terminals render the header predictably. Proposed header shape for an active run:

```text
Cowboy ─ ● ─ ↳ implement ─ ▶ 170dc431 ─ ⎇ agent/00-feature ─ ◷ 1
```

Use this mapping in the top header only:

- status: `○` idle, `●` running, `◔` waiting, `↻` retrying, `✓` completed, `✗` failed, `■` cancelled, `?` unknown/unmapped;
- current step: `↳ {step}`;
- active run: `▶ {short_run_id}`;
- workflow: `⎇ {workflow_name}` as the git-flow/branch-like workflow icon;
- background tasks: `◷ {count}`.

Do not print status words like `running` / `waiting`, the word `workflow`, or `no active run` in the top header. Keep state-colored styling by continuing to style the header from `state.display_state()`. Keep fuller textual status copy in the status line below the transcript; this plan only compacts the top header.

# Changes

- Update `crates/tui/app/src/app/controls/header.rs` so `text` builds structured header parts with semantic priorities instead of relying on `starts_with("workflow:")`, `starts_with("run:")`, or other rendered string prefixes for width-based removal.
- Add a header-local status-icon mapper from `state.display_state()` to the compact symbols:
  - `idle` -> `○`;
  - `running` -> `●`;
  - `waiting` -> `◔`;
  - `retrying` -> `↻`;
  - `completed` -> `✓`;
  - `failed` -> `✗`;
  - `cancelled` / `canceled` -> `■`;
  - any unknown status -> `?`.
- Replace verbose metadata labels in the top header with constants for the selected Unicode symbols:
  - `step:{step}` becomes `↳ {step}`;
  - `run:{short}` becomes `▶ {short}`;
  - `workflow:{workflow}` becomes `⎇ {workflow}`;
  - `tasks:{count}` becomes `◷ {count}`.
- Remove the top-header `no active run` text. For no active run, render only `Cowboy ─ ○` plus any other applicable compact state; leave explanatory text to the existing status line.
- Preserve the current short-run-id behavior: strip the `run-` prefix and use the first UUID segment when available.
- Preserve the existing separator (` ─ `), optional field ordering, header bold modifier, and state-based styling.
- Preserve narrow-width field-removal priority for optional details: tasks first, workflow second, run third, step fourth. Do not drop `Cowboy` or the status icon before final truncation.
- Make header width checks Unicode-display-width-aware for the new symbols. Prefer a small header-local helper using the existing `unicode-width` dependency, or update the shared truncate helper only if the change is covered by focused tests and does not broaden behavior unexpectedly.
- Do not change workflow runtime logic, CLI output, transcript event rendering, status-line copy, composer behavior, or configuration.

# Tests to be added/updated

- Update `crates/tui/app/src/app/controls/header.rs` tests to assert a full-width active-run header uses the new compact Unicode symbols: `●`, `↳`, `▶`, `⎇`, and, when applicable, `◷`.
- Update header tests to assert the top header no longer contains verbose labels or status words: `running`, `waiting`, `completed`, `failed`, `cancelled`, `step:`, `run:`, `workflow:`, `tasks:`, or `no active run`.
- Add status-icon mapping coverage for at least `idle`, `running`, `waiting`, `retrying`, `completed`, `failed`, and `cancelled`, using state transitions/events where practical and a small mapper unit test if direct state construction is clearer.
- Update the existing narrow-header test so it verifies the workflow value and `⎇` field are dropped at narrow widths while `Cowboy`, the status icon, and the width bound remain correct.
- Add a header test covering an active background task count so `◷ {count}` appears when space allows and remains the first optional field dropped when width is constrained.
- Add or update a width test for Unicode-symbol headers so truncation/removal uses displayed column width, not byte length.
- Update any TUI render tests in `crates/tui/app/src/app/tests.rs` only if they assert old top-header copy.

# How to verify

- Run `cargo test -p cowboy controls::header::tests` to verify compact header formatting, status icons, field priority, and width behavior.
- Run `cargo test -p cowboy tests::draw_active_run_composer_shows_draft_copy_without_slash_suggestions` if any full-screen TUI draw assertions change.
- Run `cargo test -p cowboy` as the focused crate regression pass.
- Manual smoke check in the TUI with an active run: confirm the top header shows compact symbols such as `●`, `↳`, `▶`, `⎇`, and `◷` with their values; confirm the literal words `workflow`, `running`, and `no active run` are absent from the top header; narrow the terminal and confirm optional fields disappear in priority order before the fixed `Cowboy` and status icon are truncated.

# TODO

- [x] Convert header metadata construction in `crates/tui/app/src/app/controls/header.rs` from verbose label strings to structured compact Unicode-symbol parts.
- [x] Add the header status-icon mapper for idle, running, waiting, retrying, completed, failed, cancelled/canceled, and unknown statuses.
- [x] Replace the top-header textual status field with the status icon while preserving state-based styling.
- [x] Replace `workflow:` with the git-flow-like `⎇` workflow field.
- [x] Replace the run-id `#`/`run:` plan with the better `▶` run field while preserving short run-id extraction.
- [x] Remove `no active run` from the top header and rely on the idle status icon plus existing status-line text.
- [x] Preserve header ordering, separator, bold modifier, styling, and optional-field drop priority.
- [x] Make header length checks safe for Unicode symbol display width.
- [x] Update header unit tests for compact symbols and absence of verbose labels/status words.
- [x] Add header status-icon mapping coverage for all known run states.
- [x] Add header coverage for background task symbol rendering and drop priority.
- [x] Update any affected full-screen TUI draw tests that assert old top-header copy.
- [x] Run the focused header tests.
- [x] Run the focused `cowboy` crate regression tests.
- [x] Manually smoke test the active-run and idle headers in the TUI at normal and narrow widths.
- [x] Address reviewer feedback by restoring the implementation to the approved header-only scope.
- [x] Address reviewer feedback by removing contradictory full-UI iconization TODOs from the plan checklist.
