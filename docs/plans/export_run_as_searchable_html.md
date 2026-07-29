# Plan

Add a shared `export <run-id>` command for both product CLI and TUI slash-command surfaces. The command will load the canonical run plus its persisted workflow event log and write one standalone HTML transcript to the runtime working directory. The default filename will be derived from the loaded run id as `cowboy-export-<safe-run-id>.html`; characters outside ASCII letters, digits, `-`, and `_` will be replaced so a persisted id cannot escape the destination directory. Re-running the command for the same run will replace that deterministic export.

The exported transcript will contain the initial run request followed by every user-visible workflow card in event order. Persisted agent response/thought chunks and tool updates must be replayed through the same coalescing and agent-descriptor snapshot rules used by `AppState`, so the HTML represents the cards seen in the TUI rather than one fragmented card per raw streaming event. Command-submission, `/runs`, help, and other process-local application cards are not part of a persisted run and will not be synthesized; the initial request will be synthesized from `WorkflowRun::original_request` and `created_at`.

Each exported card will use an HTML `<details>` element with a `<summary>` card header. Cards are collapsed by default, individually expandable, and accompanied by `Expand all` and `Collapse all` controls. A case-insensitive in-page search box will search the full header and body text, hide nonmatching cards, expand matching cards, display a visible match count, and restore all cards to the collapsed default when cleared. The document must be self-contained, require no CDN or network access, preserve line breaks and readable card sections, and HTML-escape all run/event content before insertion.

Keep ownership boundaries intact:

- `cowboy-command-parser` owns `ExportArgs` and the shared CLI/slash grammar.
- `cowboy-workflow-engine` continues to own canonical run and persisted event loading; it may expose the configured runtime working directory through a read-only accessor but must not own HTML presentation.
- `crates/tui/app` owns card projection, HTML rendering, file creation, CLI output, and TUI completion/error cards.

# Changes

- In `crates/tui/command-parser/src/lib.rs`, add `SharedCommand::Export(ExportArgs)` with one required positional `run-id`. Generated slash help and suggestions must advertise `/export <run-id>`.
- In `crates/tui/app/src/app/state.rs`, extract the workflow-event coalescing and per-`(run_id, step_id)` agent-descriptor snapshot logic into a small reusable projector that can ingest events incrementally for the live TUI and replay a complete persisted event vector for export. Preserve the current boundaries: adjacent response/thought chunks coalesce only for the same run and step; tool updates replace the active card only for the same run, step, and tool call; any non-active card ends the coalescing window.
- In `crates/tui/app/src/app/events.rs` and `crates/tui/app/src/app/card.rs`, expose an internal card projection that both terminal rendering and HTML export can consume. The HTML path must use the untruncated card header, metadata, section labels, and rendered text content rather than terminal borders or an 80-column rendering, so exported headers and bodies are complete while retaining the same event titles and content transformations as the TUI.
- Add `crates/tui/app/src/export.rs` with an application-level export function that:
  - loads the run and persisted events through `WorkflowRuntime`;
  - synthesizes the initial request card;
  - replays events through the shared projector;
  - renders escaped, standalone HTML with embedded CSS and static JavaScript search/expand controls;
  - derives the safe deterministic filename under the runtime working directory and replaces it only after the complete document has been generated successfully;
  - returns the final path and exported card count for CLI/TUI reporting.
- In `crates/tui/app/src/main.rs`, dispatch `cowboy export <run-id>`, print the canonical run id, card count, and written path on success, and retain the existing nonzero error path for unknown runs, unreadable event logs, or write failures.
- In `crates/tui/app/src/app/commands.rs` and `crates/tui/app/src/app/state.rs`, dispatch `/export <run-id>` as a non-workflow background task that is allowed while a workflow is active. Render an `Export` completion card containing the run id, card count, and output path; route failures through the existing `Error` card behavior without changing active-run state or prompt windows.
- Update `crates/tui/app/src/lib.rs` to expose only the application-level export entry point needed by the binary. If the default destination needs runtime cwd access, add a read-only accessor in `crates/workflow/engine/src/runtime.rs` and re-export no new persistence internals.
- Update `README.md`, `AGENTS.md`, `docs/architecture.md`, and `docs/module-map.md` with `cowboy export <run-id>` and `/export <run-id>`, the default output location/name, collapsed-card behavior, and self-contained text search.
- Add repository-contained validation scripts `scripts/verify-export-fixture.sh` and `scripts/verify-export-browser.sh`. Both scripts must derive the repository root at runtime, keep generated data under `target/export-smoke-review`, contain no absolute home/session paths, and fail on any unmet assertion. The fixture script owns deterministic run/event/export creation and writes shell-escaped `RUN_ID`, `HTML_PATH`, and `SMOKE` values to `target/export-smoke-review/result.env`. The browser script consumes that file, serves only the generated directory on loopback, drives the exported page with `playwright-cli`, and removes its exact server process through a trap.

# Tests to be added/updated

- Add command-parser tests for `cowboy export run-123` and `/export run-123`, required-run-id validation, generated `/export <run-id>` usage, help rows, and slash suggestions.
- Add projector tests in `crates/tui/app/src/app/state.rs` or a new focused projector module using persisted-style event vectors. Cover adjacent response/thought chunk merging, tool-call update replacement, non-active boundaries, different step/run/tool ids, and descriptor snapshots so live TUI ingestion and replayed export produce identical projected entries.
- Add HTML export unit tests in `crates/tui/app/src/export.rs` using a temporary runtime:
  - the initial request and every projected workflow card appear in order;
  - all cards are emitted as closed `<details>` elements with complete `<summary>` headers;
  - card bodies preserve multiline content and section labels without terminal border glyphs or width truncation;
  - special characters and strings such as `<script>`, `&`, quotes, and `</script>` are escaped and never become executable markup;
  - embedded controls include search, match count, expand-all, and collapse-all behavior without external URLs;
  - raw streaming chunks/tool updates do not create duplicate fragmented cards;
  - the filename is sanitized, deterministic, and replaced successfully on a second export.
- Add a CLI integration test at `crates/tui/app/tests/export_cli.rs` that creates a temporary workflow/config, starts a run, executes `cowboy export <run-id>`, and asserts the reported file exists in the configured runtime cwd and contains the request, workflow output, collapsed cards, and search controls. Add a missing-run case that exits unsuccessfully and creates no HTML file.
- Add TUI command tests in `crates/tui/app/src/app/commands.rs` proving `/export <run-id>` is accepted during active execution, runs as a non-workflow background task, produces an `Export` completion card, and leaves active run/prompt state unchanged; cover the existing `Error` card path for export failures.
- Update existing card/state rendering tests affected by the shared projector/card projection refactor, preserving all current terminal card titles, coalescing behavior, descriptor labels, streaming behavior, and width-aware rendering.
- Add executable validation-script assertions:
  - `scripts/verify-export-fixture.sh` must create a real persisted run, replace its event log with fixed adjacent response chunks and a running-to-completed tool update, export from the generated runtime cwd, assert that the reported HTML file exists, and print `EXPORT_FIXTURE_OK`.
  - `scripts/verify-export-browser.sh` must assert five ordered cards, one coalesced two-line response card, one final completed tool card, all cards initially closed, individual/global expansion behavior, one visible open match for each fixed body token, and exactly one loopback document request with no subresource or external requests; it must print `EXPORT_BROWSER_OK`.

# How to verify

Run the focused parser, projector/export, and command tests:

```bash
cargo test -p cowboy-command-parser export
cargo test -p cowboy export
cargo test -p cowboy app::state::tests
cargo test -p cowboy app::commands::tests
cargo test -p cowboy --test export_cli
```

Then run the package regressions and warning gates:

```bash
cargo test -p cowboy
cargo test -p cowboy-workflow-engine
cargo fmt --check
cargo clippy -p cowboy -p cowboy-command-parser -p cowboy-workflow-engine --all-targets -- -D warnings
```

Run the checked-in deterministic fixture and browser validators from repository root:

```bash
test "$(git rev-parse --show-toplevel)" = "$(pwd)"
test -x scripts/verify-export-fixture.sh
test -x scripts/verify-export-browser.sh
! rg -n '[.]copilot|session-state|ROOT="/' scripts/verify-export-fixture.sh scripts/verify-export-browser.sh
bash scripts/verify-export-fixture.sh
bash scripts/verify-export-browser.sh
```

`scripts/verify-export-fixture.sh` must derive `ROOT` with `git rev-parse --show-toplevel`, use only `target/export-smoke-review` for generated files, safely recreate that exact directory, build `cowboy`, create an isolated status-only workflow/config, persist a run, install a fixed event log containing `BODY_ONLY_SEARCH_TOKEN` and `TOOL_UPDATE_SEARCH_TOKEN`, export it, and write `target/export-smoke-review/result.env`.

`scripts/verify-export-browser.sh` must require `python3`, `curl`, and `playwright-cli`; source only the repository-generated `result.env`; serve `target/export-smoke-review` on `127.0.0.1`; collect browser requests before navigation/reload; and assert the card order, coalescing, collapse/expand controls, search results, and network isolation described in `Tests to be added/updated`.

Expected result: the private-path guard returns no matches; the fixture script prints `EXPORT_FIXTURE_OK` with the generated run id and repository-local HTML path; the browser script prints `EXPORT_BROWSER_OK cards=5 response_matches=1 tool_matches=1 reload_requests=1 external_requests=0`; and both scripts exit `0` while leaving reproducible artifacts only under `target/export-smoke-review`.

# TODO

- [x] TODO-01: Add the shared `export <run-id>` command grammar and generated CLI/slash metadata.
  - Procedure: Add `ExportArgs` and `SharedCommand::Export` in `crates/tui/command-parser/src/lib.rs`; add CLI/slash parse, missing-argument, usage, help, and suggestion tests; run `cargo test -p cowboy-command-parser export`.
  - Expected result: The focused test command exits successfully; both command surfaces produce the same `ExportArgs { run_id }`; missing run ids are rejected; and generated usage is `/export <run-id>`.
  - Implementer observed result: `cargo test -p cowboy-command-parser export` passed 3 focused tests covering identical CLI/slash parsing, required run-id validation, `/export <run-id>` usage, generated help, and suggestions.

- [x] TODO-02: Extract a reusable workflow-card event projector without changing live TUI behavior.
  - Procedure: Move active-event coalescing and agent-descriptor snapshot rules from `AppState` into a reusable projector; route `AppState::apply_workflow_event` through it; add replay tests for response/thought chunks, tool updates, boundaries, ids, and descriptor changes; run `cargo test -p cowboy app::state::tests`.
  - Expected result: The state tests exit successfully; replayed persisted events yield the same ordered entries as incremental ingestion; and all existing coalescing, descriptor, active-run, prompt, and streaming assertions remain unchanged.
  - Implementer observed result: `cargo test -p cowboy app::state::tests` passed 37 tests with 1 intentional ignored helper; the new replay equivalence test matched incremental state projection across response chunks, step/run boundaries, tool replacement, and descriptor snapshots.

- [x] TODO-03: Make the existing card projection reusable by terminal and HTML renderers.
  - Procedure: Refactor `crates/tui/app/src/app/events.rs` and `card.rs` so one untruncated semantic card supplies the terminal renderer and exposes escaped-export inputs; update affected renderer tests; run `cargo test -p cowboy app::events::tests` followed by `cargo test -p cowboy app::card::tests`.
  - Expected result: Both focused test commands exit successfully; terminal output retains current titles, metadata, styles, sections, wrapping, and borders; and export-facing card data contains complete headers and body text without terminal border glyphs or width truncation.
  - Implementer observed result: The event suite passed 31 tests and the card suite passed 12 tests; terminal rendering remained unchanged, while the semantic-card test retained the complete header and multiline labeled section without truncation or border glyphs.

- [x] TODO-04: Implement secure standalone HTML generation and deterministic file writing.
  - Procedure: Add `crates/tui/app/src/export.rs`; load the run/events, synthesize the initial request card, replay projected events, escape all dynamic text, render collapsed `<details>` cards plus embedded search/expand controls, sanitize the filename, and replace the destination only after successful generation; run `cargo test -p cowboy export`.
  - Expected result: The focused tests exit successfully; fixture exports contain every projected card once and in order; hostile markup is inert; search/control assets are inline with no external URLs; and repeated export replaces the same safe file.
  - Implementer observed result: `cargo test -p cowboy export` passed 6 focused tests plus the CLI export integration test; projected response/tool events appeared once in order, hostile markup was escaped, controls and search were inline, filenames were sanitized, and a second complete write replaced the deterministic destination.

- [x] TODO-05: Wire export execution and reporting through CLI and TUI surfaces.
  - Procedure: Add CLI dispatch in `main.rs`, application exports in `lib.rs`, and a non-workflow TUI background-result path in `commands.rs`/`state.rs`; add `crates/tui/app/tests/export_cli.rs` and focused TUI tests; run `cargo test -p cowboy --test export_cli` and `cargo test -p cowboy app::commands::tests`.
  - Expected result: Both commands exit or complete successfully for a valid run and report the canonical id, card count, and existing HTML path; unknown runs fail through the standard error paths without creating a file; and `/export` does not alter active workflow or prompt state.
  - Implementer observed result: The CLI integration test passed and the command suite passed 42 tests; valid exports reported run id, card count, and an existing cwd path, missing runs failed without another HTML file, and `/export` preserved active execution and pending-prompt state while using the standard Error card on failure.

- [x] TODO-06: Document the export command and complete regression verification.
  - Procedure:
    1. Update `README.md`, `AGENTS.md`, `docs/architecture.md`, and `docs/module-map.md`.
    2. Run `for file in README.md AGENTS.md docs/architecture.md docs/module-map.md; do rg -q 'cowboy export <run-id>' "$file" && rg -q '/export <run-id>' "$file" && rg -q 'cowboy-export-<safe-run-id>[.]html' "$file" && rg -qi 'collapsed' "$file" && rg -qi 'case-insensitive' "$file"; done`.
    3. Run `cargo test -p cowboy`.
    4. Run `cargo test -p cowboy-workflow-engine`.
    5. Run `cargo fmt --check`.
    6. Run `cargo clippy -p cowboy -p cowboy-command-parser -p cowboy-workflow-engine --all-targets -- -D warnings`.
    7. Add executable, repository-contained `scripts/verify-export-fixture.sh` and `scripts/verify-export-browser.sh` with the contracts specified in `Changes` and `How to verify`.
    8. Run `! rg -n '[.]copilot|session-state|ROOT="/' scripts/verify-export-fixture.sh scripts/verify-export-browser.sh`.
    9. Run `bash scripts/verify-export-fixture.sh`.
    10. Run `bash scripts/verify-export-browser.sh`.
  - Expected result: Documentation consistently describes both command surfaces, default filename/location, collapsed cards, and search; all Rust commands exit successfully with no warnings; the private-path guard returns no matches; the fixture validator prints `EXPORT_FIXTURE_OK` and stores all generated state under `target/export-smoke-review`; and the browser validator prints `EXPORT_BROWSER_OK cards=5 response_matches=1 tool_matches=1 reload_requests=1 external_requests=0` after proving ordered coalesced cards, expansion/search behavior, and zero external requests.
  - Implementer observed result: All four documents passed the per-file command-surface, filename/location, collapsed-card, and case-insensitive-search assertions. The Cowboy and workflow-engine test suites, formatting check, and Clippy warning gate exited successfully. Both checked-in scripts are executable, derive the repository root, contain no private home/session paths, and leave generated files only under `target/export-smoke-review`. The fixture validator printed `EXPORT_FIXTURE_OK`; the browser validator printed `EXPORT_BROWSER_OK cards=5 response_matches=1 tool_matches=1 reload_requests=1 external_requests=0`.
