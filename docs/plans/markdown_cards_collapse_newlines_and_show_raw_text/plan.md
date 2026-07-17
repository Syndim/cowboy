# Plan

Use the reviewed root-cause analysis in `docs/plans/markdown_cards_collapse_newlines_and_show_raw_text/rca.md` and the existing failing regression test `crates/tui/app/src/app/events.rs::markdown_cards_preserve_line_breaks_and_render_tables` as the implementation contract. Fix the shared TUI Markdown renderer so every current `render_content` caller receives consistent line and table handling; do not change workflow events, payloads, persistence, card framing, or workflow behavior.

Implement the renderer change in three parts: preserve ordinary Markdown soft breaks as terminal row boundaries, enable GitHub-Flavored Markdown table parsing, and translate parsed table boundaries into terminal rows and visually separated cells while retaining the renderer's existing span styles. Keep the investigator-added regression test unchanged and make product code satisfy it.

Test the shared renderer along two independent axes rather than duplicating every Markdown fixture across every card: a construct matrix in `markup.rs` will verify every supported Markdown rendering behavior, while event/state card tests will verify every free-form content surface routes through that shared renderer with its intended base style. This provides complete coverage without a brittle card-by-syntax cross-product.

# Changes

- Update `crates/tui/app/src/app/markup.rs::render_content` to include `pulldown_cmark::Options::ENABLE_TABLES` alongside the existing strikethrough and task-list options.
- Change `MarkdownRenderer::handle_event` so `Event::SoftBreak` finishes the current terminal line instead of appending a space. Preserve the existing image-alt fallback behavior by treating a break inside image alt text as spacing within the label rather than discarding or splitting the label.
- Add minimal table-row rendering state to `MarkdownRenderer`:
  - Start table head and body rows on clean line boundaries and reset the current cell index.
  - Insert a generated, base-styled cell separator between cells without re-emitting source pipe or delimiter-row syntax.
  - Finish each table-head/body row as a distinct `ratatui::text::Line` and close the table on a line boundary.
  - Keep cell text on the normal `append_text` path so emphasis, strong text, links, code, base colors, and other nested inline styles remain intact; apply any table-header styling through the existing modifier stack rather than replacing cell styles.
- Keep table layout deliberately streaming and width-agnostic. The existing card layer remains responsible for width-aware wrapping, avoiding a second layout or padding system in the Markdown parser.
- Do not change `workflow_event_card` or other `render_content` callsites. The shared renderer change must cover agent response/thought/progress cards, prompts, application-card details, and pending-prompt rendering uniformly.

# Tests to be added/updated

- Keep `crates/tui/app/src/app/events.rs::markdown_cards_preserve_line_breaks_and_render_tables` unchanged. It remains the investigator-added end-to-end regression for distinct ordinary source rows, parsed table rows without raw delimiter syntax, and a preserved bold cell span inside an agent-response card.
- Shared `render_content` construct tests in `crates/tui/app/src/app/markup.rs` (exact test names for review):
  - Existing `plain_text_uses_base_style`: plain text retains the caller's base style.
  - Add `empty_markdown_returns_one_empty_line`: empty input remains renderable as one empty terminal line.
  - Existing `markdown_composes_nested_inline_styles_with_base_color`: emphasis, strong, nested strong/emphasis, and strikethrough remove source delimiters and compose modifiers with the base color.
  - Update `markdown_formats_block_boundaries_prefixes_and_breaks`: headings, paragraphs, ordered/unordered/task lists, block quotes, horizontal rules, soft breaks, and hard breaks retain their styles/prefixes and use the new row-boundary contract.
  - Existing `markdown_preserves_links_images_html_and_payload_fallbacks`: different-label and same-label links, non-empty/empty images, inline HTML, and fallback payload styling remain readable.
  - Add `markdown_preserves_multiline_image_alt_spacing`: an image alt soft break renders as one space inside the image label, never a joined word or separate card row.
  - Add `markdown_renders_block_html_math_and_footnote_fallbacks`: block HTML plus inline math, display math, and footnote payload events remain visible with fallback styling rather than disappearing.
  - Existing `markdown_reuses_code_highlighting_without_delimiters`: inline code, known-language fenced code, terminal/shell aliases, unknown-language fenced code, and indented code remove Markdown delimiters and use syntax or fallback styling as appropriate.
  - Existing `markdown_preserves_blank_lines_in_highlighted_fenced_code`: multiline fenced code retains internal blank rows.
  - Existing `markdown_highlights_unterminated_fenced_code_without_delimiters`: unterminated fenced code remains visible and highlighted without exposing the fence.
  - Add `markdown_renders_gfm_tables_as_distinct_styled_rows`: table header and multiple body rows are distinct; cells are visibly separated; the source delimiter row is absent; following text starts on a clean row; and emphasis, strong, inline-code, and link styles survive inside cells.
- Workflow-event card tests in `crates/tui/app/src/app/events.rs` (split the current aggregate `all_free_form_workflow_card_content_renders_markdown` into these exact, independently reported tests; share a private assertion helper to avoid duplicated setup):
  - Add `step_progress_card_renders_markdown_lines_and_styles` for `StepProgress.message` (the workflow-progress/status text surface).
  - Add `agent_prompt_card_renders_markdown_lines_and_styles` for `AgentPrompt.prompt` (the prompt sent to the agent).
  - Keep `agent_response_renders_markdown_instead_of_raw_syntax`, `markdown_cards_preserve_line_breaks_and_render_tables`, and `response_fenced_rust_gets_syntect_styles_inside_card` for `AgentResponse.content` (agent response/reply cards).
  - Add `agent_thought_card_renders_markdown_lines_and_styles` for `AgentThought.content` (agent thinking cards).
  - Add `agent_tool_update_output_card_renders_markdown_lines_and_styles` for `AgentToolCallUpdate.content` after JSON/text extraction.
  - Add `agent_plan_entry_card_renders_markdown_lines_and_styles` for each `AgentPlan.entries` value after display conversion.
  - Add `step_completed_body_card_renders_markdown_lines_and_styles` for `StepCompleted.body`, while retaining `agent_step_completed_body_renders_markdown_instead_of_raw_syntax` for the agent-completion variant.
  - Add `waiting_for_input_card_renders_markdown_lines_and_styles` for `WaitingForInput.message`.
  - Add `step_retrying_reason_card_renders_markdown_lines_and_styles` for `StepRetrying.reason`.
  - Add `run_failed_reason_card_renders_markdown_lines_and_styles` for `RunFailed.reason`.
- State-owned card tests in `crates/tui/app/src/app/state.rs` (exact existing names):
  - Update `pending_prompt_message_renders_markdown` for the persistent pending-prompt card.
  - Update `application_card_details_render_markdown` for locally generated application-card detail rows.
- Every named Markdown-bearing surface test will use a compact multiline Markdown fixture and assert: source newlines become distinct terminal rows; source delimiters are absent; inline bold styling composes with that surface's base style; and the surrounding card title/metadata remains intact. Table syntax is exercised once at the shared renderer and once through the unchanged agent-response regression instead of repeated across every surface.
- Retain `renders_lifecycle_cards_with_icon_metadata`, `renders_agent_prompt_thought_response_and_plan_cards`, `renders_tool_cards_with_output_sections_and_suppressed_json`, `renders_waiting_and_completed_cards_with_sections`, and `applies_key_event_styles_inside_cards` for non-free-form card structure and lifecycle/status variants. Add `renders_agent_prompt_window_opened_and_closed_cards_with_metadata` for the currently uncovered `AgentPromptWindowOpened` and `AgentPromptWindowClosed` variants. The complete non-Markdown-field mapping is: `RunStarted`, `StepStarted`, `AgentSessionReady`, `ManuallyResolved`, `RunCompleted`, `RunCancelled`, and `RunStatusChanged` in the lifecycle test; prompt-window opened/closed in the new prompt-window test; and `AgentToolCall` in the tool-card test. These fields are identifiers, state, or metadata rather than Markdown-bearing free-form content, so tests must verify their card titles, metadata, state styles, and absence of leaked raw field labels instead of artificially parsing them as Markdown.

# How to verify

Run the focused regression and renderer tests first, then formatting and warning checks:

```bash
cargo test -p cowboy app::events::tests::markdown_cards_preserve_line_breaks_and_render_tables -- --exact --nocapture
cargo test -p cowboy app::markup::tests -- --nocapture
cargo test -p cowboy app::events::tests -- --nocapture
cargo test -p cowboy app::state::tests::pending_prompt_message_renders_markdown -- --exact --nocapture
cargo test -p cowboy app::state::tests::application_card_details_render_markdown -- --exact --nocapture
cargo fmt --check
cargo clippy -p cowboy --lib --tests -- -D warnings
```

The event regression must change from its current deterministic failure to a pass without editing that test. The renderer tests must confirm the new shared behavior while guarding existing Markdown features, and Clippy must report no warnings.

# TODO

- [x] Enable GFM table parsing in the shared `render_content` parser options.
- [x] Preserve ordinary Markdown soft breaks as terminal line boundaries without regressing image-alt rendering.
- [x] Render parsed table heads, rows, and cells as styled terminal lines without raw source delimiter syntax.
- [x] Add or update every explicitly named shared-renderer construct test in `Tests to be added/updated`, covering plain/empty input, inline styles, blocks, lists, quotes, rules, soft/hard breaks, links, images, HTML/math/footnote fallbacks, all existing code modes, and GFM tables.
- [x] Add `markdown_preserves_multiline_image_alt_spacing` to verify image-alt soft breaks remain space-separated.
- [x] Add `markdown_renders_gfm_tables_as_distinct_styled_rows` for header/body row boundaries, multiple rows, cell separation, post-table content, delimiter removal, and nested inline styling.
- [x] Split the aggregate workflow-card Markdown test into the explicitly named tests for step progress, agent prompt, agent thought, tool update output, agent plan entry, step-completed body, waiting-for-input message, retry reason, and failure reason; retain all named agent-response regressions unchanged.
- [x] Update the explicitly named pending-prompt and application-card tests to assert multiline row preservation, parsed inline styles, and intact card structure.
- [x] Retain the named lifecycle/status/tool-call card tests and add `renders_agent_prompt_window_opened_and_closed_cards_with_metadata` so every event variant without a Markdown-bearing free-form field has explicit card-structure coverage.
- [x] Run the focused event regression, renderer tests, formatter check, and warning-denying Clippy command; fix any failures without modifying the investigator-added repro test.
