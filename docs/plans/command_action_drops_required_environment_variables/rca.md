## Bug behavior

`action.command` starts child programs with only `PATH` from Cowboy's process environment. On Windows, a directly launched `powershell.exe` therefore receives no `SystemRoot`. PowerShell can abort during .NET runtime initialization before evaluating the requested script, producing HRESULT `8009001d` (observed externally as exit code `-65536`). Workflow arguments, quoting, and script contents are not reached.

The current workaround is to launch a shell first, reconstruct environment variables there, and encode the PowerShell script to survive another parsing layer. That workaround moves host process setup into workflow code and defeats the direct-program, explicit-argument-vector interface of `action.command`.

## Root cause

`CommandActionRunner::run` in `crates/workflow/actions/src/command.rs` constructs the child with `Command::env_clear()` and then restores only `PATH`. This policy removes platform variables required while some child runtimes initialize. The focused reproduction injects `SystemRoot` into an isolated parent test process, invokes the real command runner without a shell or quoted script, and observes `SystemRoot=missing` in the command child.

The loss cannot be repaired through the declarative action. `CommandAction` in `crates/workflow/core/src/action.rs` has no environment field, and the Lua conversion in `crates/workflow/lua/src/convert.rs` accepts no environment values for `action.command`. `docs/workflow-authoring.md` explicitly documents the current PATH-only policy. Therefore every normal workflow call through this action loses `SystemRoot`; the only available workaround is an extra launcher that reconstructs it before starting the affected runtime.

The isolated probe rules out argument quoting and working-directory handling: it passes only the existing test-runner arguments, uses a valid temporary working directory, and still loses the variable at the `env_clear` boundary.

## Reproduction steps

1. Run the focused test command from the repository root:

   ```text
   cargo test -p cowboy-workflow-actions command::tests::command_runner_preserves_system_root_for_child_runtime_initialization -- --exact
   ```

2. The test starts an isolated copy of the test process with a non-sensitive marker value for `SystemRoot`; this avoids mutating the test harness's global environment.
3. That process executes a `CommandAction` through `CommandActionRunner`.
4. The command child reports whether `SystemRoot` exists, without printing its value.
5. Observe that the parent has supplied the variable but the command child prints `SystemRoot=missing`.

The command was run twice and failed identically, completing in about 0.03 seconds each time.

## Regression test

- Test file: `crates/workflow/actions/src/command.rs`
- Test name: `command::tests::command_runner_preserves_system_root_for_child_runtime_initialization`
- Command: `cargo test -p cowboy-workflow-actions command::tests::command_runner_preserves_system_root_for_child_runtime_initialization -- --exact`
- Expected failure before the fix: exit code `101`; the nested probe fails because the child output contains `SystemRoot=missing` instead of `SystemRoot=set`.

The nested ignored test `command::tests::command_runner_system_root_probe` and the `system_root` helper mode are test harness details. The named outer test is the regression contract and runs normally.

## Current failing result

```text
running 1 test
test command::tests::command_runner_system_root_probe ... FAILED

thread 'command::tests::command_runner_system_root_probe' panicked:
fields: { ..., "stdout": "...SystemRoot=missing...", ... }

test command::tests::command_runner_preserves_system_root_for_child_runtime_initialization ... FAILED

test result: FAILED. 0 passed; 1 failed
error: test failed, to rerun pass `-p cowboy-workflow-actions --lib`
```

The focused command exits with status `101`. Temporary paths and unrelated harness details are omitted from this document.

## Fix constraints

- Keep the investigator-added regression test unchanged; product code must make it pass.
- Preserve the direct executable plus exact argument-vector behavior. A shell wrapper, encoded script, or workflow-authored environment reconstruction is not an acceptable product fix.
- Ensure platform environment required to start supported child runtimes is present before spawn. For the reported failure, `SystemRoot` must reach a Windows command child.
- Do not blindly inherit or persist the entire ambient environment. It can contain credentials and other secrets; use an explicit, maintainable policy for required variables.
- Do not expose environment values in `StepOutput`, event logs, or persisted run records.
- Preserve existing command behavior for configured working directory, closed stdin, timeout and kill handling, bounded stdout/stderr capture, status routing, and spawn-error reporting.
- Reconcile the existing PATH-only sanitation test and authoring documentation with the corrected environment contract as part of the eventual fix.
- Product code was not changed during this investigation.
