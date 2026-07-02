# Plan

Change the TUI transcript renderer from padded `label: value` blocks to compact event headers plus unindented markdown body lines. For each workflow transcript item, put metadata on the first line with the event title and inline key/value pairs. Put long content fields (`message`, `prompt`, `content`, `thought`, `body`, plan entries, and tool output) on the following lines rendered from column 0 through the existing markup/markdown renderer.

The waiting-for-input display should become structurally like:

```text
04:46:26  Waiting for input  step=confirm_result  prompt=result_confirmation_13  choices=<freeform>
Review the implementation summary below. Type 'yes' to approve and commit it, or describe the changes you want before committing.

Approved review:
Step: review
Status: approved
...
```

The same layout must be used when the current pending prompt is rendered as a focused prompt card rather than only as a workflow event, so the duplicated prompt path cannot keep the old left padding.

# Changes

- Update `crates/tui/src/app/events.rs` to add compact header/body rendering helpers:
  - a header helper that starts with the existing timestamp and event title, then appends inline metadata spans such as `step=...`, `prompt=...`, `choices=...`, `status=...`, or `role=...`;
  - a body helper that calls `markup::render_markup` directly so body lines are not prefixed by `message:`, `content:`, or continuation padding.
- Replace padded `field_line` / `push_multiline` rendering for content-heavy workflow events with the compact helpers. Cover these event kinds because they can render long text:
  - `StepProgress` (`message` body);
  - `AgentPrompt` (`prompt` body);
  - `AgentResponse` (`content` body);
  - `AgentThought` (`thought` body);
  - `AgentToolCallUpdate` (`content` body when present);
  - `AgentPlan` (plan entry bodies);
  - `StepCompleted` (`body` body, still respecting its existing line cap);
  - `WaitingForInput` (`message` body and inline `step`, `prompt`, `choices` metadata).
- Keep metadata-only events compact by rendering their fields inline on the first line where practical, without introducing body lines just to preserve the old layout.
- Update `crates/tui/src/app/controls/transcript.rs` so `prompt_card_lines` mirrors the new waiting-for-input event layout:
  - first line: `Waiting for input` plus inline `step=...`, `prompt=...`, and `choices=...` metadata;
  - following lines: `prompt.message()` rendered through `markup::render_markup` starting at column 0;
  - remove the separate padded `message:`, `choices:`, and `Type an answer below...` lines from the prompt card.
- Update `crates/tui/src/app/state.rs` `transcript_line_count` so the pending-prompt contribution is computed from `prompt_card_lines(prompt).len()` or an equivalent shared helper instead of the current fixed `7`, because rendered prompt height will depend on markdown/body line count.
- Preserve existing styles from `crates/tui/src/app/styles.rs`: metadata spans stay metadata-colored, warning/status spans keep their existing semantic colors, and body spans keep the same normal/thought/prompt/plan/tool styles they use today.

# Tests to be added/updated

- Update `crates/tui/src/app/events.rs` tests for `WaitingForInput` to assert:
  - first line contains `Waiting for input`, `step=...`, `prompt=...`, and `choices=<freeform>`;
  - message body starts on the next line without `message:` or leading continuation spaces;
  - markdown/code rendering still works for body text.
- Update content-event tests in `crates/tui/src/app/events.rs` for at least `AgentResponse` and `StepCompleted` to assert long body text is not prefixed by `content:` / `body:` or padded left.
- Update `crates/tui/src/app/controls/transcript.rs` prompt-card tests to assert the focused prompt card uses the same compact first-line metadata and unindented message body.
- Add or update a `state.rs` line-count test for a pending prompt with a multiline markdown message so scrolling uses the new dynamic prompt-card height instead of the old fixed height.
- Keep existing syntax-highlight tests in `markup.rs` unchanged unless the helper signature changes; they should continue to prove fenced code is rendered through the current markup renderer.

# How to verify

- Run `cargo test -p cowboy events::tests` to verify workflow-event rendering changes.
- Run `cargo test -p cowboy controls::transcript::tests` to verify transcript prompt-card rendering changes.
- Run `cargo test -p cowboy state::tests` to verify transcript line counting and prompt state behavior.
- Run `cargo test -p cowboy` as the focused crate regression pass after renderer tests are updated.
- Manual smoke check in the TUI with a workflow that reaches `WaitingForInput` and a multiline approval message:
  - confirm the first line contains the timestamp/title and inline metadata;
  - confirm the message begins at the left edge of the transcript content area on the second line;
  - confirm list items, blank lines, inline code, and fenced code are readable through the markdown renderer;
  - confirm scrolling still follows the latest event and can reveal the full prompt body.

# TODO

- [x] Add compact workflow-event header/body helpers in `crates/tui/src/app/events.rs`.
- [x] Convert content-heavy workflow events in `events.rs` from padded labels to compact headers with unindented markdown bodies.
- [x] Convert metadata-only workflow events in `events.rs` to inline first-line metadata where practical.
- [x] Update waiting-for-input event rendering to put `step`, `prompt`, and `choices` on the first line and render the message from column 0.
- [x] Update `prompt_card_lines` in `crates/tui/src/app/controls/transcript.rs` to match the waiting-for-input event layout.
- [x] Update pending-prompt transcript line counting in `crates/tui/src/app/state.rs` to use dynamic rendered prompt height.
- [x] Update event renderer tests for compact metadata and unindented body content.
- [x] Update transcript prompt-card tests for compact metadata and unindented body content.
- [x] Add or update state line-count coverage for multiline pending prompts.
- [x] Run focused TUI renderer/state tests.
- [x] Run the full `cowboy` crate test suite.
- [x] Manually smoke test a multiline waiting-for-input prompt in the TUI.

# Manual smoke evidence

Observed in a PTY smoke run of `target/debug/cowboy` with a temporary workflow that reaches `WaitingForInput` and a multiline markdown prompt:

- First line contained timestamp/title/inline metadata:

```text
│05:30:19  Waiting for input  step=confirm_result  prompt=result_confirmation_13  choices=<freeform> │
```

- Message body began at the left edge on the next line, with no `message:` label or continuation padding:

```text
│Review the implementation summary below.
│Approved review:
```

- Markdown remained readable, including list item, inline code, fenced code marker, and fenced code body:

```text
│- `inline code` item
│```rust
│fn main() { println("hi"); }
```

- Blank lines rendered as visual separation between prose, list, and fenced-code sections.
- Scrolling/follow behavior was observed: follow-latest showed `tail line 08`, `PgUp` revealed the `Waiting for input` header and top body, and `PgDn` returned to `tail line 08`.
- Smoke event artifact: `/tmp/cowboy-tui-review-smoke-observed-hdwg33zi/state/events/run-ef450a46-4000-48e3-b8e6-015fa06f0857.json`.
