# RCA: Asking for user input hides the full step output

## Bug behavior

When a workflow step returns status `blocked` (the mechanism by which an agent
step asks the user for direction to unblock), the TUI renders a
`Step completed` card whose `Body` section holds the agent's full "why blocked /
unblock path" explanation. That body is truncated to 8 visual rows, and the
remainder is collapsed into a `… N more rows` marker. As a result, the user does
not see the full context needed to answer the prompt.

Observed card (abridged, from the report):

```
✓ Step completed · ↳ implement · ▶ fc8ca9ad
╭──────────────────────────────────────────────╮
│Action: agent · Status: blocked                │
├─── Body ───────────────────────────────────── ┤
│# Blocked — with evidence and an unblock path  │
│ ...                                            │
│… 79 more rows                                  │
╰──────────────────────────────────────────────╯
```

## Root cause

`crates/tui/app/src/app/events.rs` builds the `StepCompleted` card and caps the
`Body` section to 8 rows unconditionally:

```rust
.section(
    CardSection::named("Body", render_markup(body, style_transcript_normal()))
        .capped(8),
)
```

The cap is applied by `section_wrapped_lines` in
`crates/tui/app/src/app/card.rs`, which keeps `max_lines` visual rows and appends
a `… {omitted} more rows` marker. `CardSection::capped(8)` overrides the default
`SECTION_BODY_LIMIT` (120).

This cap is appropriate for ordinary completed steps, but a step completing with
status `blocked` is precisely the signal that the workflow is about to ask the
user for direction (see `crates/workflow/actions/src/ask_user.rs` and the
`blocked` routing in `crates/workflow/engine/test_files/agent/00-feature.lua`,
where the `blocked` status routes to a step that asks the user). In that case the
body is the context the user must read to respond, so truncating it to 8 rows
hides the information needed to unblock the run.

## Reproduction steps

1. Run a workflow whose agent step can return status `blocked` with a long body
   (more than 8 wrapped rows of explanation), e.g. the feature workflow's
   `implement` step returning `blocked`.
2. Observe the TUI transcript render a `Step completed · Status: blocked` card.
3. Note the `Body` section shows only the first 8 rows followed by
   `… N more rows`, hiding the remaining unblock context before the follow-up
   `Waiting for input` prompt.

Deterministic unit reproduction: render a `WorkflowEventKind::StepCompleted`
event with `status = "blocked"` and a 20-line body via `render_workflow_event`
and inspect the produced text — the body is cut to 8 lines with a
`… 12 more rows` marker.

## Regression test

- Test file path: `crates/tui/app/src/app/events.rs`
- Test name: `blocked_step_completed_body_shows_full_context_for_user_input`
- Command:
  `cargo test -p cowboy --lib blocked_step_completed_body_shows_full_context_for_user_input`
- Expected failure before the fix: the assertions
  `text.contains("blocked body line 20")` and `!text.contains("more rows")` fail
  because the body is capped at 8 rows and a `… 12 more rows` marker is emitted.

The test renders a `StepCompleted` event with `status = Some("blocked")` and a
20-line body, then asserts the card shows the full body (`blocked body line 1`
through `blocked body line 20`) and does not contain a `more rows` truncation
marker.

## Current failing result

```
running 1 test
test app::events::tests::blocked_step_completed_body_shows_full_context_for_user_input ... FAILED

---- app::events::tests::blocked_step_completed_body_shows_full_context_for_user_input stdout ----
✓ Step completed · ↳ implement · ▶ 170dc431
╭──────────────────────────────────────────────────────────────────────────────╮
│Action: agent · Status: blocked                                               │
├─── Body ─────────────────────────────────────────────────────────────────────┤
│blocked body line 1                                                           │
│blocked body line 2                                                           │
│blocked body line 3                                                           │
│blocked body line 4                                                           │
│blocked body line 5                                                           │
│blocked body line 6                                                           │
│blocked body line 7                                                           │
│blocked body line 8                                                           │
│… 12 more rows                                                                │
╰──────────────────────────────────────────────────────────────────────────────╯

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 171 filtered out
```

## Fix constraints

- Fix belongs in the TUI app crate (`crates/tui/app/src/app/events.rs`, with
  supporting rendering in `crates/tui/app/src/app/card.rs`); do not add workflow
  runtime logic to the TUI crates.
- When a step completes with a status that hands control back to the user for
  input (notably `blocked`), the `Body` section must show the full context
  rather than an 8-row cap. A reasonable approach is to not apply the tighter
  `.capped(8)` for blocked/ask-user-bound completions, deferring to the default
  `SECTION_BODY_LIMIT`.
- Preserve the existing behavior for ordinary completed steps (the current
  `renders_waiting_and_completed_cards_with_sections` test still expects the
  8-row cap with a `… N more rows` marker for a non-blocked completed step).
- Keep width-safe truncation of individual rows intact; the fix concerns row
  count (vertical) truncation, not per-line width truncation.
- Do not change product code as part of this investigation; only the regression
  test and this RCA are added here.
- Run the narrowest relevant checks and fix all compiler/Clippy warnings before
  yielding when implementing the fix.
