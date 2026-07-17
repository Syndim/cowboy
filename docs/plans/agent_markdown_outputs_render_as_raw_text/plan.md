# Plan

Implement the reviewed [RCA](./rca.md) with the broader scope confirmed during plan review: every free-form output payload rendered inside a TUI card must use the existing Markdown renderer, not only `AgentResponse` or completed agent bodies. This supersedes the RCA's narrower non-agent-literal constraint while retaining its diagnosis that card projection selects the wrong rendering mode.

The Markdown implementation already exists and works. `crates/tui/app/src/app/markup.rs::render_content` is a two-mode dispatcher: `ContentFormat::Markdown` invokes `pulldown_cmark` through `MarkdownRenderer`, while `ContentFormat::LiteralWithCodeHighlighting` bypasses Markdown parsing and intentionally keeps delimiters such as `**`. The existing `AgentResponse` test passes because that one branch selects `Markdown`; the unchanged completed-step regression fails because `StepCompleted.body` selects `LiteralWithCodeHighlighting`. The same literal selection is repeated for nearly every other free-form event body in `events.rs` and for pending-prompt/application-card text in `state.rs`. Therefore the defect is not missing or broken Markdown logic—the UI call sites opt out of it.

The previous implementation allowed each card branch to choose a format independently, which created the inconsistent behavior and leaves future branches able to repeat it. Because the confirmed product contract is now “all free-form card output is Markdown,” the fix should encode that invariant in the renderer API instead of changing the existing dispatcher arguments one call at a time. Removing the format parameter and literal branch makes Markdown the only path for prose/output passed to `render_content`; explicitly constructed `Line`/`Span` card chrome remains unaffected.

Apply one consistent rendering contract to workflow-event bodies and application-card details. The inventory in `crates/tui/app/src/app/events.rs` includes step progress, agent prompts, responses, thoughts, tool-update output, agent plan entries, completed-step bodies for every action, waiting-for-input messages, retry reasons, and run-failure reasons. The non-event card paths in `crates/tui/app/src/app/state.rs` include the active pending-prompt message and `TranscriptEntry::Card` detail lines. Structural UI text—card titles, metadata, action/status rows, tool titles/statuses, choice labels, and built-in next-action instructions—remains explicitly styled UI chrome rather than Markdown input.

Keep `crates/tui/app/src/app/events.rs::app::events::tests::agent_step_completed_body_renders_markdown_instead_of_raw_syntax` unchanged as the regression input. Make product code satisfy its delimiter-removal and bold-span assertions; do not rewrite or replace that test. Do not alter workflow event payloads, frontmatter parsing, persisted `StepOutput`, workflow semantics, card framing, wrapping, streaming aggregation, or truncation behavior.

# Changes

- Simplify `crates/tui/app/src/app/markup.rs` so `render_content` always parses Markdown while preserving the caller's base style, fenced-code syntax highlighting, inline-code fallback styling, blank lines in code blocks, links/images/HTML fallbacks, and unsupported-payload behavior.
- Remove `ContentFormat` and the obsolete literal-only rendering branch and helpers once every production caller uses the unified Markdown contract; retain the shared syntax-highlighting machinery used by Markdown code blocks.
- Update every free-form body/output call to `render_content` in `crates/tui/app/src/app/events.rs`, including all `StepCompleted` actions rather than branching on `action == "agent"`.
- Update pending-prompt messages and application-card detail lines in `crates/tui/app/src/app/state.rs` to use the same unified renderer, so duplicate waiting cards and non-workflow transcript cards do not preserve raw Markdown delimiters.
- Leave structural labels and metadata on their current direct `Line`/`Span` paths; these values define card chrome rather than prose output and must keep their status-specific styles.

# Tests to be added/updated

- Preserve the investigator-added `agent_step_completed_body_renders_markdown_instead_of_raw_syntax` test exactly.
- Replace the existing literal step-progress expectation with a Markdown-rendering expectation, and add table-driven event-card coverage for the remaining free-form payload variants: agent prompt, thought, tool update, plan entry, non-agent completed body, waiting message, retry reason, and failure reason. Assertions must verify both removal of Markdown delimiters and the expected Ratatui modifier/style.
- Keep the existing agent-response Markdown and fenced-code tests as controls for streaming output and code highlighting.
- Update pending-prompt and application-card detail tests in `state.rs` so they assert rendered Markdown rather than preserved punctuation.
- Update `markup.rs` tests and call signatures for the single rendering mode. Remove tests that exclusively defend deleted literal behavior, while retaining coverage for Markdown block/inline formatting, base-color composition, fenced Rust/shell/unknown-language highlighting, unterminated or unsupported input fallback, and ordinary plain text.

# How to verify

1. Run the unchanged regression test:
   `cargo test -p cowboy app::events::tests::agent_step_completed_body_renders_markdown_instead_of_raw_syntax -- --exact --nocapture`
2. Run the cross-event Markdown coverage:
   `cargo test -p cowboy app::events::tests::all_free_form_workflow_card_content_renders_markdown -- --exact --nocapture`
3. Run the pending-prompt and application-card checks:
   `cargo test -p cowboy app::state::tests::pending_prompt_message_renders_markdown -- --exact --nocapture`
   `cargo test -p cowboy app::state::tests::application_card_details_render_markdown -- --exact --nocapture`
4. Run the complete affected rendering modules:
   `cargo test -p cowboy app::events::tests -- --nocapture`
   `cargo test -p cowboy app::state::tests -- --nocapture`
   `cargo test -p cowboy app::markup::tests -- --nocapture`
5. Check formatting:
   `cargo fmt --check`
6. Check the affected crate for warnings:
   `cargo clippy -p cowboy --lib --tests -- -D warnings`

# TODO

- [x] Make `render_content` the single Markdown rendering entry point and remove obsolete literal-mode code.
- [x] Route every free-form workflow-event card payload through unified Markdown rendering.
- [x] Route pending-prompt messages and application-card details through unified Markdown rendering.
- [x] Keep structural card chrome and status-specific styling outside Markdown parsing.
- [x] Keep the investigator-added completed-agent regression test unchanged and passing.
- [x] Add cross-event coverage for every free-form workflow card payload category.
- [x] Update pending-prompt and application-card detail tests for rendered Markdown.
- [x] Update markup tests for the single mode while preserving formatting, highlighting, and fallback contracts.
- [x] Run the focused regression and all-output Markdown tests.
- [x] Run the complete affected rendering test modules.
- [x] Run formatting and affected-crate Clippy checks.
