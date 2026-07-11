# Plan

Refine the existing TUI card for the initial plain-text workflow submission. Keep the change in `crates/tui/app`: the submission already renders as a `Run` card, but its title lacks the fixed initial timestamp and submitted-run context while its body repeats the `submitted run:` status prefix. The rendered title contract will be `00:00:00 · ◌ Run · submitted run`; the body will contain only the submitted request text.

The implementation and post-change verification are complete. The original baseline placed the full `submitted run: {request}` label in the card body and passed the old focused test; the updated card now separates fixed-time/title context from the request body, and the evidence below records the updated test, package, formatting, lint, and manual smoke results.

This is presentation-only. The fixed `00:00:00` is an explicit initial-card display value, not a runtime timestamp and not elapsed-time accounting. Preserve the full `submitted run: {request}` status string, input history, background-task locking, runtime dispatch, and all non-targeted command paths.

# Changes

- Update `crates/tui/app/src/app/state.rs` so `TranscriptEntry::Card` carries ordered title-prefix and title-suffix parts while retaining its existing title/details behavior for other cards. Route those parts through the card renderer's width-safe title ordering rather than embedding display metadata in the body.
- Adapt the private `AppState` card-report-task helper to accept separate title prefix, title suffix, status, and body details. Preserve status assignment, `running` state, transcript insertion, and Tokio task spawning semantics.
- Update `crates/tui/app/src/app/commands.rs::spawn_start_run` to pass `00:00:00` as the title prefix, `submitted run` as the title suffix, and only `{request}` as body details. Keep the existing `WorkflowRuntime::start_run(request)` future unchanged. This covers plain input and the default `/run` path that share `spawn_start_run`; stepwise and workflow-specific variants remain on their existing report path.
- Keep the status text as `submitted run: {request}` so status-oriented tests and the status strip retain their current contract. Do not change workflow events, elapsed-time calculation for event cards, command parsing, runtime behavior, persistence, or input history.

# Tests to be added/updated

- Update `crates/tui/app/src/app/commands.rs::tests::plain_request_submission_renders_initial_input_as_card` to assert the exact compact title ordering: `00:00:00 · ◌ Run · submitted run`. Assert the framed body contains only the generic request text and does not contain `submitted run:`; keep the assertion that the entry is card-rendered rather than a bare leading status line.
- The command-path regression fully exercises the new card-prefix/suffix helper, so no supplemental implementation-detail test is required.
- Leave `run_and_plain_text_keep_selector_backed_start_labels` and `run_command_paths_display_runtime_supplied_topic_only` unchanged except for compilation-driven adjustments; they must continue to prove that runtime status labels, task counts, and dispatch variants are unaffected.

# How to verify
1. The pre-change baseline focused test passed with `1 passed`, but only against the old `submitted run:` body assertion; it was not treated as feature evidence.
2. Post-change focused regression:
   `cargo test -p cowboy app::commands::tests::plain_request_submission_renders_initial_input_as_card -- --nocapture`
   Result: `1 passed`.
3. Command-module regression:
   `cargo test -p cowboy app::commands::tests`
   Result: `18 passed`.
4. Full TUI package regression:
   `cargo test -p cowboy`
   Result: `164 passed`, `2 ignored`.
5. Formatting and lint checks:
   `cargo fmt --check`
   `cargo clippy -p cowboy --all-targets -- -D warnings`
   Both completed successfully.
6. Manual smoke evidence: built the updated `target/debug/cowboy`, launched it in a 120×40 pseudo-terminal, submitted the generic request `smoke request`, and pressed Enter. The rendered transcript showed `00:00:00 · ◌ Run · submitted run`, a framed `smoke request` body, and no framed `submitted run:` prefix. Ctrl-C then exited the TUI.

7. Follow-up style correction: inserted the required blank line between `Card::title_prefix` and `Card::title_suffix`; reran `cargo fmt --check`, which completed successfully.

# TODO

- [x] Extend the TUI card transcript representation with ordered title-prefix and title-suffix support while preserving existing card callers.
- [x] Adapt the `AppState` card-report-task helper to separate title metadata from body details without changing status, run-state, or task-spawn semantics.
- [x] Update `spawn_start_run` to render the initial request as `00:00:00 · ◌ Run · submitted run` with the raw request in the card body.
- [x] Preserve the full `submitted run: {request}` status and all non-targeted command, runtime, history, and background-task behavior.
- [x] Update the initial-submission regression test for fixed-time title ordering and prefix-free body content.
- [x] Confirm the command-path regression fully covers the new helper; no supplemental prefix-helper test is needed.
- [x] Run the focused command tests, full `cowboy` tests, formatting check, Clippy check, and manual TUI smoke verification.
