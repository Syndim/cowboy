## Bug behavior

Markdown-bearing transcript cards fail in two independent ways. An agent-response card containing ordinary Markdown text separated by one source newline renders `first line second line` on one terminal row. Separately, a synthetic agent-response card containing a GitHub-Flavored Markdown table renders the header, delimiter row, and data row on one terminal line and exposes the raw `|` and `---` syntax. Inline emphasis inside the table payload is parsed, so the event payload reaches the Markdown renderer intact.

## Root cause

`crates/tui/app/src/app/events.rs::workflow_event_card` correctly sends `AgentResponse.content` through `render_content`, so the event-to-card callsite is not bypassing Markdown rendering.

The failure is inside `crates/tui/app/src/app/markup.rs`:

- For ordinary Markdown, `pulldown-cmark` emits `Event::SoftBreak` for a single source newline inside a paragraph. `MarkdownRenderer::handle_event` deliberately replaces every soft break with one space, so `first line\nsecond line` becomes one rendered row before card framing.
- `render_content` enables only `Options::ENABLE_STRIKETHROUGH` and `Options::ENABLE_TASKLISTS`. It does not enable `Options::ENABLE_TABLES`, so the parser treats GFM table source as ordinary paragraph text rather than emitting table structure. The table's source newlines are consequently also emitted as soft breaks and replaced with spaces.
- The renderer currently ignores `Tag::Table`, `Tag::TableHead`, `Tag::TableRow`, and `Tag::TableCell` boundaries in both `start_tag` and `end_tag`. Therefore enabling table parsing alone would still not produce distinct terminal rows or cell separation.

The card framing and wrapping layer is downstream of this loss. `CardSection` receives one line containing the raw table tokens and only width-wraps that already-collapsed line. As a control, the existing `markdown_formats_block_boundaries_prefixes_and_breaks` test passes and explicitly confirms the current soft-break-to-space policy.

## Reproduction steps

1. Construct a `WorkflowEventKind::AgentResponse` whose ordinary Markdown content is `first line\nsecond line`.
2. Render it through `render_workflow_event`, the transcript card path used by the TUI, and observe both values on the same terminal row.
3. Independently construct another `AgentResponse` containing an ordinary paragraph, a three-line GFM table, bold content in one cell, and a trailing paragraph.
4. Render the table scenario through the same card path and observe that the table header, delimiter, and value collapse into one row containing raw `| --- | --- |` syntax.
5. Run the focused regression command and observe one failure report containing both independently rendered cards.

## Regression test

- Test file: `crates/tui/app/src/app/events.rs`
- Test name: `app::events::tests::markdown_cards_preserve_line_breaks_and_render_tables`
- Command: `cargo test -p cowboy app::events::tests::markdown_cards_preserve_line_breaks_and_render_tables -- --exact --nocapture`
- Expected failure before the fix: the combined assertion reports both independent defects. The ordinary payload renders `first line second line` on one row, while the table payload renders its header and value on one row containing raw `| --- | --- |` syntax.

## Current failing result

The revised focused command was run three times and failed deterministically with exit code 101. The relevant output from the final run was:

```text
running 1 test
thread 'app::events::tests::markdown_cards_preserve_line_breaks_and_render_tables' panicked at crates/tui/app/src/app/events.rs:1145:9:
ordinary Markdown lines should occupy distinct rows: ● Agent response · ↳ implement · ▶ 170dc431
╭──────────────────────────────────────────────────────────────────────────────╮
│first line second line                                                        │
╰──────────────────────────────────────────────────────────────────────────────╯

Markdown table should render as distinct rows without raw delimiter syntax: ● Agent response · ↳ implement · ▶ 170dc431
╭──────────────────────────────────────────────────────────────────────────────╮
│Summary                                                                       │
│| Item | State | | --- | --- | | first | done |                               │
│Next line                                                                     │
╰──────────────────────────────────────────────────────────────────────────────╯
test app::events::tests::markdown_cards_preserve_line_breaks_and_render_tables ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 231 filtered out

error: test failed, to rerun pass `-p cowboy --lib`
```

The control command `cargo test -p cowboy app::markup::tests::markdown_formats_block_boundaries_prefixes_and_breaks -- --exact --nocapture` passed.

## Fix constraints

- Keep Markdown interpretation in the TUI presentation layer; do not alter workflow events, agent payloads, persistence, or workflow semantics.
- Preserve single source newlines as terminal row boundaries for Markdown-bearing card content instead of converting them to spaces.
- Parse and render GFM table structure into distinct rows and cells without exposing the source delimiter row. Enabling `Options::ENABLE_TABLES` alone is insufficient because table tags currently have no rendering behavior.
- Preserve card framing, width-aware wrapping, base styles, nested inline styles, fenced-code syntax highlighting, links, images, lists, block quotes, hard breaks, and unsupported-content fallbacks.
- Apply the behavior through the existing shared `render_content` path so workflow cards, pending prompts, and application-card details do not acquire competing Markdown conventions.
- Keep the investigator-added regression test unchanged; product code must satisfy its distinct-row, raw-syntax, and bold-span assertions.
- Product code is intentionally unchanged during this investigation.
