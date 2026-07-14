## Bug behavior

When composer input reaches the right edge of the input box, a word that would fit on the next visual row is split between rows instead of moving intact to the continuation row. At a 16-column terminal width, `hello bananas` is rendered as `hello banana` followed by `s`, making the word appear cut at the line boundary.

## Root cause

The composer has two different wrapping paths in `crates/tui/app/src/app/controls/composer.rs`:

- `height` asks a Ratatui `Paragraph` configured with `Wrap { trim: false }` for its wrapped line count.
- `render` calls `rendered_input`, which calls the application-owned `wrapped_input_lines` function and then gives the resulting strings to a `Paragraph` as explicit lines.

`wrapped_input_lines` iterates one Unicode scalar at a time and starts a continuation row only when adding the next character would exceed `content_width`. It tracks display-cell width, but it does not track whitespace or the start of the current word. The renderer therefore fills all 12 available content cells with `hello banana` and moves only the final `s` to the next row. Because the fragments have already become separate `Line` values, the rendering path cannot move the complete word to the continuation row.

The content-width calculation is not the cause: the observed split occurs exactly after the 12 content cells available inside the border and two-cell prompt. The defect is the character-boundary-only policy in the application-owned wrapper, compounded by line counting and rendering using different wrapping implementations.

## Reproduction steps

1. Create an idle `AppState` and enter `hello bananas` in the composer.
2. Draw the full TUI through `Terminal<TestBackend>` at 16 columns by 10 rows.
3. Inspect the two composer content rows.
4. Observe `> hello banana` on the first row and `  s` on the continuation row instead of `> hello` followed by `  bananas`.
5. Repeat the focused test; it fails deterministically with the same rendered buffer.

## Regression test

- Test file path: `crates/tui/app/src/app/tests.rs`
- Test name: `app::tests::draw_moves_whole_word_to_continuation_row`
- Command: `cargo test -p cowboy app::tests::draw_moves_whole_word_to_continuation_row -- --exact --nocapture`
- Expected failure before the fix: the full TUI rendering assertion fails because `bananas`, which fits on a continuation row, is split into `banana` and `s` at the input-box boundary.

## Current failing result

```text
running 1 test
thread 'app::tests::draw_moves_whole_word_to_continuation_row' panicked at crates/tui/app/src/app/tests.rs:254:5:
expected the whole word on the continuation row; rendered rows:
Cowboy
  > /run investi
gate failing tes
ts
  > /workflows
○
┌ Enter submits┐
│> hello banana│
│  s           │
└──────────────┘
test app::tests::draw_moves_whole_word_to_continuation_row ... FAILED

failures:
    app::tests::draw_moves_whole_word_to_continuation_row

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 172 filtered out
error: test failed, to rerun pass `-p cowboy --lib`
```

The same focused command was run twice and produced the same split and failing assertion both times.

## Fix constraints

- Change only TUI composer layout/rendering behavior; do not move workflow runtime behavior into the TUI crate.
- Preserve explicit newline handling, Unicode display widths, the first-row prompt, continuation indentation, input clipping, slash-suggestion budgeting, and cursor placement.
- Preserve character-level fallback for a single word wider than the available content width so long unbroken input remains visible.
- Make height calculation, rendered visual rows, clipping, and cursor coordinates follow one consistent wrapping policy.
- Keep the regression test unchanged while fixing product code.
