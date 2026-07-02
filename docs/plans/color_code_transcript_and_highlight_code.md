# Plan

Refine the Cowboy TUI transcript so event types are visually distinct under dark terminals, using an OMP-inspired palette: normal text stays white, thought text becomes muted gray, workflow state uses status colors, and tool activity uses pending/success/error colors. Use a real Rust syntax-highlighting stack for code snippets and shell commands instead of maintaining Cowboy-specific token colors or a handwritten command lexer.

Grounding from OMP: its theme model separates `text`, `thinkingText`, `muted`, `accent`, `success`, `warning`, `error`, tool colors, Markdown colors, and syntax colors. Cowboy currently has only generic helpers in `crates/tui/src/app/styles.rs`, and transcript rendering flattens every `WorkflowEvent` to `String` in `crates/tui/src/app/events.rs`/`state.rs`, then converts those strings to unstyled `Line`s in `crates/tui/src/app/controls/transcript.rs`. The implementation should keep event persistence/domain data unchanged, apply transcript item colors only in the TUI projection, and delegate syntax token colors to `syntect`/`syntect-tui`.

Concrete dark-mode transcript palette for non-syntax UI items:

| Item | Color | Ratatui style |
|---|---:|---|
| Normal transcript text / agent response prose | `#FFFFFF` | `Color::White` |
| Secondary metadata labels, timestamps, run ids, sessions | `#6B7280` | `Color::Rgb(107, 114, 128)` |
| Thought headers and thought body | `#565F89` | `Color::Rgb(86, 95, 137)` |
| Active/running/accent headers | `#7AA2F7` | `Color::Rgb(122, 162, 247)` |
| Tool call title / pending tool status | `#2AC3DE` | `Color::Rgb(42, 195, 222)` |
| Completed/success status | `#9ECE6A` | `Color::Rgb(158, 206, 106)` |
| Waiting/suspended/warning status | `#E0AF68` | `Color::Rgb(224, 175, 104)` |
| Failed/error/cancelled status | `#F7768E` | `Color::Rgb(247, 118, 142)` |
| Agent plan entries | `#BB9AF7` | `Color::Rgb(187, 154, 247)` |
| Prompt/request content sent to agent | `#9AA5CE` | `Color::Rgb(154, 165, 206)` |
| Borders/dividers/code fence border | `#3B4261` | `Color::Rgb(59, 66, 97)` |
| Inline-code fallback when syntax highlighting is unavailable | `#C0CAF5` | `Color::Rgb(192, 202, 245)` |

Syntax-highlight colors for fenced code blocks, inline code, and shell commands should come from `syntect`'s bundled theme data, converted to Ratatui spans with `syntect-tui`. Default to `base16-ocean.dark` for dark terminals. Do not define Cowboy-owned syntax token colors for keywords, strings, comments, numbers, operators, or shell flags.

# Changes

- Extend `crates/tui/src/app/styles.rs` from coarse status helpers into a small transcript palette:
  - keep existing `style_accent`, `style_muted`, `style_success`, `style_warning`, `style_error`, and border helpers for current callers;
  - add named helpers for transcript normal text, metadata, thought, plan, prompt, tool pending, code fallback, and non-syntax command labels;
  - use `Color::Rgb` for the non-syntax transcript palette above so dark-mode rendering is consistent across terminals that support truecolor.
- Add real syntax highlighting dependencies to `crates/tui/Cargo.toml`:
  - add `syntect = "5"` for syntax sets, language detection, and bundled themes;
  - add `syntect-tui = "3"` because it targets `ratatui 0.29` and converts `syntect` highlight segments into `ratatui::text::Span` values.
- Add a TUI-local transcript markup/highlighting module:
  - load `SyntaxSet::load_defaults_newlines()` and `ThemeSet::load_defaults()` once with `std::sync::OnceLock`;
  - use `ThemeSet::themes["base16-ocean.dark"]` as the default syntax theme;
  - resolve fenced language tags through `SyntaxSet::find_syntax_by_token`, then extension lookup, then plain-text fallback;
  - highlight each code line with `syntect::easy::HighlightLines` and convert segments through `syntect_tui::into_span`;
  - never emit raw ANSI escape strings into Ratatui lines.
- Replace string-only transcript projection with styled transcript projection:
  - introduce a TUI-local `TranscriptEntry`/`RenderedTranscriptEntry` representation that can preserve `WorkflowEventKind` until render time;
  - keep runtime event persistence and `cowboy-workflow-engine::WorkflowEvent` unchanged;
  - keep ad-hoc cards from `AppState::push_card` supported as plain/styled card entries.
- Update `crates/tui/src/app/events.rs` so workflow event rendering can produce `Vec<Line<'static>>` with spans instead of only one flattened `String`:
  - timestamp and field labels use metadata gray;
  - high-level event titles use the event-specific color;
  - `AgentThought` title and body use thought gray;
  - `AgentResponse` content uses normal white for prose and `syntect`/`syntect-tui` for code spans/blocks;
  - `AgentPrompt` content uses prompt muted text for prose and `syntect`/`syntect-tui` for code spans/blocks;
  - `AgentToolCall`/`AgentToolCallUpdate` use tool cyan for titles/pending, green for completed, yellow for warning-like states, red for failed/error states, and highlighted code/command output when the content is code-like;
  - `AgentPlan` uses purple for plan labels and the same markup/highlighting renderer for entry text;
  - `WaitingForInput` keeps warning yellow title and white prompt message body;
  - run completed/failed/cancelled map to success/error colors.
- Update `crates/tui/src/app/state.rs` to store typed/stylable transcript entries instead of final rendered strings where needed:
  - preserve streaming append semantics for consecutive `AgentResponse` and `AgentThought` chunks;
  - keep `status` as a plain string for header/status bar compatibility, deriving it from the same event render text;
  - expose transcript entries through an API that `controls/transcript.rs` can render into styled `Line`s.
- Update `crates/tui/src/app/controls/transcript.rs` to render styled `Line`s directly:
  - stop converting every event line through `line.to_string()`;
  - keep existing chronological ordering, blank-line spacing, scroll behavior, pending-prompt de-duplication, and wrapping behavior;
  - style the empty state and pending prompt card with the new palette.
- Add syntax highlighting for Markdown code snippets and commands in transcript content:
  - detect fenced code blocks with optional language tags, inline code spans, and indented command blocks inside agent responses, step bodies, plans, prompts, and tool output;
  - pass fenced `bash`, `sh`, `shell`, `zsh`, `console`, and `terminal` blocks to `syntect`'s shell syntax instead of a Cowboy-specific command-color implementation;
  - treat command-looking transcript lines such as `$ cargo test`, `cargo run -- ...`, and `/run ...` as shell/plain command snippets only when they are clearly standalone commands;
  - use the inline-code fallback color only if `syntect` cannot highlight the snippet.
- Keep styling local to the TUI crate:
  - no changes to workflow core, Lua, store, catalog, or agent execution contracts;
  - no persisted schema migration;
  - no behavioral change to CLI command output unless a shared helper currently forces it.

# Tests to be added/updated

- Add unit tests in `crates/tui/src/app/styles.rs` or the new transcript renderer module verifying the concrete color mapping for non-syntax transcript items: thought, normal text, metadata, tool pending, success, warning, error, plan, prompt, and code fallback.
- Add syntax renderer tests using `syntect`/`syntect-tui` for fenced Rust code, fenced shell code, inline code fallback, unknown-language fallback, and unterminated code fences.
- Add command highlighting tests proving shell fences and clear standalone commands are routed through `syntect` shell/plain syntax, not a Cowboy-owned token-color lexer.
- Update `crates/tui/src/app/events.rs` tests so each `WorkflowEventKind` still renders the expected text while also asserting key spans carry the expected style:
  - `AgentThought` body is gray;
  - `AgentResponse` prose body is white;
  - fenced Rust in an `AgentResponse` produces at least two distinct non-default syntect-derived styles;
  - `WaitingForInput` title is warning yellow;
  - failed run title is red;
  - completed run title is green;
  - tool completed status is green and pending status is cyan.
- Add transcript control tests in `crates/tui/src/app/controls/transcript.rs` ensuring styled lines survive `lines(state, max_visible_lines)` without being flattened to unstyled strings.
- Update existing chronological-order, prompt-card, streaming-response, and empty-state tests to keep their text assertions passing under the styled renderer.
- Add one draw-level snapshot-style test in `crates/tui/src/app/tests.rs` if existing test utilities can inspect styles; otherwise keep style assertions in renderer unit tests and use draw-level coverage only for text/layout regression.

# How to verify

- Run `cargo fmt -p cowboy --check`.
- Run `cargo test -p cowboy app::styles` if style tests live in `styles.rs`.
- Run `cargo test -p cowboy app::events`.
- Run `cargo test -p cowboy app::controls::transcript`.
- Run `cargo test -p cowboy app::tests` for draw-level TUI regressions.
- Run `cargo test -p cowboy` after the targeted tests pass.
- Manually smoke-test `cargo run -p cowboy` in a dark terminal:
  - start a workflow that emits agent thought, response, tool call, plan, waiting input, completed, and failed/cancelled events;
  - confirm thoughts are gray, normal prose is white, status/title colors match the palette, and metadata is dimmer than content;
  - paste or generate a response containing a Rust fenced code block and a shell command block, and confirm `syntect` highlighting appears through Ratatui spans without raw ANSI escape text.

# TODO

- [x] Add concrete non-syntax transcript palette helpers to `crates/tui/src/app/styles.rs`.
- [x] Add `syntect` and `syntect-tui` dependencies to `crates/tui/Cargo.toml`.
- [x] Add a TUI-local syntax/markup renderer using `SyntaxSet`, `ThemeSet`, `HighlightLines`, and `syntect_tui::into_span`.
- [x] Load syntax sets and the `base16-ocean.dark` theme once with `std::sync::LazyLock`.
- [x] Route fenced code blocks, inline code, and command-looking standalone lines through the syntax/markup renderer.
- [x] Use syntect shell syntax for `bash`, `sh`, `shell`, `zsh`, `console`, and `terminal` fences instead of handwritten command token colors.
- [x] Introduce a typed/stylable transcript entry representation in the TUI crate.
- [x] Preserve streaming append behavior for typed `AgentResponse` and `AgentThought` entries.
- [x] Convert workflow event rendering from flattened `String` output to styled `Line` output.
- [x] Keep a plain-text status string derived from rendered events for header/status uses.
- [x] Update transcript rendering to keep styled spans instead of calling `to_string()` on every event line.
- [x] Apply event-specific colors for run, step, prompt, thought, response, tool, plan, waiting, success, warning, error, and metadata fields.
- [x] Style empty-state and pending-prompt transcript cards with the same palette.
- [x] Add or update style mapping tests for all concrete non-syntax transcript item colors.
- [x] Add syntax renderer tests for Rust fences, shell fences, inline code fallback, unknown languages, and unterminated fences.
- [x] Add command routing tests for cargo commands, slash commands, shell fences, and non-command prose.
- [x] Add or update workflow event renderer tests for styled thoughts, responses, tools, waiting input, success, failure, and syntect-highlighted code.
- [x] Add transcript control tests proving styled spans survive clipping/scrolling.
- [x] Run targeted TUI tests and full `cargo test -p cowboy`.
- [x] Smoke-test the dark-terminal transcript colors and syntect-based code/command highlighting with draw-level and renderer coverage.
