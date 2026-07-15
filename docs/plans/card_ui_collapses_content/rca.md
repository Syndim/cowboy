## Bug behavior

Card bodies are rendered in a collapsed form even though the terminal UI provides no control to reveal omitted rows. A completed step with ten body rows and status `approved` displays rows 1–8, replaces rows 9–10 with `… 2 more rows`, and permanently hides the omitted content from the card.

The problem is broader than this ten-row example: every `CardSection::body` and `CardSection::named` section receives a default 120-visual-row cap, so sufficiently long content in any card is also collapsed.

## Root cause

`crates/tui/app/src/app/events.rs` constructs a completed-step body section and applies `.capped(8)` whenever the step status is not `blocked`. Only `blocked` completed steps bypass that explicit cap.

`crates/tui/app/src/app/card.rs` also initializes every body and named section with `SECTION_BODY_LIMIT` (120 rows). `section_wrapped_lines` wraps the content, keeps only `max_lines` rows with `.take(section.max_lines)`, and appends an `… N more rows` marker when rows were discarded.

The `Card` and `CardSection` models contain no expanded/collapsed state, and the transcript controls contain no action that can reveal discarded rows. The direct renderer regression test fails without invoking transcript viewport clipping, which rules out scrolling as the cause. The renderer itself discards the rows before the transcript receives them.

## Reproduction steps

1. Construct a `WorkflowEventKind::StepCompleted` event with status `approved` and a body containing ten newline-separated rows.
2. Render it through `render_workflow_event`, the same card renderer used by the terminal transcript.
3. Inspect the rendered card text.
4. Observe that `body line 10` is absent and `… 2 more rows` is present, despite there being no expand action.
5. Run the narrow regression command twice; it fails deterministically in under one second after compilation artifacts are warm.

## Regression test

- Test file: `crates/tui/app/src/app/events.rs`
- Test name: `app::events::tests::step_completed_card_does_not_collapse_body_without_expand_control`
- Command: `cargo test -p cowboy app::events::tests::step_completed_card_does_not_collapse_body_without_expand_control -- --exact --nocapture`
- Expected failure before the fix: the assertion that the rendered card contains `body line 10` fails because the card contains only rows 1–8 followed by `… 2 more rows`.

## Current failing result

The command exits with code 101:

```text
running 1 test
thread 'app::events::tests::step_completed_card_does_not_collapse_body_without_expand_control' panicked at crates/tui/app/src/app/events.rs:867:9:
✓ Step completed · ↳ review · ▶ 170dc431
╭──────────────────────────────────────────────────────────────────────────────╮
│Action: status · Status: approved                                             │
├─── Body ─────────────────────────────────────────────────────────────────────┤
│body line 1                                                                   │
│body line 2                                                                   │
│body line 3                                                                   │
│body line 4                                                                   │
│body line 5                                                                   │
│body line 6                                                                   │
│body line 7                                                                   │
│body line 8                                                                   │
│… 2 more rows                                                                 │
╰──────────────────────────────────────────────────────────────────────────────╯
test app::events::tests::step_completed_card_does_not_collapse_body_without_expand_control ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 209 filtered out
```

A second identical invocation produced the same omitted rows and failure.

## Fix constraints

- Do not discard card body rows at either the completed-step-specific eight-row cap or the generic 120-row section cap; the requested behavior applies to all card content.
- Preserve display-width-safe line wrapping, card borders, section labels, span styles, and transcript scrolling. Viewport clipping with working scroll controls is distinct from destructive card collapse.
- Do not add an expansion mechanism as a substitute for the requested always-expanded cards.
- Update existing renderer tests that currently encode the eight-row truncation behavior when implementing the product fix; keep the investigator-added regression test unchanged.
- This investigation changes only the regression test and RCA documentation; product code remains unchanged.
