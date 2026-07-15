## Plan

Implement the fix described in the approved RCA at `docs/plans/card_ui_collapses_content/rca.md` as a clean cutover to always-expanded card sections. Card rendering will preserve every visual row produced by display-width-aware wrapping; transcript viewport clipping and its existing scroll controls will remain the only mechanism that limits what is visible at once.

Use the investigator-added regression test `crates/tui/app/src/app/events.rs::app::events::tests::step_completed_card_does_not_collapse_body_without_expand_control` unchanged as the first pass/fail signal. Do not replace it or weaken its assertions.

## Changes

- In `crates/tui/app/src/app/card.rs`, remove section-level row-limit state and APIs (`SECTION_BODY_LIMIT`, `CardSection::max_lines`, and `CardSection::capped`) so both `CardSection::body` and `CardSection::named` retain all content.
- Simplify `section_wrapped_lines` to return every row from the existing `wrap_line` path. Remove the destructive `.take(...)` path and the `… N more rows` marker while preserving span styles, Unicode/display-width-safe wrapping, section labels, borders, and row widths. Keep wrapping to a single pass rather than re-wrapping content to count omitted rows.
- In `crates/tui/app/src/app/events.rs`, build the completed-step `Body` section directly without the status-dependent eight-row cap. Remove the now-obsolete `body_should_expand` helper and its blocked-status special case; all completed statuses must follow the same always-expanded behavior.
- Leave transcript viewport selection, scroll offsets, and scroll key handling unchanged. They provide non-destructive access to long cards after the renderer stops discarding rows.

## Tests to be added/updated

- Keep `step_completed_card_does_not_collapse_body_without_expand_control` unchanged and use it to prove the approved-status regression is fixed.
- Update `renders_waiting_and_completed_cards_with_sections` in `crates/tui/app/src/app/events.rs` so it expects the completed card's final body row and no `more rows` marker instead of encoding the former eight-row truncation.
- Retain `blocked_step_completed_body_shows_full_context_for_user_input` as coverage that blocked completed steps still show all content after the status-specific branch is removed.
- Replace the cap-oriented renderer expectations in `crates/tui/app/src/app/card.rs` with always-expanded wrapping assertions: all wrapped segments must remain present, no omission marker may appear, and every rendered row must remain within the requested display width.
- Add renderer coverage with more than 120 rows for both body and named sections so removal of the generic default cap is protected independently of the ten-row completed-step regression.
- Run the existing transcript-control tests to confirm long content remains reachable through viewport scrolling rather than being destroyed by card rendering.

## How to verify

1. Run the unchanged regression test and confirm it changes from the RCA's deterministic exit-code-101 failure to a pass:
   `cargo test -p cowboy app::events::tests::step_completed_card_does_not_collapse_body_without_expand_control -- --exact --nocapture`
2. Run the focused card renderer tests:
   `cargo test -p cowboy app::card::tests`
3. Run the focused workflow-event renderer tests:
   `cargo test -p cowboy app::events::tests`
4. Run the transcript viewport and scrolling tests:
   `cargo test -p cowboy app::controls::transcript::tests`
5. Check formatting and all Cowboy targets for compiler and Clippy warnings:
   `cargo fmt --check`
   `cargo clippy -p cowboy --all-targets -- -D warnings`

## TODO

- [x] Remove generic section caps and omission-marker rendering from `crates/tui/app/src/app/card.rs` while preserving wrapping, styles, borders, labels, and width bounds.
- [x] Remove the completed-step eight-row cap and obsolete status-specific expansion helper from `crates/tui/app/src/app/events.rs`.
- [x] Run the unchanged investigator regression test and confirm the product-code fix makes the final body row visible without an omission marker.
- [x] Update existing event and card renderer tests that encode truncation, without changing the investigator-added regression test.
- [x] Add greater-than-120-row coverage for both body and named card sections.
- [x] Run the focused card, event, and transcript tests plus formatting and Clippy checks, fixing any warnings caused by the change.
