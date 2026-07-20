# Plan

Extend the existing command-action child-environment allow-list so Cowboy forwards `PATHEXT` from the Cowboy process to workflow `action.command` children when the variable is present. Keep the current security model: `CommandActionRunner` still calls `env_clear()`, copies only approved variables, does not synthesize missing values, does not expose environment values in records/events/logs, and does not add workflow-authored environment overrides.

Repository inspection shows the relevant boundary is `crates/workflow/actions/src/command.rs`: `COMMAND_ENV_ALLOW_LIST` is applied immediately after `env_clear()` and before `tokio::process::Command::spawn()`. The existing test-only `EXPECTED_COMMAND_ENV_ALLOW_LIST` drives an isolated subprocess probe that verifies approved variables reach the child and unapproved markers do not. `docs/workflow-authoring.md` documents the exact allow-list for workflow authors.

This feature is a narrow allow-list expansion only. It should not change `CommandAction` in `crates/workflow/core/src/action.rs`, Lua conversion in `crates/workflow/lua/src/convert.rs`, command routing, command output shape, working-directory behavior, stdin handling, timeout behavior, or shell-free argument-vector execution.

# Changes

- In `crates/workflow/actions/src/command.rs`, add `PATHEXT` to `COMMAND_ENV_ALLOW_LIST` so command children inherit it unchanged when Cowboy's own process has it.
- In `crates/workflow/actions/src/command.rs`, update the test mirror `EXPECTED_COMMAND_ENV_ALLOW_LIST` and the existing allow-list probe expectations so `PATHEXT` is covered by the same positive forwarding test as `PATH`, `SystemRoot`, `USERPROFILE`, `LOCALAPPDATA`, `APPDATA`, `TEMP`, and `TMP`.
- Keep negative environment coverage for `HOME`, `CARGO`, and `COWBOY_COMMAND_UNAPPROVED` to prove the change does not regress to full environment inheritance.
- In `docs/workflow-authoring.md`, update the command action environment paragraph to include `PATHEXT` in the documented allow-list and retain the warning that all other ambient variables remain removed.

# Tests to be added/updated

- Update `command::tests::command_runner_forwards_only_allowlisted_environment_variables` through `EXPECTED_COMMAND_ENV_ALLOW_LIST` so its isolated parent process supplies a non-sensitive marker `PATHEXT` value and the command child reports `PATHEXT=set` without printing the value.
- Keep `command::tests::command_runner_uses_sanitized_environment` asserting representative unrelated variables are absent from command children.
- Keep `command::tests::command_runner_preserves_system_root_for_child_runtime_initialization` unchanged; the PATHEXT feature must not disturb the existing Windows-runtime initialization coverage.
- Do not add tests that assert real host `PATHEXT` contents, because environment values can reveal local configuration. Tests should assert only `set` or `missing` marker states.

# How to verify

- `cargo test -p cowboy-workflow-actions command::tests::command_runner_forwards_only_allowlisted_environment_variables -- --exact`
- `cargo test -p cowboy-workflow-actions command::tests::command_runner_uses_sanitized_environment -- --exact`
- `cargo test -p cowboy-workflow-actions command::tests::command_runner_preserves_system_root_for_child_runtime_initialization -- --exact`
- `cargo test -p cowboy-workflow-actions command`
- `cargo fmt --all -- --check`
- `cargo clippy -p cowboy-workflow-actions --all-targets -- -D warnings`

# TODO

- [x] TODO-01: Add `PATHEXT` to the production command environment allow-list.
  - Procedure: Edit `crates/workflow/actions/src/command.rs` so `COMMAND_ENV_ALLOW_LIST` contains `PATHEXT` exactly once alongside the existing approved names, then inspect the constant in the diff.
  - Expected result: The production allow-list includes `PATHEXT`; no other production command-runner behavior or data model changes appear in the diff.
  - Implementer observed result: `COMMAND_ENV_ALLOW_LIST` now has eight entries and includes `PATHEXT` exactly once between `PATH` and `SystemRoot`; `apply_command_environment` and command-runner data model code were left unchanged.

- [x] TODO-02: Update command-runner allow-list test coverage for `PATHEXT`.
  - Procedure: Edit `EXPECTED_COMMAND_ENV_ALLOW_LIST` in `crates/workflow/actions/src/command.rs` to mirror the production allow-list, then run `cargo test -p cowboy-workflow-actions command::tests::command_runner_forwards_only_allowlisted_environment_variables -- --exact`.
  - Expected result: The exact test passes; the isolated helper observes `PATHEXT=set` for the approved marker and still observes `HOME=missing`, `CARGO=missing`, and `COWBOY_COMMAND_UNAPPROVED=missing`.
  - Implementer observed result: `EXPECTED_COMMAND_ENV_ALLOW_LIST` mirrors the production eight-entry allow-list with `PATHEXT` exactly once, and `cargo test -p cowboy-workflow-actions command::tests::command_runner_forwards_only_allowlisted_environment_variables -- --exact` passed with 1 test passed.

- [x] TODO-03: Preserve existing sanitized-environment and SystemRoot behavior.
  - Procedure: Run `cargo test -p cowboy-workflow-actions command::tests::command_runner_uses_sanitized_environment -- --exact` and `cargo test -p cowboy-workflow-actions command::tests::command_runner_preserves_system_root_for_child_runtime_initialization -- --exact`.
  - Expected result: Both exact tests pass; unrelated ambient variables remain absent and `SystemRoot` still reaches the command child.
  - Implementer observed result: Both exact tests passed with 1 test passed each; the sanitized-environment test still covers absent unrelated variables, and the SystemRoot probe still passed.

- [x] TODO-04: Update workflow authoring documentation with the new allow-list.
  - Procedure: Edit `docs/workflow-authoring.md` command-action environment paragraph to include `PATHEXT`, then review that paragraph for the `env_clear()` policy, missing-value behavior, and no workflow-authored environment override warning.
  - Expected result: The documentation names `PATHEXT` in the approved list and still states that all other ambient variables remain removed.
  - Implementer observed result: The command-action environment paragraph now lists `PATHEXT` after `PATH` and still states that Cowboy clears the child environment, copies only approved variables when present, does not synthesize missing values, removes every other ambient variable, and provides no workflow-authored environment override.

- [x] TODO-05: Run focused crate verification after implementation.
  - Procedure: Run `cargo test -p cowboy-workflow-actions command`, `cargo fmt --all -- --check`, and `cargo clippy -p cowboy-workflow-actions --all-targets -- -D warnings` from the repository root.
  - Expected result: All commands exit successfully with no Rustfmt diff and no Clippy warnings.
  - Implementer observed result: `cargo test -p cowboy-workflow-actions command` exited 0 with 10 tests passed, 3 ignored, and 6 filtered; `cargo fmt --all -- --check` exited 0 with no diff; `cargo clippy -p cowboy-workflow-actions --all-targets -- -D warnings` exited 0 with no warnings.
