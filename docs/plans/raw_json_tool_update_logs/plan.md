## Plan

Base the fix on `docs/plans/raw_json_tool_update_logs/rca.md` and keep `crates/tui/src/app/events.rs::renders_json_encoded_tool_update_content_as_progress_summary` as the primary regression input. The failing behavior is local to TUI event presentation: `AgentToolCallUpdate.content` can arrive as a JSON-encoded string, `display_json_value` treats that string as literal display text, and repeated progress payloads differ only by volatile fields such as `durationMs`.

Fix the display at the TUI boundary. Parse JSON-encoded tool-update strings only for rendering, extract human progress from known structured shapes, and keep raw protocol payloads out of the transcript. For `details.jobs[*]`, render a concise progress summary that includes the job label and status, and intentionally omits volatile implementation details such as job ids and duration counters.

Also coalesce consecutive updates for the same tool call while they remain the latest transcript entry. The user should see one updating progress entry for a repeated background-task status stream, not many near-duplicate entries. Keep this coalescing in `crates/tui/src/app/state.rs`; do not move workflow runtime or ACP parsing policy into lower-level crates.

## Changes

- In `crates/tui/src/app/events.rs`, add a tool-update-specific display helper instead of using the generic `display_json_value` directly for `AgentToolCallUpdate.content`.
- For `serde_json::Value::String` tool-update content, trim and attempt `serde_json::from_str::<serde_json::Value>`; when parsing succeeds, render the parsed value instead of the outer string.
- Preserve existing normal text rendering when a string is not valid JSON.
- Preserve existing extraction for textual fields such as `text`, `content`, `message`, `output`, `stdout`, `stderr`, `result`, and `summary`.
- Add structured progress extraction for `details.jobs[*]`, formatting each job as a short label/status line. Prefer `label` for the visible name, fall back to a generic job type only when no label is present, and do not render `durationMs`, ids, or raw `details` JSON.
- Keep the fallback for unrecognized structured values as `<structured tool result>` rather than dumping raw JSON.
- In `crates/tui/src/app/state.rs`, extend active-event coalescing to consecutive `AgentToolCallUpdate` events with the same run id, step id, and tool call id while the active entry is still the transcript tail.
- For coalesced tool updates, replace the stored event with the latest update rather than appending content, so status/title/content reflect the current tool progress without accumulating duplicate bodies.
- Preserve existing streaming append behavior for `AgentResponse` and `AgentThought` events.
- Do not change workflow event schemas, persisted run state, ACP client parsing, or runtime event emission.

## Tests to be added/updated

- Keep the investigator-added repro test unchanged: `crates/tui/src/app/events.rs::renders_json_encoded_tool_update_content_as_progress_summary`.
- Add or update a focused renderer unit test in `crates/tui/src/app/events.rs` for structured `details.jobs[*]` content supplied as a direct JSON object, asserting the label and status render and raw JSON fields do not.
- Add a focused state unit test in `crates/tui/src/app/state.rs` for consecutive `AgentToolCallUpdate` events with the same tool call id and content that differs only by `durationMs`; assert the transcript has one tool-update entry, contains the concise label/status summary, and does not contain raw JSON or `durationMs`.
- Keep existing tests for `renders_tool_update_content`, `renders_tool_updates_without_ids_and_with_parsed_nested_content`, and response/thought stream coalescing passing; do not rewrite them around the new implementation.

## How to verify

- Confirm the repro is red before the implementation if needed:
  - `cargo test -p cowboy --lib renders_json_encoded_tool_update_content_as_progress_summary`
- After the fix, run the repro test:
  - `cargo test -p cowboy --lib renders_json_encoded_tool_update_content_as_progress_summary`
- Run the TUI event renderer tests:
  - `cargo test -p cowboy --lib app::events::tests`
- Run the TUI state tests that cover stream/coalescing behavior:
  - `cargo test -p cowboy --lib app::state::tests`
- If a focused command fails, inspect whether the change broke one of these contracts: normal text extraction, nested `content` text extraction, raw JSON suppression, volatile field omission, or active transcript coalescing.

## TODO

- [x] Read `docs/plans/raw_json_tool_update_logs/rca.md` and inspect the existing failing repro test before editing.
- [x] Add a tool-update-specific display helper in `crates/tui/src/app/events.rs`.
- [x] Parse JSON-encoded string payloads for tool updates and render the parsed structure when parsing succeeds.
- [x] Preserve plain non-JSON string rendering for normal tool-update text.
- [x] Preserve existing extraction for textual fields such as `text`, `content`, `message`, `output`, `stdout`, `stderr`, `result`, and `summary`.
- [x] Extract `details.jobs[*]` summaries with visible job label and status.
- [x] Omit volatile/raw structured fields such as `durationMs`, job ids, and `details` JSON from rendered tool-update bodies.
- [x] Keep unrecognized structured tool-update payloads on the existing `<structured tool result>` fallback.
- [x] Extend active transcript coalescing in `crates/tui/src/app/state.rs` for consecutive updates of the same tool call.
- [x] Replace the active tool-update transcript event with the latest update instead of appending duplicate bodies.
- [x] Preserve existing append semantics for `AgentResponse` and `AgentThought` streaming events.
- [x] Add or update the direct-object `details.jobs[*]` renderer unit test in `crates/tui/src/app/events.rs`.
- [x] Add the consecutive tool-update coalescing unit test in `crates/tui/src/app/state.rs`.
- [x] Run `cargo test -p cowboy --lib renders_json_encoded_tool_update_content_as_progress_summary`.
- [x] Run `cargo test -p cowboy --lib app::events::tests`.
- [x] Run `cargo test -p cowboy --lib app::state::tests`.
