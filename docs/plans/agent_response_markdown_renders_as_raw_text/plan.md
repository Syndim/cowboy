## Plan

Continue from the reviewed implementation described by the approved [RCA](./rca.md), but replace the two ambiguous module-level renderer functions with one explicit rendering contract before further implementation changes. `crates/tui/app/src/app/markup.rs` will expose `render_content(text, base_style, ContentFormat)`, where `ContentFormat::LiteralWithCodeHighlighting` preserves the pre-fix lightweight renderer and `ContentFormat::Markdown` uses the implemented Pulldown Cmark renderer.

Only `WorkflowEventKind::AgentResponse` selects `ContentFormat::Markdown`. Every other current caller selects `ContentFormat::LiteralWithCodeHighlighting`; this is an interface migration, not permission to reinterpret those payloads as CommonMark. The investigator-added `crates/tui/app/src/app/events.rs::agent_response_renders_markdown_instead_of_raw_syntax` remains unchanged as the fixed regression input and must continue to pass through the real agent-response card path.

Keep workflow events, agent output, and persisted payloads as provider-neutral strings. Rendering mode is presentation metadata selected at each TUI callsite; it is not added to workflow-domain or persisted types.

## Changes

- Define `pub(super) enum ContentFormat { LiteralWithCodeHighlighting, Markdown }` and a single `pub(super) fn render_content(text, base_style, format)` entrypoint in `crates/tui/app/src/app/markup.rs`.
  - Dispatch internally to clearly named private helpers for the two implementations.
  - Remove the module-level `render_markup` and `render_markdown` entrypoints after every caller and unit test has migrated; do not leave aliases or compatibility wrappers.
  - Keep `pulldown-cmark` `0.13`, `Parser::new_ext`, and exactly `Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS`; do not enable additional extensions.
- Preserve `LiteralWithCodeHighlighting` behavior byte-for-byte at the rendered-text level:
  - ordinary lines and Markdown punctuation remain literal;
  - inline backticks remain visible while their enclosed content uses `style_transcript_code_fallback`;
  - fenced-code delimiters remain visible in the border style, fenced bodies keep Syntect or unknown-language fallback styling, and unterminated fences continue through end of input;
  - standalone shell and slash-command detection continues to syntax-highlight recognized command lines;
  - caller-provided base styles continue to apply to ordinary text.
- Preserve the implemented `Markdown` behavior for agent responses:
  - strong, emphasis, and strikethrough add modifiers without replacing the caller's base foreground;
  - inline backticks and fenced-code delimiters are hidden, while code content reuses the existing Syntect/fallback helpers and preserves blank source lines;
  - a standalone command not marked as inline or fenced code is ordinary Markdown text and is not heuristically command-highlighted;
  - paragraphs and block endings create deterministic lines, soft breaks become spaces, hard breaks create lines, and headings, lists, task items, block quotes, and thematic rules retain their implemented terminal forms;
  - links render their label plus a distinct destination, images render alt-text placeholders without fetching, raw HTML stays literal in the fallback code style, and unhandled container tags retain child content while unhandled textual payloads use the fallback code style.
- Migrate every production callsite explicitly:
  - `ContentFormat::Markdown`: only the `WorkflowEventKind::AgentResponse` body in `crates/tui/app/src/app/events.rs`.
  - `ContentFormat::LiteralWithCodeHighlighting` in `events.rs`: step progress, agent prompts, agent thoughts, tool-call update output, agent plan entries, completed-step bodies, waiting-for-input messages, retry reasons, and run-failure reasons.
  - `ContentFormat::LiteralWithCodeHighlighting` in `crates/tui/app/src/app/state.rs`: pending-prompt messages and application-card detail lines.
- Leave `Card`, `CardSection`, transcript wrapping, section row caps, blocked-body expansion, and caller-selected base styles unchanged. Both content formats continue to return `Vec<Line<'static>>` before the existing card framing/wrapping/capping stage.
- Do not change workflow-engine events, agent payload construction, persistence, or non-TUI crates. The dependency and Markdown implementation already present remain in scope only as required by the agent-response fix.

## Tests to be added/updated

- Keep `crates/tui/app/src/app/events.rs::agent_response_renders_markdown_instead_of_raw_syntax` unchanged. It must continue to prove raw `**` delimiters disappear and the enclosed response text receives `Modifier::BOLD`.
- Update existing `markup.rs` tests to call `render_content` with an explicit `ContentFormat`; preserve all current assertions for Markdown structure, unsupported-content fallback, base-style composition, syntax highlighting, and fenced-code blank lines.
- Replace the ambiguous test name `step_progress_keeps_legacy_markup_rendering` with `step_progress_uses_literal_with_code_highlighting`, preserving its assertions that `**working**` stays literal and is not bold. The documented intended behavior above authorizes only this naming/interface update, not a semantic change.
- Add `app::markup::tests::literal_mode_preserves_markdown_delimiters_and_highlights_commands` to exercise the actual renderer rather than only `is_command_line`: verify Markdown punctuation and inline backticks remain visible, and a recognized standalone command receives non-fallback syntax styling.
- Add state-level coverage for both non-event callsite categories: pending-prompt messages and application-card details containing Markdown punctuation must remain literal under `LiteralWithCodeHighlighting`.
- Retain the existing event-level fenced-Rust agent-response test and the focused blank-line regression. Agent response is the only caller category whose visible semantics change, so the unchanged investigator test plus these response tests provide its event-level coverage; non-response callers retain their previous contract and are guarded by the literal-mode unit, event, and state tests.
- Run the full `cowboy` library tests after the focused tests to exercise all migrated callsites and existing card wrapping/row-cap behavior.

## How to verify

1. Run the unchanged investigator regression:
   `cargo test -p cowboy app::events::tests::agent_response_renders_markdown_instead_of_raw_syntax -- --exact --nocapture`
2. Run the agent-response code-block regressions:
   `cargo test -p cowboy app::events::tests::response_fenced_rust_gets_syntect_styles_inside_card -- --exact --nocapture`
   `cargo test -p cowboy app::markup::tests::markdown_preserves_blank_lines_in_highlighted_fenced_code -- --exact --nocapture`
3. Run the explicit-mode isolation tests:
   `cargo test -p cowboy app::events::tests::step_progress_uses_literal_with_code_highlighting -- --exact --nocapture`
   `cargo test -p cowboy app::markup::tests::literal_mode_preserves_markdown_delimiters_and_highlights_commands -- --exact --nocapture`
   `cargo test -p cowboy app::state::tests -- --nocapture`
4. Run the focused renderer and event suites:
   `cargo test -p cowboy app::markup::tests -- --nocapture`
   `cargo test -p cowboy app::events::tests -- --nocapture`
5. Run the complete TUI library target:
   `cargo test -p cowboy --lib`
6. Check formatting, warnings, and Rust diagnostics:
   `cargo fmt --check`
   `cargo clippy -p cowboy --lib --tests -- -D warnings`
   Run LSP diagnostics for `crates/tui/app/src/app/markup.rs`, `crates/tui/app/src/app/events.rs`, and `crates/tui/app/src/app/state.rs`.
7. Confirm no ambiguous renderer callsites remain and inspect the focused diff:
   search `crates/tui/app/src` for `render_markup|render_markdown`, expecting no module-level callsites;
   `git status --short && git diff -- Cargo.lock crates/tui/app/Cargo.toml crates/tui/app/src/app/markup.rs crates/tui/app/src/app/events.rs crates/tui/app/src/app/state.rs docs/plans/agent_response_markdown_renders_as_raw_text`

## TODO

- [x] Add `pulldown-cmark` `0.13` with only strikethrough and task-list extensions.
- [x] Implement and verify agent-response Markdown rendering, unsupported-content policies, nested base styles, and shared code highlighting.
- [x] Keep the investigator-added agent-response regression unchanged and make it pass.
- [x] Add `ContentFormat` and the single `render_content` entrypoint with private, explicitly named mode implementations.
- [x] Migrate every `events.rs` caller to an explicit format, selecting Markdown only for `AgentResponse`.
- [x] Migrate pending-prompt and application-card detail callers in `state.rs` to `LiteralWithCodeHighlighting`.
- [x] Remove the ambiguous module-level `render_markup` and `render_markdown` functions without compatibility aliases.
- [x] Update renderer tests to use explicit formats and rename the step-progress scope-isolation test without changing its assertions.
- [x] Add focused literal-mode command/punctuation coverage and state-level literal-content coverage.
- [x] Run the unchanged regression, code-block regressions, isolation tests, module suites, full TUI library tests, formatting, Clippy, and LSP diagnostics.
- [x] Review the final diff for complete caller migration, unchanged card/persistence behavior, focused scope, and absence of sensitive data.
