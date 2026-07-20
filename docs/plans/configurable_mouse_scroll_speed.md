# Plan

Make transcript mouse-wheel scrolling configurable in the TUI and lower the built-in default from the current hardcoded 10 visual rows per wheel event to 3 visual rows. Keep keyboard transcript scrolling (`Ctrl+U` / `Ctrl+D`) at the existing 10-row step so this feature only changes the mouse-wheel behavior the user reported.

Repository inspection found the relevant implementation in `crates/tui/app/src/app/input.rs`, `crates/tui/app/src/app/state.rs`, and `crates/tui/app/src/app/controls/transcript.rs`. The hardcoded step comes from `AppState::next_scroll_offset()` and `AppState::scroll_events_down()`, which currently add/subtract 10 rows and are used by both keyboard and mouse scroll paths. The TUI config is parsed in `crates/tui/app/src/config.rs` with `#[serde(default, deny_unknown_fields)]`, so the new setting should be a first-class `AppConfig` field with validation and tests rather than an ad hoc environment variable.

# Changes

- Add a top-level `mouse_scroll_lines` field to `AppConfig` in `crates/tui/app/src/config.rs`.
  - Type: an unsigned integer that converts safely to `usize` for UI state.
  - Default: `3`.
  - Validation: reject `0` with an actionable config-load error.
  - Keep `deny_unknown_fields` so misspelled config keys still fail.
- Store the effective mouse-scroll step in `AppState` when constructed from `AppConfig`.
  - Expose it only inside the app module; do not leak TUI behavior into workflow runtime crates.
- Parameterize transcript scroll helpers so callers can request a specific visual-row delta.
  - Keep existing keyboard behavior at 10 rows.
  - Use `mouse_scroll_lines` for `MouseEventKind::ScrollUp` and `MouseEventKind::ScrollDown` inside the transcript area.
  - Preserve existing clamping, follow-latest behavior at offset 0, selection clearing when scrolling changes offset, and no-op behavior outside the transcript.
- Update config examples and user-facing docs that describe `config.toml`.
  - At minimum: `README.md` and `demo-config.toml`.
  - If implementation touches the authoritative config contract docs, keep `docs/architecture.md`, `docs/module-map.md`, and `docs/workflow-authoring.md` consistent.

# Tests to be added/updated

- Extend `crates/tui/app/src/config.rs` tests to cover:
  - missing config uses `mouse_scroll_lines = 3`;
  - an explicit `mouse_scroll_lines` value parses and reaches `AppConfig`;
  - `mouse_scroll_lines = 0` is rejected;
  - unknown top-level fields remain rejected.
- Update `crates/tui/app/src/app/input.rs` tests to assert:
  - default mouse wheel scrolls by 3 rows when enough transcript content exists;
  - a custom `mouse_scroll_lines` value changes mouse-wheel distance;
  - `Ctrl+U` / `Ctrl+D` remain 10-row keyboard scrolls;
  - resize reconciliation still moves immediately for both mouse and keyboard, with mouse assertions using the configured mouse delta.
- Keep existing tests for empty/short transcripts, non-transcript mouse regions, unsupported mouse events, and selection behavior passing without relaxing their assertions.

# How to verify

1. Run the focused config tests:
   ```bash
   cargo test -p cowboy config::tests
   ```
   Expected result: all config tests pass, including default, override, runtime-conversion, unknown-field, and zero-value rejection coverage for `mouse_scroll_lines`.
2. Run the focused TUI input tests:
   ```bash
   cargo test -p cowboy app::input::tests
   ```
   Expected result: mouse wheel tests prove the default 3-row behavior and custom configured behavior; keyboard transcript scrolling remains 10 rows; empty, short, and non-transcript mouse events remain no-ops.
3. Run the broader affected TUI app tests:
   ```bash
   cargo test -p cowboy app::tests
   cargo test -p cowboy app::controls::transcript::tests
   ```
   Expected result: scroll-limit, resize, transcript rendering, and selection tests still pass.
4. Run the crate lint gate required for TUI changes:
   ```bash
   cargo clippy -p cowboy --all-targets -- -D warnings
   ```
   Expected result: no Rust compiler or Clippy warnings.
5. Run this reproducible manual smoke check in a terminal with mouse-wheel support:
   1. Create the temporary workflow and two configs:
      ```bash
      SMOKE_ROOT="$(mktemp -d)"
      mkdir -p "$SMOKE_ROOT/workflows" "$SMOKE_ROOT/state-1" "$SMOKE_ROOT/state-default"
      cat > "$SMOKE_ROOT/workflows/overflow.lua" <<'LUA'
      local show = step("show")

      show.run = function(ctx)
        local lines = {}
        for index = 1, 80 do
          lines[#lines + 1] = string.format("SMOKE-LINE-%02d", index)
        end

        return action.status {
          status = "success",
          body = table.concat(lines, "\n"),
        }
      end

      return workflow("overflow", show, { description = "scroll smoke workflow" })
      LUA
      cat > "$SMOKE_ROOT/config-1.toml" <<TOML
      state_dir = "$SMOKE_ROOT/state-1"
      workflow_store = "$SMOKE_ROOT/state-1/workflow.redb"
      workflow_dirs = ["$SMOKE_ROOT/workflows"]
      mouse_scroll_lines = 1
      TOML
      cat > "$SMOKE_ROOT/config-default.toml" <<TOML
      state_dir = "$SMOKE_ROOT/state-default"
      workflow_store = "$SMOKE_ROOT/state-default/workflow.redb"
      workflow_dirs = ["$SMOKE_ROOT/workflows"]
      TOML
      ```
   2. Launch the configured-one-line case:
      ```bash
      cargo run -p cowboy -- --config "$SMOKE_ROOT/config-1.toml"
      ```
   3. In the TUI, type `/run --workflow overflow smoke-overflow`, press `Enter`, wait for `Run completed`, and press `End` so the transcript follows the tail.
   4. Record the smallest visible `SMOKE-LINE-NN` number in the overflow card body, scroll the mouse wheel up exactly one detent over the transcript, and record the smallest visible `SMOKE-LINE-NN` again.
      Expected result: the absolute change is `1` when `mouse_scroll_lines = 1`.
   5. Press `End`, record the smallest visible `SMOKE-LINE-NN`, press `Ctrl+U` once, and record it again.
      Expected result: the absolute change is `10`, proving keyboard scroll still uses the existing larger step.
   6. Exit with `Ctrl+C`, launch the default case with `cargo run -p cowboy -- --config "$SMOKE_ROOT/config-default.toml"`, rerun `/run --workflow overflow smoke-overflow`, wait for completion, press `End`, and repeat the one-detent mouse-wheel observation.
      Expected result: the absolute change is `3`, proving the omitted config uses the smaller built-in default.

# TODO

- [x] TODO-01: Add a validated top-level `mouse_scroll_lines` field to TUI config with default 3.
  - Procedure: Run `cargo test -p cowboy config::tests` after adding the field, default, parsing coverage, runtime construction coverage if needed, and zero-value validation.
  - Expected result: tests show missing config yields `mouse_scroll_lines = 3`, explicit positive values parse, and `mouse_scroll_lines = 0` fails config loading with an actionable error.
  - Observed result: `cargo test -p cowboy config::tests` passed with 17 tests; coverage now asserts missing config yields `mouse_scroll_lines = 3`, explicit `mouse_scroll_lines = 5` parses, `mouse_scroll_lines = 0` is rejected with `mouse_scroll_lines must be greater than zero`, unknown top-level fields remain rejected, and demo config keeps the default value.

- [x] TODO-02: Store the effective mouse scroll step in `AppState` without changing workflow runtime config.
  - Procedure: Add focused tests named `app_state_uses_configured_mouse_scroll_lines` in `crates/tui/app/src/app/state.rs` and `runtime_config_does_not_include_mouse_scroll_lines` in `crates/tui/app/src/config.rs`, then run:
    ```bash
    cargo test -p cowboy app::state::tests::app_state_uses_configured_mouse_scroll_lines
    cargo test -p cowboy config::tests::runtime_config_does_not_include_mouse_scroll_lines
    ```
  - Expected result: the state test constructs `AppState` with `AppConfig { mouse_scroll_lines: 7, .. }` and observes that the app state returns `7` through its internal mouse-scroll accessor; the runtime-config test constructs two otherwise identical configs with different `mouse_scroll_lines` values and observes equal `RuntimeConfig` fields, proving the setting stays TUI-only.
  - Observed result: both focused commands passed with one matching test each; `app_state_uses_configured_mouse_scroll_lines` observed `state.mouse_scroll_lines() == 7`, and `runtime_config_does_not_include_mouse_scroll_lines` observed equal serialized `RuntimeConfig` values for configs that differed only by `mouse_scroll_lines`.

- [x] TODO-03: Parameterize transcript scrolling so mouse wheel uses the configured step and keyboard scroll keeps the existing 10-row step.
  - Procedure: Add or update focused input tests with these exact names, then run:
    ```bash
    cargo test -p cowboy app::input::tests::transcript_wheel_uses_default_mouse_scroll_lines
    cargo test -p cowboy app::input::tests::transcript_wheel_uses_configured_mouse_scroll_lines
    cargo test -p cowboy app::input::tests::keyboard_transcript_scroll_keeps_ten_line_step
    ```
  - Expected result: the default mouse-wheel test observes `state.scroll_offset() == 3` after one `MouseEventKind::ScrollUp` on scrollable transcript content; the configured mouse-wheel test observes the configured value, such as `2`; the keyboard test observes `state.scroll_offset() == 10` after one `Ctrl+U` on the same kind of content.
  - Observed result: all three focused input commands passed; the default mouse-wheel test observed `state.scroll_offset() == 3`, the configured mouse-wheel test observed `state.scroll_offset() == 2`, and the keyboard `Ctrl+U` test observed `state.scroll_offset() == 10` with `mouse_scroll_lines = 2`.

- [x] TODO-04: Update resize, no-op, and boundary tests for configurable mouse scrolling.
  - Procedure: Run `cargo test -p cowboy app::input::tests` after updating assertions that previously assumed a shared 10-row mouse and keyboard scroll distance.
  - Expected result: all input tests pass, including empty/short transcript no-ops, non-transcript region no-ops, resize reconciliation, follow-latest restoration, and selection-clearing behavior.
  - Observed result: `cargo test -p cowboy app::input::tests` passed with 39 tests; resize reconciliation now expects the configured mouse delta for mouse paths and 10 rows for keyboard paths, while empty/short transcript no-ops, non-transcript region no-ops, follow-latest restoration, and selection-clearing behavior stayed covered.

- [x] TODO-05: Document `mouse_scroll_lines` in config examples and user-facing config docs.
  - Procedure: Perform this ordered documentation check after editing:
    1. Open `README.md` and verify the example `config.toml` block contains a top-level line `mouse_scroll_lines = 3` before `[config_sets.default]` and prose states that the field controls transcript mouse-wheel visual rows per detent and must be greater than zero.
    2. Open `demo-config.toml` and verify it contains `mouse_scroll_lines = 3` as a top-level key near `state_dir`, `workflow_store`, and `workflow_dirs`.
    3. If any of `docs/architecture.md`, `docs/module-map.md`, or `docs/workflow-authoring.md` mention TUI config fields after implementation, verify they use the same field name `mouse_scroll_lines`, default `3`, and zero-is-invalid rule; if they do not mention TUI mouse scrolling, leave them unchanged.
    4. Run this exact command:
       ```bash
       python3 -c "from pathlib import Path; [(_ for _ in ()).throw(AssertionError(path)) for path in ['README.md', 'demo-config.toml'] if 'mouse_scroll_lines = 3' not in Path(path).read_text()]; print('mouse_scroll_lines docs present')"
       ```
  - Expected result: the manual checks find the same field name, default, and validation rule in every touched config document; the Python 3 command prints `mouse_scroll_lines docs present`.
  - Observed result: README.md contains `mouse_scroll_lines = 3` before `[config_sets.default]` and prose says it controls transcript mouse-wheel visual rows per detent, defaults to `3`, and must be greater than zero; demo-config.toml contains the same top-level key near the path settings; docs/architecture.md, docs/module-map.md, and docs/workflow-authoring.md do not document TUI mouse scrolling, so they were left unchanged; the exact `python3` docs command printed `mouse_scroll_lines docs present`.

- [x] TODO-06: Run final focused verification and record exact evidence for the reviewer.
  - Procedure: Run these exact commands and manual smoke steps, preserving stdout/stderr summaries and the observed line-number deltas as evidence:
    ```bash
    cargo test -p cowboy config::tests
    cargo test -p cowboy app::input::tests
    cargo test -p cowboy app::tests
    cargo test -p cowboy app::controls::transcript::tests
    cargo clippy -p cowboy --all-targets -- -D warnings
    ```
    Then perform the manual smoke procedure in `How to verify` step 5 using the exact `overflow.lua`, `config-1.toml`, and `config-default.toml` contents shown there.
  - Expected result: all commands pass; the smoke evidence records a one-detent mouse-wheel absolute line-number change of `1` with `mouse_scroll_lines = 1`, a one-detent mouse-wheel absolute line-number change of `3` with the field omitted, and a one-press keyboard `Ctrl+U` absolute line-number change of `10`.
  - Observed result: final verification passed: `cargo test -p cowboy config::tests` reported 17 passed, `cargo test -p cowboy app::input::tests` reported 39 passed, `cargo test -p cowboy app::tests` reported 35 passed, `cargo test -p cowboy app::controls::transcript::tests` reported 21 passed, and `cargo clippy -p cowboy --all-targets -- -D warnings` completed with no warnings. The smoke workflow/config files were created under `/tmp/cowboy-scroll-smoke-m4gsa9b3`; terminal-smoke observations showed the transcript tail initially at smallest visible `SMOKE-LINE-50`, one SGR mouse-wheel-up detent with `mouse_scroll_lines = 1` repainted the top to `SMOKE-LINE-49` for delta `1`, one `Ctrl+U` from the tail repainted from `SMOKE-LINE-50` to `SMOKE-LINE-40` for delta `10`, and one SGR mouse-wheel-up detent with the field omitted repainted from `SMOKE-LINE-50` to `SMOKE-LINE-47` for delta `3`.
