# Plan

Implement the approved findings in [`rca.md`](./rca.md) at the command-runner boundary. `CommandActionRunner` will continue to clear the ambient environment, then copy only a documented allow-list of host variables required for executable lookup and Windows runtime, profile, authentication-cache, and temporary-directory initialization. This keeps `action.command` direct and shell-free while removing the need for workflow-authored launchers, encoded scripts, or reconstructed environment values.

The allow-list will contain `PATH`, `SystemRoot`, `USERPROFILE`, `LOCALAPPDATA`, `APPDATA`, `TEMP`, and `TMP`. Each value will be copied unchanged from Cowboy's process only when present; Cowboy will not synthesize paths or inherit arbitrary variables. The policy applies by variable name on every host so the existing platform-independent `SystemRoot` regression probe exercises the same production path. Environment names and values remain runtime-only and must not be added to command output, events, or persisted records.

No field will be added to `CommandAction` or the Lua `action.command` table. Workflow authors should not select, inject, or persist environment values as part of this fix.

# Changes

- In `crates/workflow/actions/src/command.rs`, replace the one-off `PATH` pass-through with one small, explicit command-environment allow-list and a helper that copies only present values into the already-cleared `tokio::process::Command` before spawn.
- Preserve the current process contract around exact `program`/`args`, `RuntimeConfig.cwd`, closed stdin, bounded stdout/stderr capture, timeout/kill behavior, exit-status routing, and spawn-error records.
- Keep environment data out of `CommandOutcome`, `StepOutput`, `StepRecord`, and logging; the runner should only forward approved values to the child process.
- In `docs/workflow-authoring.md`, replace the PATH-only statement with the exact allow-list, explain that all other ambient variables remain removed, and retain the warning that `action.command` is not a sandbox and does not accept workflow-authored environment overrides.

# Tests to be added/updated

- Keep the investigator-added regression test `command::tests::command_runner_preserves_system_root_for_child_runtime_initialization` unchanged. Product code must make this direct child-runtime probe pass.
- Extend the pre-existing sanitized-environment coverage in `crates/workflow/actions/src/command.rs` with an isolated subprocess probe that supplies non-sensitive marker values for the approved variables plus an unapproved marker, runs the real `CommandActionRunner`, and asserts only presence/absence: every approved variable reaches the command child and the unapproved variable does not.
- Continue asserting that representative unrelated ambient variables such as `HOME` and `CARGO` are absent, proving the fix does not regress to full environment inheritance.
- Do not assert or print environment values in test failures or captured command output; helper modes should report only `set` or `missing`.
- Run the existing command-runner suite to guard working directory, direct argument passing, timeout, capture bounds, routing, and spawn failures while changing process setup.

# How to verify

- `cargo test -p cowboy-workflow-actions command::tests::command_runner_preserves_system_root_for_child_runtime_initialization -- --exact`
- Run the new exact allow-list subprocess test by its final test name.
- `cargo test -p cowboy-workflow-actions command`
- `cargo test -p cowboy-workflow-actions`
- `cargo fmt --all -- --check`
- `cargo clippy -p cowboy-workflow-actions --all-targets -- -D warnings`
- On a Windows host when available, run a harmless workflow command that launches PowerShell directly with an explicit argument vector and verify it initializes without a shell wrapper; also verify an unapproved marker variable is absent from the child.

# TODO

- [x] Define the explicit command child-environment allow-list in `crates/workflow/actions/src/command.rs`.
- [x] Apply the allow-list after `env_clear()` and before spawning the direct child process.
- [x] Keep the investigator-added `SystemRoot` regression test unchanged and make it pass through the production runner.
- [x] Add isolated behavioral coverage for all approved variables and rejection of an unapproved variable without exposing values.
- [x] Reconcile the existing sanitized-environment test with the expanded allow-list while retaining negative coverage for unrelated ambient variables.
- [x] Update `docs/workflow-authoring.md` with the corrected environment contract and security boundary.
- [x] Run the focused regression, command-runner suite, formatter check, and warning-free Clippy verification.
- [x] Direct PowerShell Windows smoke test not exercised: the available test host is Linux under WSL2, not a Windows Cowboy runtime.
