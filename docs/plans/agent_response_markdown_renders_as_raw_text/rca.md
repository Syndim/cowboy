## Bug behavior

Agent response cards display Markdown delimiters as literal text instead of presenting the formatted content. For example, an agent response containing `Implemented **successfully**.` is displayed with the `**` delimiters visible rather than showing `successfully` in bold.

## Root cause

The response payload reaches the TUI without losing information: `AgentProgressKind::Response` is mapped to `WorkflowEventKind::AgentResponse`, and `crates/tui/app/src/app/events.rs` passes that event's `content` to `render_markup`.

`crates/tui/app/src/app/markup.rs::render_markup` is not a general Markdown renderer. It recognizes fenced code blocks, command-looking lines, and backtick-delimited inline code. Every other line is sent to `render_inline_code`; when the line contains no backtick, that function returns the entire source line as one styled span without parsing Markdown delimiters or applying Markdown styles. Therefore `**successfully**` remains literal text. The TUI crate also has no Markdown parser dependency. Card rendering and line wrapping preserve the already-raw span, so they are not the source of the defect.

## Reproduction steps

1. Construct a `WorkflowEventKind::AgentResponse` whose content is `Implemented **successfully**.`.
2. Render it through `render_workflow_event`, the same event-card path used by the TUI transcript.
3. Inspect the rendered response body.
4. Observe that the body contains literal `**` delimiters and has no bold span for `successfully`.

## Regression test

- Test file: `crates/tui/app/src/app/events.rs`
- Test name: `app::events::tests::agent_response_renders_markdown_instead_of_raw_syntax`
- Command: `cargo test -p cowboy app::events::tests::agent_response_renders_markdown_instead_of_raw_syntax -- --exact --nocapture`
- Expected failure before the fix: the rendered body still contains `**`; the assertion rejecting raw delimiters fails before the subsequent assertion can verify `Modifier::BOLD` on `successfully`.

## Current failing result

The focused command was run twice and failed deterministically with exit code 101. The relevant output was:

```text
running 1 test
thread 'app::events::tests::agent_response_renders_markdown_instead_of_raw_syntax' panicked at crates/tui/app/src/app/events.rs:927:9:
│Implemented **successfully**.                                                 │
test app::events::tests::agent_response_renders_markdown_instead_of_raw_syntax ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 172 filtered out
```

## Fix constraints

- Preserve agent response content while converting supported Markdown syntax into terminal text and Ratatui styles; Markdown delimiters must not remain visible as raw syntax.
- Preserve existing fenced-code syntax highlighting, unknown-language fallback styling, inline-code styling, card framing, wrapping, and row-capping behavior.
- Keep Markdown interpretation in the TUI presentation layer; workflow, agent, and persisted event payloads must remain provider-neutral plain strings.
- The regression test must pass by rendering the real `AgentResponse` event path, not by weakening or replacing its assertions.
- Product code is intentionally unchanged during this investigation.
