## Plan

Use the RCA in `docs/plans/card_ui_should_take_full_width/rca.md` as the source of truth. The regression test `crates/tui/app/src/app/card.rs::wide_cards_expand_to_available_width` already captures the bug: `Card::render(120)` produces an 80-column border because the renderer caps requested width at `DEFAULT_CARD_WIDTH`.

Fix the card renderer so the width supplied by transcript rendering is preserved for wide terminals while retaining the existing minimum-width guard for narrow terminals.

## Changes

- In `crates/tui/app/src/app/card.rs`, change `Card::render` width normalization from an upper-bounded clamp to a minimum-only guard, e.g. keep at least `MIN_CARD_WIDTH` but do not cap at `DEFAULT_CARD_WIDTH`.
- Keep `DEFAULT_CARD_WIDTH` for default/plain-text rendering paths that intentionally render without a live terminal width.
- Leave the border, section divider, body wrapping, padding, and title truncation helpers driven by the normalized render width so they naturally expand to the full available transcript width.
- Avoid changing transcript width plumbing in `crates/tui/app/src/app/controls/transcript.rs`, `crates/tui/app/src/app/state.rs`, and `crates/tui/app/src/app/events.rs`; repository inspection shows these paths already pass the available width into `Card::render`.

## Tests to be added/updated

- Keep the investigator-added repro test unchanged: `crates/tui/app/src/app/card.rs::wide_cards_expand_to_available_width`.
- Do not replace that test with a narrower assertion; it must continue to prove that a 120-column render produces a 120-column card border.
- No new regression test is required unless implementation reveals another uncovered edge case. Existing card tests already cover 80-column rendering, narrow wrapping, empty cards, section truncation, and border chrome.

## How to verify

- Run `cargo test -p cowboy app::card::tests::wide_cards_expand_to_available_width` and confirm it passes.
- Run `cargo test -p cowboy app::card::tests` and confirm existing card rendering behavior still passes.
- If the implementation changes any broader TUI rendering path beyond `Card::render`, also run the narrowest affected module tests for that path.

## TODO

- [x] Update `Card::render` to enforce only `MIN_CARD_WIDTH` and remove the `DEFAULT_CARD_WIDTH` upper cap from live card rendering.
- [x] Confirm `DEFAULT_CARD_WIDTH` remains in default/plain-text rendering paths where no live terminal width is provided.
- [x] Keep `wide_cards_expand_to_available_width` intact and use it as the regression gate for the fix.
- [x] Run the repro test command and record the passing result.
- [x] Run the full `app::card::tests` set and fix any regressions in card rendering behavior.
