## Bug behavior

When the TUI transcript/text log has many entries, each typed character in the composer becomes slow to appear. The lag is reproduced in the TUI draw path by rendering a state with typed input and a long transcript: the same 80x24 draw that takes about 2.34 ms with 64 entries takes about 29.74 ms with 20,000 entries.

## Root cause

The keypress loop marks the whole TUI dirty after ordinary input edits, so typing redraws the full screen. During that redraw, `crates/tui/src/app/controls/transcript.rs::lines` builds and wraps the entire transcript before slicing out the visible rows:

- `lines` calls `visual_rows(all_lines(state), wrap_width)`.
- `all_lines` iterates every `state.event_entries()` item and calls `entry.render_lines()`.
- `TranscriptEntry::Workflow::render_lines` re-renders workflow events through `render_workflow_event`.
- `visual_rows` wraps every logical line into visual rows.
- Only after all historical rows are rendered and wrapped does `lines` compute `rows[start..end]` for the visible viewport.

Because typing causes redraws and transcript rendering is proportional to total transcript history, input latency grows with log length instead of remaining bounded by the visible transcript window.

## Reproduction steps

1. Populate `AppState` with many workflow transcript entries.
2. Type composer input with `state.push_input("typed input stays responsive")`.
3. Draw through the existing `Terminal<TestBackend>` and `draw(frame, state)` path at 80x24.
4. Compare a short transcript draw against a long transcript draw.
5. Observe the long transcript redraw exceeding the bounded-redraw budget while the visible output still only needs the tail rows plus composer.

## Regression test

- Test file path: `crates/tui/src/app/tests.rs`
- Test name: `app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history`
- Command: `cargo test -p cowboy app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history -- --exact`
- Expected failure before the fix: the long-transcript typed-input redraw exceeds the budget derived from the short-transcript redraw, showing redraw cost scales with full transcript history.

## Current failing result

```text
running 1 test
failures:

---- app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history stdout ----

thread 'app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history' (498618) panicked at crates/tui/src/app/tests.rs:257:5:
redrawing typed input should render only visible transcript tail rows, not scale with the full transcript; 64 entries took 2.341833ms, 20_000 entries took 29.736875ms, budget was 21.734664ms
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 86 filtered out; finished in 2.13s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Do not move workflow runtime behavior into `crates/tui`; keep this as a TUI rendering/input responsiveness fix.
- Preserve transcript tail visibility, manual scroll offsets, follow-latest behavior, pending prompt rendering, wrapping, and existing styles.
- Avoid re-rendering and re-wrapping the whole transcript on every composer edit; redraw work for typing should be bounded by visible rows or cached transcript rows.
- Keep workflow event contents redacted only by existing rendering rules; do not add sensitive data to diagnostic output or docs.
