## Plan

Update TUI chrome separators from dash-like separator glyphs to the middle dot separator ` · `. Treat this as a rendering-only change in the `cowboy` TUI app crate: no workflow runtime, command parsing, persistence, or event semantics should change.

Scope the replacement to visible UI separators that separate chrome fields or shortcut phrases:

- Header topic separator: `Cowboy - <topic>` becomes `Cowboy · <topic>`.
- Compact status metadata separator: `icon ─ step ─ run ─ workflow ─ tasks` becomes `icon · step · run · workflow · tasks`.
- Composer title separators between shortcut phrases use ` · `.

Do not replace hyphens that are part of content or key names, such as `Shift/Ctrl-Enter`, `run-id`, slash command usage, Markdown bullets in transcript content, or user/workflow-provided text.

## Changes

- `crates/tui/app/src/app/controls/header.rs`
  - Change the active-topic title format from `Cowboy - {topic}` to `Cowboy · {topic}`.
  - Keep idle rendering as `Cowboy`.
  - Preserve display-width truncation via the existing helper.

- `crates/tui/app/src/app/controls/chrome.rs`
  - Change the compact metadata `SEPARATOR` constant from ` ─ ` to ` · `.
  - Preserve the existing metadata ordering, width calculation, priority-based part dropping, and final truncation behavior.

- `crates/tui/app/src/app/controls/composer.rs`
  - Change separator glyphs in composer title strings from ` ─ ` to ` · `.
  - Keep key-name hyphens unchanged in `Shift/Ctrl-Enter`.
  - Preserve the existing title branches for idle, active-run draft mode, and waiting-for-input mode.

- `crates/tui/app/src/app/commands.rs`
  - Update command-path assertions that verify the runtime-supplied topic appears in the header.
  - Do not change command dispatch, runtime task spawning, or topic lifecycle behavior.

- `crates/tui/app/src/app/tests.rs`
  - Update render-level expectations that look for compact status metadata with the old separator.
  - Keep assertions focused on UI output, not workflow runtime behavior.

## Tests to be added/updated

- Update `app::controls::header` tests to expect `Cowboy · <topic>` for normal, background-task, long-topic truncation, and wide-character truncation cases.
- Update `app::commands` tests that assert header text after a `RunStarted` event with `request_topic`.
- Update `app::controls::status` tests to expect ` · ` between compact metadata parts for active, waiting, background-task, and narrow-width cases.
- Update `app::controls::composer` tests for active-run draft titles, and add or extend coverage for the idle and waiting-for-input title variants if they are not already asserted.
- Update full draw tests in `crates/tui/app/src/app/tests.rs` that currently match old compact status output.
- Add negative assertions where useful to prove separator replacement does not alter key-name hyphens such as `Shift/Ctrl-Enter`.

## How to verify

Run the narrow TUI test targets after implementation:

```bash
cargo test -p cowboy app::controls::header
cargo test -p cowboy app::controls::status
cargo test -p cowboy app::controls::composer
cargo test -p cowboy app::commands
cargo test -p cowboy app::tests
```

Manual smoke check:

```bash
cargo run
```

Then verify in the TUI:

- The header shows `Cowboy · <topic>` when a run topic is available and `Cowboy` when no topic is available.
- The bottom status metadata row separates fields with ` · `.
- The composer title separates shortcut phrases with ` · ` while keeping `Shift/Ctrl-Enter` unchanged.
- Narrow terminal truncation/drop behavior still keeps the highest-priority status metadata and does not corrupt borders or composer layout.

## TODO

- [x] Replace the header topic separator with ` · ` without changing idle header behavior.
- [x] Replace the compact metadata separator constant with ` · ` while preserving width and priority-drop behavior.
- [x] Replace composer title separator glyphs with ` · ` while preserving key-name hyphens.
- [x] Update focused header, status, composer, command, and full-draw test expectations.
- [x] Run the targeted TUI tests and record exact verification results.

## Verification results

- `cargo test -p cowboy app::controls::header` — passed: 6 tests.
- `cargo test -p cowboy app::controls::status` — passed: 5 tests.
- `cargo test -p cowboy app::controls::composer` — passed: 16 tests.
- `cargo test -p cowboy app::commands` — passed: 16 tests.
- `cargo test -p cowboy app::tests` — passed: 20 tests.
- `cargo clippy -p cowboy --all-targets` — passed.
- Manual TUI smoke via `cargo run --bin cowboy` in a pseudo-terminal — passed: idle header rendered `Cowboy`, composer rendered `Enter submits · Shift/Ctrl-Enter newline · type / for commands`, key-name hyphen remained intact, and the old composer ` ─ ` separator was absent.
