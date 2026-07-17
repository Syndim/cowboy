## Bug behavior

The card UI renders the canonical completed output from an agent step as raw Markdown. For example, a completed agent body containing `Completed **successfully**.` displays the `**` delimiters instead of showing `successfully` in bold. Streaming `AgentResponse` cards already render the same syntax correctly, so agent output presentation is inconsistent across cards.

## Root cause

Agent prompts explicitly require valid YAML frontmatter followed by a Markdown body. `crates/workflow/agent/src/frontmatter.rs::parse_frontmatter_output` removes the frontmatter and stores the remaining Markdown unchanged in `StepOutput.body`. `crates/workflow/engine/src/events.rs::WorkflowEvent::step_completed_kind` then copies that body into `WorkflowEventKind::StepCompleted.body`, retaining the associated action name.

In `crates/tui/app/src/app/events.rs::workflow_event_card`, `AgentResponse.content` is passed to `render_content` with `ContentFormat::Markdown`, but every `StepCompleted.body` is passed with `ContentFormat::LiteralWithCodeHighlighting`. The branch does not use `action == "agent"` to select the body format. Literal mode deliberately preserves Markdown punctuation, so the final agent body reaches the card renderer intact but remains raw. The existing agent-response regression passes, which confirms that the Markdown renderer and card span wrapping work; the defect is the completed-step format selection.

## Reproduction steps

1. Construct a `WorkflowEventKind::StepCompleted` with `action` set to `agent` and `body` set to `Completed **successfully**.`.
2. Render the event through `render_workflow_event`, the same event-card path used by the TUI transcript.
3. Locate the completed-step body line.
4. Observe that the rendered line still contains `**successfully**` and does not expose a bold `successfully` span.

## Regression test

- Test file: `crates/tui/app/src/app/events.rs`
- Test name: `app::events::tests::agent_step_completed_body_renders_markdown_instead_of_raw_syntax`
- Command: `cargo test -p cowboy app::events::tests::agent_step_completed_body_renders_markdown_instead_of_raw_syntax -- --exact --nocapture`
- Expected failure before the fix: the completed agent body retains the raw `**` delimiters, so the assertion expecting `Completed successfully.` fails before the bold-span assertion.

## Current failing result

The focused command was run twice and failed deterministically with exit code 101. The relevant output was:

```text
running 1 test
thread 'app::events::tests::agent_step_completed_body_renders_markdown_instead_of_raw_syntax' panicked at crates/tui/app/src/app/events.rs:895:9:
│Completed **successfully**.                                                   │
test app::events::tests::agent_step_completed_body_renders_markdown_instead_of_raw_syntax ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 228 filtered out

error: test failed, to rerun pass `-p cowboy --lib`
```

As a control, the existing agent-response Markdown test passed:

```text
cargo test -p cowboy app::events::tests::agent_response_renders_markdown_instead_of_raw_syntax -- --exact --nocapture
```

## Fix constraints

- Render Markdown-bearing agent output in cards through the existing TUI Markdown renderer; do not alter the provider-neutral event payloads, parsed `StepOutput`, persistence, or workflow semantics.
- At minimum, the canonical `StepCompleted.body` for `action == "agent"` must render as Markdown. Preserve literal rendering for non-agent actions whose bodies are not contractually Markdown.
- Inventory the other agent-originated card payloads before implementation so the requirement to render all agent Markdown output is applied consistently without reinterpreting user prompts, workflow status text, command output, or tool output as Markdown.
- Preserve card framing, wrapping, full-body behavior, base colors, fenced-code syntax highlighting, inline-code styling, and unsupported-content fallbacks.
- Keep the investigator-added regression test unchanged; make product code satisfy its raw-delimiter and bold-span assertions.
- Product code is intentionally unchanged during this investigation.
