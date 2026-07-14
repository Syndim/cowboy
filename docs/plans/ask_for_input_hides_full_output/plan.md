# Plan: Show full step output when asking the user for input

## Plan

When an agent step completes with status `blocked`, the workflow hands control
back to the user to answer a follow-up prompt. The `Body` section of the
`Step completed` card carries the agent's full "why blocked / unblock path"
explanation, which is exactly the context the user must read to respond. Today
`render_workflow_event` in `crates/tui/app/src/app/events.rs` caps that body to
8 visual rows via `CardSection::capped(8)`, collapsing the rest into a
`… N more rows` marker and hiding the context.

Root cause and constraints are documented in
`docs/plans/ask_for_input_hides_full_output/rca.md`. The reviewed regression
test `blocked_step_completed_body_shows_full_context_for_user_input`
(`crates/tui/app/src/app/events.rs`) already demonstrates the bug and is the
acceptance test for the fix; it must not be rewritten or replaced.

The fix keeps the tighter 8-row cap for ordinary completed steps but defers to
the default `SECTION_BODY_LIMIT` (120) for completions that route control back
to the user for input (notably `status == "blocked"`). This is a
presentation-only change confined to the TUI app crate; no workflow runtime
logic is added to the TUI crates.

## Changes

- `crates/tui/app/src/app/events.rs`
  - In the `WorkflowEventKind::StepCompleted` arm, stop unconditionally applying
    `.capped(8)` to the `Body` section. When the completion status is one that
    hands control back to the user for input (`blocked`), build the `Body`
    section without the tighter cap so it uses the default `SECTION_BODY_LIMIT`.
    For all other statuses, preserve the existing `.capped(8)` behavior.
  - Implement this by branching on `status_value` (e.g. a small helper
    `hands_off_to_user(status)` / inline check for `"blocked"`) to decide
    whether to call `.capped(8)`.

- `crates/tui/app/src/app/card.rs`
  - No behavior change expected; the default `SECTION_BODY_LIMIT` path already
    exists. Only touch this file if a small supporting helper is needed to
    express "uncapped body section" cleanly. Keep width-safe per-row truncation
    intact — the change concerns vertical row-count capping only.

## Tests to be added/updated

- Keep as-is (input to the fix, do not modify):
  `crates/tui/app/src/app/events.rs::blocked_step_completed_body_shows_full_context_for_user_input`
  — must pass after the fix (full body shown, no `more rows` marker).
- Keep passing (guards regression for ordinary completions):
  `crates/tui/app/src/app/events.rs::renders_waiting_and_completed_cards_with_sections`
  — a non-blocked completed step still shows the 8-row cap with a
  `… N more rows` marker.

## How to verify

- Targeted repro test (must pass after fix):
  `cargo test -p cowboy --lib blocked_step_completed_body_shows_full_context_for_user_input`
- Regression for ordinary completions (must still pass):
  `cargo test -p cowboy --lib renders_waiting_and_completed_cards_with_sections`
- Full app-crate test pass:
  `cargo test -p cowboy`
- Lint/format cleanliness:
  `cargo clippy -p cowboy --all-targets` and `cargo fmt --check`

## TODO

- [x] Read `docs/plans/ask_for_input_hides_full_output/rca.md` and confirm the
      failing repro test.
- [x] In `crates/tui/app/src/app/events.rs`, branch the `StepCompleted` `Body`
      section so `blocked` completions skip `.capped(8)` and use the default
      `SECTION_BODY_LIMIT`.
- [x] Keep `.capped(8)` for all non-`blocked` completed statuses.
- [x] Verify per-row width-safe truncation and `card.rs` rendering remain
      unchanged.
- [x] Run `cargo test -p cowboy --lib blocked_step_completed_body_shows_full_context_for_user_input`
      and confirm it passes.
- [x] Run `cargo test -p cowboy --lib renders_waiting_and_completed_cards_with_sections`
      and confirm it still passes.
- [x] Run `cargo test -p cowboy`, `cargo clippy -p cowboy --all-targets`, and
      `cargo fmt --check`; fix all compiler and Clippy warnings before yielding.
- [x] Reviewer follow-up: replace the inline `status_value == "blocked"` check in
      the `StepCompleted` arm with an intention-revealing predicate
      (`body_should_expand`) that centralizes the expand-worthy status set; keep
      behavior identical and the repro test unchanged.
