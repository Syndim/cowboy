# Plan

Improve the Cowboy TUI transcript with OMP-style visual cards while keeping workflow runtime data, persisted event logs, CLI output, and agent execution contracts unchanged. The work stays in `crates/tui/app`: workflow events already arrive as typed `WorkflowEventKind` values, current TUI code renders them through `app/events.rs`, stores them as `TranscriptEntry` values in `app/state.rs`, and displays them inside an outer bordered `Paragraph` in `app/controls/transcript.rs`.

The revised layout removes the main transcript/content border. Header, status, and composer remain. The middle content area becomes open space containing individual cards; the cards provide the visual grouping instead of the global transcript box.

Reuse the current icon-based chrome from `crates/tui/app/src/app/controls/chrome.rs` instead of inventing a parallel label scheme. Card titles and card metadata should use the same compact glyph vocabulary already used by the status bar, with the same ` · ` separator and Unicode display-width truncation behavior.

Icon contract to reuse from current chrome:

| Meaning | Existing glyph |
|---|---:|
| idle | `○` |
| running / active | `●` |
| waiting | `◔` |
| retrying | `↻` |
| completed / success | `✓` |
| failed / error | `✗` |
| cancelled | `■` |
| unknown state | `?` |
| current step | `↳ {step}` |
| active run | `▶ {short_run_id}` |
| workflow | `⎇ {workflow_name}` |
| background tasks | `◷ {count}` |
| separator | ` · ` |

Do not render card-title metadata as `step=`, `run=`, `workflow=`, `tasks=`, or status words when an existing icon covers the concept. Text labels remain acceptable inside body sections where they are user-facing content, for example a prompt body can still contain `Role: developer` if that text came from the prompt.

Overall screen mockup:

```text
Cowboy · Add health route

✓ • Read docs/plans/example.md · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│  1 # Plan                                                      │
│  2                                                            │
│  3 Update the workflow runtime...                             │
│    … 42 more lines                                            │
├─── Output ─────────────────────────────────────────────────────┤
│ Read 45 lines from docs/plans/example.md                       │
╰────────────────────────────────────────────────────────────────╯

● Agent response · ↳ implement
╭────────────────────────────────────────────────────────────────╮
│ Implemented the TUI card renderer and updated transcript rows. │
╰────────────────────────────────────────────────────────────────╯

● · ↳ implement · ▶ 170dc431 · ⎇ bugfix · ◷ 1
┌ Run active · type draft, Enter waits · Esc cancels ────────────┐
│ >                                                              │
└────────────────────────────────────────────────────────────────┘
```

Use the OMP design primitives that matter for this request:

- rounded card chrome (`╭╮╰╯`, `│`, `├┤`) for transcript entries;
- a compact icon-first title row containing status glyph, human title, and high-value metadata;
- named sections such as `Output`, `Prompt`, `Body`, `Choices`, or `Next action` when a card has multiple content areas;
- bounded previews for long content with a visible truncation line instead of raw walls of text;
- width-safe wrapping using visual width, not byte length;
- muted borders, accent/status colors, and readable text from the existing Cowboy transcript palette.

Do not copy OMP internals or introduce a theme system. Implement a small Cowboy-specific card renderer that consumes existing Ratatui `Line`/`Span` values and returns width-safe framed rows. The renderer should be reusable by workflow events, command/status cards from `AppState::push_card`, and the pending-prompt display.

Element mockups to implement:

```text
Main content area with no outer transcript border

Cowboy · Active run topic

● Run started · ↳ plan · ▶ 170dc431 · ⎇ bugfix
╭────────────────────────────────────────────────────────────────╮
╰────────────────────────────────────────────────────────────────╯

● Step started · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
╰────────────────────────────────────────────────────────────────╯

● · ↳ implement · ▶ 170dc431 · ⎇ bugfix
┌ Composer keeps its own border ─────────────────────────────────┐
│ > user input                                                   │
└────────────────────────────────────────────────────────────────┘
```

```text
Tool call / tool update card

● • Bash cargo test -p cowboy · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ running 23 tests                                               │
│ test app::events::tests::renders_tool_card ... ok              │
│   … 18 more lines                                              │
├─── Output ─────────────────────────────────────────────────────┤
│ command still running                                          │
╰────────────────────────────────────────────────────────────────╯

✓ • Bash cargo test -p cowboy · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ test result: ok                                                │
├─── Output ─────────────────────────────────────────────────────┤
│ command completed successfully                                 │
╰────────────────────────────────────────────────────────────────╯
```

```text
Agent thought card

● Agent thinking · ↳ plan · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ Checking where transcript rendering is flattened today.        │
╰────────────────────────────────────────────────────────────────╯
```

```text
Agent response card

● Agent response · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ Updated `controls/transcript.rs` to render cards directly.     │
│                                                                │
│ ```rust                                                        │
│ frame.render_widget(transcript, area);                         │
│ ```                                                            │
╰────────────────────────────────────────────────────────────────╯
```

```text
Agent plan card

● Agent plan · ↳ plan · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ - Add icon-first card renderer                                 │
│ - Remove main content border                                   │
│ - Preserve tail rendering                                      │
╰────────────────────────────────────────────────────────────────╯
```

```text
Prompt sent to agent card

● Prompt sent to agent · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
├─── Prompt ─────────────────────────────────────────────────────┤
│ Role: implementer                                              │
│ Task: improve transcript visual grouping                       │
╰────────────────────────────────────────────────────────────────╯
```

```text
Waiting for input card

◔ Waiting for input · ↳ review · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ Review implementation and choose an outcome.                   │
├─── Choices ────────────────────────────────────────────────────┤
│ approve · request_changes · reject                             │
╰────────────────────────────────────────────────────────────────╯
```

```text
Step completed card

✓ Step completed · ↳ review · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
├─── Body ───────────────────────────────────────────────────────┤
│ Implementation approved.                                       │
╰────────────────────────────────────────────────────────────────╯
```

```text
Run failed card

✗ Run failed · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ Missing required YAML frontmatter.                             │
├─── Next action ────────────────────────────────────────────────┤
│ List resolvable statuses with /resolve <run>.                  │
╰────────────────────────────────────────────────────────────────╯
```

```text
Compact lifecycle/progress/status cards

● Agent session ready · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
╰────────────────────────────────────────────────────────────────╯

● Step progress · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ Running formatter on changed Rust files.                       │
╰────────────────────────────────────────────────────────────────╯

↻ Step retrying · ↳ implement · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ attempt 2/3                                                    │
│ Agent response was missing required frontmatter.                │
╰────────────────────────────────────────────────────────────────╯

✓ Manually resolved · ↳ review · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
╰────────────────────────────────────────────────────────────────╯

✓ Run completed · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
╰────────────────────────────────────────────────────────────────╯

■ Run cancelled · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
╰────────────────────────────────────────────────────────────────╯

◔ Run status changed · ▶ 170dc431
╭────────────────────────────────────────────────────────────────╮
│ waiting                                                        │
╰────────────────────────────────────────────────────────────────╯
```

```text
Slash/command feedback card

✗ Error
╭────────────────────────────────────────────────────────────────╮
│ usage: unmatched quote in slash command                        │
╰────────────────────────────────────────────────────────────────╯

◔ Notice
╭────────────────────────────────────────────────────────────────╮
│ no active background task                                      │
╰────────────────────────────────────────────────────────────────╯
```

# Changes

- Reuse the latest icon-based chrome contract from `crates/tui/app/src/app/controls/chrome.rs`:
  - use `status_icon` semantics for all card leading glyphs: `○`, `●`, `◔`, `↻`, `✓`, `✗`, `■`, and `?`;
  - use existing metadata glyphs in card titles: `↳ {step}`, `▶ {short_run_id}`, `⎇ {workflow_name}`, and `◷ {count}`;
  - use the existing ` · ` separator between title metadata parts;
  - reuse or move `short_run_id` and `truncate_to_display_width` so cards, status, and header do not maintain competing icon/width implementations;
  - avoid new text labels for metadata already represented by icons.
- Add a TUI-local card rendering module, likely `crates/tui/app/src/app/card.rs` or `crates/tui/app/src/app/transcript_card.rs`:
  - define `CardTone` or equivalent for neutral, accent, success, warning, error, thought, prompt, plan, and tool states;
  - define a `CardSection` structure with a section label and styled body lines;
  - render icon-first title rows immediately above each rounded card;
  - render full-width rounded borders using `╭`, `╮`, `╰`, `╯`, `│`, `├`, `┤`, and `─`;
  - truncate card titles and metadata to the available visual width with `unicode_width`, preserving valid Unicode boundaries;
  - wrap styled body spans inside the card interior instead of relying on a later generic transcript wrap to break borders;
  - include a truncation marker such as `… N more lines` for capped sections.
- Remove the main transcript/content border from `crates/tui/app/src/app/controls/transcript.rs`:
  - stop wrapping the transcript `Paragraph` in `Block::default().borders(Borders::ALL)`;
  - compute visible height from `area.height` instead of `area.height.saturating_sub(2)`;
  - compute content width from `area.width` instead of `area.width.saturating_sub(2)`;
  - keep header, status strip, and composer borders unchanged.
- Refactor transcript entry rendering to be width-aware:
  - replace or supplement `TranscriptEntry::render_lines()` with a width-aware path used by `controls/transcript.rs`;
  - keep `TranscriptEntry::plain_text()` for status, search, and existing state checks;
  - preserve the current bounded-tail behavior for large histories, especially `bounded_tail_visual_rows()` and `stream_event_tail_visual_rows()` performance guarantees;
  - ensure card rendering only processes enough entries/rows for the visible viewport plus scroll offset.
- Convert workflow event rendering in `crates/tui/app/src/app/events.rs` from inline rows to icon-first card-shaped render data:
  - implement the mockups above for run lifecycle, step lifecycle, progress, session ready, prompt, thought, response, plan, waiting input, step completed, retrying, manual resolution, run failed, run cancelled, run completed, and run status changed events;
  - map event state to leading icons through the shared status icon contract, not ad hoc glyphs;
  - preserve the existing Markdown/code highlighting from `app/markup.rs` inside card bodies;
  - keep timestamps out of card titles unless needed for a future explicit time-display change; runtime state remains visible through the existing status bar metadata;
  - keep step ids, short run ids, workflow names, and task counts visible as icon metadata when useful, but avoid noisy internal ids such as tool call ids.
- Make tool calls look like OMP tool cards while following Cowboy's current icons:
  - use titles like `● • Read artifact://… · ↳ implement · ▶ 170dc431` for pending tools and `✓ • Read artifact://… · ↳ implement · ▶ 170dc431` for completed tools;
  - keep the `•` tool marker from OMP only as the tool-call marker after the status icon;
  - render tool update content under an `Output` section when content is available;
  - keep the existing JSON summarization behavior in `display_tool_update_content()` so raw JSON blobs do not leak into the card body;
  - coalesce `AgentToolCall` and the matching `AgentToolCallUpdate` by `(run_id, tool_call_id)` in the TUI projection so the transcript shows one evolving card rather than separate call/update rows.
- Update `crates/tui/app/src/app/state.rs` to support the new projection:
  - extend active-event coalescing to merge an `AgentToolCallUpdate` into the previous matching `AgentToolCall` entry when it is the current active tool card;
  - preserve streaming append behavior for `AgentResponse` and `AgentThought` chunks;
  - keep pending prompt detection/de-duplication working after the waiting prompt becomes a card;
  - continue hiding local config/state paths in idle rendering and avoid adding resolved private paths to static UI text.
- Update `crates/tui/app/src/app/controls/transcript.rs`:
  - render width-aware card rows directly into the open main content viewport;
  - remove imports and logic that only exist for the outer transcript `Block` border;
  - keep chronological order, blank spacing between entries, follow-latest behavior, scrolling, and narrow-terminal handling;
  - preserve the optimization that rendering typed input does not scale with the full transcript history.
- Refresh visual copy and symbols in existing non-workflow cards:
  - render `Usage`, `Error`, `Exit`, `Improve`, `Resolve`, `Cancelled`, and `Notice` entries through the same icon-first card renderer;
  - map `Error` to `✗`, `Cancelled` to `■`, warning-like notices to `◔`, success-like cards to `✓`, and neutral cards to `○`;
  - use concise labels and one-card bodies rather than the current title plus fixed indentation format;
  - keep slash suggestion and composer behavior unchanged except for any test expectation updates caused by card-shaped transcript rows.
- Keep scope intentionally local:
  - no changes to `cowboy-workflow-engine`, workflow event schemas, persisted event logs, redb storage, Lua workflow logic, ACP transport, or command parser behavior;
  - no new dependency unless Ratatui lacks a safe helper needed for span wrapping; prefer the existing `unicode_width` dependency already used by the TUI.

# Tests to be added/updated

- Add or update tests for the shared icon contract:
  - status-to-icon mapping covers `○`, `●`, `◔`, `↻`, `✓`, `✗`, `■`, and `?`;
  - card metadata uses `↳`, `▶`, `⎇`, and `◷` with the same ` · ` separator as the status bar;
  - card title truncation reuses Unicode display-width behavior and does not split wide characters.
- Add unit tests for the new card renderer:
  - rounded top/bottom borders use the expected glyphs;
  - title rows include the status icon, title, and icon metadata when width allows;
  - title rows do not include old metadata labels such as `step=`, `run=`, `workflow=`, or `tasks=`;
  - long titles and metadata truncate safely at narrow widths;
  - multiline body content wraps inside `│ … │` borders without overwide rows;
  - section dividers render as `├─── Output ─…┤` when a section label is present;
  - capped long sections show an `… N more lines` marker;
  - rendered rows never exceed the requested visual width.
- Update `crates/tui/app/src/app/events.rs` tests:
  - each `WorkflowEventKind` still exposes the same important text as before;
  - every mockup category in the Plan section has renderer coverage;
  - event card titles use the shared leading status icon instead of words like `running`, `waiting`, `failed`, or `completed` as metadata;
  - tool call/update rendering produces an icon-first `•` tool card title and `Output` section without raw JSON or tool call ids;
  - waiting input, run failed, completed, cancelled, thought, plan, prompt, and tool statuses carry their expected styles;
  - Markdown/code-highlighted body spans survive inside card interiors.
- Update `crates/tui/app/src/app/state.rs` tests or add focused tests for transcript projection:
  - consecutive `AgentResponse` chunks still merge;
  - consecutive `AgentThought` chunks still merge;
  - an `AgentToolCallUpdate` with the same `tool_call_id` updates the visible tool card instead of appending a separate tool update card;
  - unrelated tool updates or non-adjacent updates do not corrupt prior transcript entries.
- Update `crates/tui/app/src/app/controls/transcript.rs` tests:
  - existing chronological ordering still holds with cards;
  - pending prompts remain visible and de-duplicated;
  - narrow-width rendering keeps latest tail content and complete card borders;
  - scroll offsets still operate on wrapped visual rows;
  - styled spans survive clipping/wrapping inside card bodies;
  - the main content viewport no longer renders an outer `┌┐└┘` transcript border.
- Update draw-level tests in `crates/tui/app/src/app/tests.rs`:
  - transcript rows include rounded card glyphs for workflow/tool events;
  - transcript card titles include the same status/metadata icons used by the status bar;
  - idle draw still hides local debug/config paths;
  - active-run composer and status strip behavior remain unchanged;
  - main content has no outer transcript border while composer still has its border;
  - long-history redraw performance remains bounded and keeps the latest visible tail.

# How to verify

- Run `cargo fmt -p cowboy --check`.
- Run `cargo test -p cowboy app::controls::chrome` if the shared icon helpers gain direct tests.
- Run `cargo test -p cowboy app::card` or the actual new card module test path.
- Run `cargo test -p cowboy app::events`.
- Run `cargo test -p cowboy app::state` for coalescing and pending-prompt projection tests.
- Run `cargo test -p cowboy app::controls::transcript`.
- Run `cargo test -p cowboy app::controls::status` to prove the existing icon row was not regressed.
- Run `cargo test -p cowboy app::tests`.
- Run `cargo test -p cowboy` after targeted tests pass.
- Manually smoke-test `cargo run -p cowboy` in a terminal:
  - start a workflow that emits a prompt, agent thought, tool call, tool update output, agent response, and completion;
  - confirm the main content area has no enclosing transcript border;
  - confirm card titles reuse the current icons: status glyph first, `↳` for step, `▶` for run, `⎇` for workflow, `◷` for tasks, and ` · ` separators;
  - confirm card titles do not show the old `step=`, `run=`, `workflow=`, or `tasks=` label style;
  - confirm tool activity appears as a bordered card with status icon, `•` tool marker, `Output` section, status color, and no raw internal JSON;
  - confirm the other planned elements match their mockups: lifecycle cards, thought card, response card, plan card, prompt card, waiting-input card, completion card, failed-run card, and command feedback card;
  - resize to a narrow width and confirm card borders, wrapping, icon metadata truncation, scrolling, composer, and status strip remain usable.

# TODO

- [x] Reuse or expose the existing icon/metadata helpers from `controls/chrome.rs` for transcript cards.
- [x] Add a width-aware rounded card renderer in the TUI app crate.
- [x] Add card tones/styles wired to the existing transcript palette.
- [x] Implement icon-first card title rows using `○`, `●`, `◔`, `↻`, `✓`, `✗`, `■`, and `?`.
- [x] Implement card metadata using `↳`, `▶`, `⎇`, `◷`, and ` · ` separators.
- [x] Remove old `step=`, `run=`, `workflow=`, and `tasks=` metadata labels from card titles.
- [x] Implement safe visual-width truncation for card titles and metadata.
- [x] Implement styled body wrapping inside card borders.
- [x] Implement named card sections and long-section truncation markers.
- [x] Remove the outer main transcript/content border from `controls/transcript.rs`.
- [x] Recompute transcript visible height and width without subtracting outer border space.
- [x] Refactor transcript entry rendering to use a width-aware render path.
- [x] Preserve plain-text transcript rendering for status/search checks.
- [x] Convert workflow event rendering to icon-first card-shaped render data.
- [x] Implement card renderings for all mockup categories in the Plan section.
- [x] Render agent tool calls and updates as OMP-style tool cards using Cowboy's current icons.
- [x] Render tool update content under an `Output` section.
- [x] Preserve existing tool update content summarization and raw JSON suppression.
- [x] Coalesce matching tool call and tool update events in the TUI projection.
- [x] Preserve response and thought streaming coalescing.
- [x] Keep pending prompt de-duplication working with card output.
- [x] Render `AppState::push_card` entries through the same icon-first card renderer.
- [x] Update transcript viewport rendering for width-aware card rows.
- [x] Preserve narrow-terminal scrolling, tail clipping, and follow-latest behavior.
- [x] Preserve long-history redraw performance characteristics.
- [x] Add shared icon contract tests for status icons, metadata icons, separators, and Unicode truncation.
- [x] Add card renderer unit tests for borders, icons, truncation, wrapping, sections, width, and truncation markers.
- [x] Update workflow event renderer tests for all mockup categories, icon-first titles, card text, styles, and tool output sections.
- [x] Add or update state tests for tool-card coalescing and streaming coalescing.
- [x] Update transcript control tests for border removal, card layout, prompt visibility, scroll offsets, and span preservation.
- [x] Update draw-level TUI tests for no main content border, current icon reuse, rounded card glyphs, and unchanged composer/status behavior.
- [x] Run the targeted TUI test commands and full `cargo test -p cowboy`.
- [x] Manually smoke-test the card UI, current icon reuse, and borderless main content in an interactive terminal.

Reviewer follow-up evidence: actual interactive `cargo run -p cowboy` smoke was run in a PTY with a temporary smoke-test workspace containing a deterministic config, workflow, and ACP stub. The scenario submitted `/run --workflow smoke smoke cards`, resized the PTY during the run, answered the waiting prompt, and captured TUI output showing run start, `Agent thinking`, `✓ • Read artifact://28`, `Output`, `diff output`, `Agent response`, `Waiting for input`, `approve · reject`, `Run completed`, `↳ implement`, `▶`, `⎇ smoke`, rounded `╭`/`╰` cards, and no old `step=`, `run=`, `workflow=`, or `tasks=` labels.
