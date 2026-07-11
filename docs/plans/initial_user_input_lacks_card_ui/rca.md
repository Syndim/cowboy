# Bug behavior

Submitting an initial plain-text request in the TUI records the immediate transcript entry as bare text instead of the same card UI used for other transcript notices and workflow events.

Observed current rendering from the focused regression test:

```text
submitted run: build health route
```

Expected behavior is that the submitted initial request is rendered as a card, including a card title and framed body, e.g. a `Run` card containing `submitted run: build health route`.

# Root cause

The plain request path is:

```text
commands::submit_input
  -> dispatch_submitted_input
  -> spawn_start_run
  -> AppState::spawn_report_task
```

`spawn_start_run` builds the label `submitted run: {request}` and passes it to `AppState::spawn_report_task`. `AppState::spawn_report_task` currently does:

```rust
self.push_event(TranscriptEntry::Plain(label));
```

`TranscriptEntry::Plain` renders through `render_plain_lines`, which only styles the text and does not use `Card::render`. Other TUI transcript notices use `AppState::push_card`, and workflow events render through card builders. Because the initial submitted request is inserted before the workflow emits `RunStarted`, it bypasses the card renderer and appears as unframed text.

# Reproduction steps

1. In the TUI, submit a plain request such as `build health route` from an idle composer.
2. Observe the first transcript entry shown immediately after submission.
3. The entry appears as bare text: `submitted run: build health route`.
4. It lacks the card title, top/bottom borders, and body frame used by card-rendered transcript entries.

Repository-grounded reproduction:

```bash
cargo test -p cowboy app::commands::tests::plain_request_submission_renders_initial_input_as_card -- --nocapture
```

# Regression test

- Test file path: `crates/tui/app/src/app/commands.rs`
- Test name: `app::commands::tests::plain_request_submission_renders_initial_input_as_card`
- Command: `cargo test -p cowboy app::commands::tests::plain_request_submission_renders_initial_input_as_card -- --nocapture`
- Expected failure before the fix: the rendered transcript contains only the bare `submitted run: build health route` line, so the assertion for `◌ Run` card title fails.

# Current failing result

```text
running 1 test
thread 'app::commands::tests::plain_request_submission_renders_initial_input_as_card' panicked at crates/tui/app/src/app/commands.rs:469:9:
submitted run: build health route
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test app::commands::tests::plain_request_submission_renders_initial_input_as_card ... FAILED

failures:

failures:
    app::commands::tests::plain_request_submission_renders_initial_input_as_card

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 165 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

# Fix constraints

- Do not change workflow runtime behavior; the bug is in TUI transcript presentation for submitted work labels.
- Preserve the existing plain request dispatch behavior: a non-slash input with no pending prompt must still start a run.
- Preserve submitted input history and background-task locking semantics.
- Apply card rendering to the immediate submitted-run transcript entry without regressing slash command cards, workflow event cards, pending prompt cards, or active event coalescing.
- Keep sensitive input out of durable diagnostics; tests and RCA use generic sample request text only.
