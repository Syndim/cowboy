# Root Cause Analysis

## Bug behavior

The TUI transcript can render an agent tool-update payload as raw JSON instead of a concise progress display. The grounded reproduction uses a synthetic `AgentToolCallUpdate` whose `content` is a JSON-encoded string for a background task update:

```json
{"content":[{"type":"text","text":""}],"details":{"jobs":[{"id":"job-123","type":"task","status":"running","label":"TuiLagRegressionTest","durationMs":123798}]}}
```

The current transcript prints that string directly under the `Agent tool update` header. In repeated updates, the stable fields are the same and only `durationMs` changes, so the transcript fills with near-duplicate raw payloads.

## Root cause

`crates/tui/src/app/events.rs` renders `WorkflowEventKind::AgentToolCallUpdate` bodies through `display_json_value`. That helper treats `serde_json::Value::String` as already-displayable text and returns it unchanged.

ACP/tool backends can deliver structured tool results as JSON-encoded strings. When that happens, the renderer does not parse the string, does not extract `details.jobs[*]`, and does not fall back to a human summary. `AppState` also only coalesces streaming `AgentResponse` and `AgentThought` events, so repeated tool-progress updates are appended as separate transcript entries.

## Reproduction steps

1. Add the regression test in `crates/tui/src/app/events.rs` named `renders_json_encoded_tool_update_content_as_progress_summary`.
2. Run:

```bash
cargo test -p cowboy --lib renders_json_encoded_tool_update_content_as_progress_summary
```

3. Observe that the rendered event body still contains the raw JSON-encoded payload, including `{`, `"details"`, and `"durationMs"`.

## Regression test

- Test file path: `crates/tui/src/app/events.rs`
- Test name: `renders_json_encoded_tool_update_content_as_progress_summary`
- Command: `cargo test -p cowboy --lib renders_json_encoded_tool_update_content_as_progress_summary`
- Expected failure before the fix: the test fails because the rendered output includes raw JSON instead of a concise progress summary containing the synthetic job label `TuiLagRegressionTest` and status `running`.

## Current failing result

```text
running 1 test
failures:

---- app::events::tests::renders_json_encoded_tool_update_content_as_progress_summary stdout ----

thread 'app::events::tests::renders_json_encoded_tool_update_content_as_progress_summary' panicked at crates/tui/src/app/events.rs:499:9:
01:30:48  Agent tool update  step=implement  tool=Running background task  status=completed
{"content":[{"type":"text","text":""}],"details":{"jobs":[{"id":"job-123","type":"task","status":"running","label":"TuiLagRegressionTest","durationMs":123798}]}}

failures:
    app::events::tests::renders_json_encoded_tool_update_content_as_progress_summary

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 88 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Do not print raw JSON for JSON-encoded tool-update strings.
- Preserve useful progress details: at minimum, expose the task label and status from `details.jobs[*]`.
- Avoid showing volatile-only changes such as `durationMs` as a large raw body.
- Keep existing readable tool-output behavior for normal text fields such as `text`, `stdout`, `stderr`, `result`, and `summary`.
- Keep `crates/tui` UI-only; do not move runtime or ACP parsing policy into the TUI crate beyond display normalization.
