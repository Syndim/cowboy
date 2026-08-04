# Plan

Make ambient environment forwarding an explicit runtime configuration policy instead of a constant owned by the command-action implementation.

Add these TOML surfaces to `AppConfig`:

```toml
allowed_env = [
  "PATH",
  "PATHEXT",
  "SystemRoot",
  "USERPROFILE",
  "LOCALAPPDATA",
  "APPDATA",
  "TEMP",
  "TMP",
]

[[agents]]
name = "planner"
command = "copilot"
args = ["--acp"]
allowed_env = ["PLANNER_SERVICE_TOKEN"]

[agents.model]
id = "planner-model"
provider = "configured-provider"

[[agents]]
name = "implementer"
command = "copilot"
args = ["--acp"]
allowed_env = ["IMPLEMENTER_SERVICE_TOKEN"]

[agents.model]
id = "implementer-model"
provider = "configured-provider"
```

Top-level `allowed_env` is the global list. It applies to every process Cowboy starts for workflow execution: `action.command` children and ACP backend processes used by workflow selection, request-topic generation, role actions, workflow improvement, retries, and recovery. When `allowed_env` is omitted, Cowboy materializes the current hard-coded eight-name list (`PATH`, `PATHEXT`, `SystemRoot`, `USERPROFILE`, `LOCALAPPDATA`, `APPDATA`, `TEMP`, and `TMP`), so existing installations continue to work without adding the new key. An explicitly configured `allowed_env = []` remains distinct and forwards no ambient variables.

Each `[[agents]]` entry has its own optional `allowed_env` beside `command`, `args`, `model`, and `watchdog`. An omitted agent list defaults to empty, so the selected backend still receives the top-level default eight-name list even when the user configures no `allowed_env` key anywhere. A Lua role selects one of these configured entries through its existing `agent = "name"` field. Whenever Cowboy starts that backend, it forwards the union of the top-level global list and the selected agent entry's list. Agent entries are additive and cannot remove globally allowed names. Multiple Lua roles that select the same configured agent intentionally share its environment policy; roles that require different policies must select different named agent entries.

Non-role ACP operations use the same resolver as today: workflow selection, request-topic generation, and workflow improvement resolve the implicit/default configured agent and receive the global list plus that agent entry's list. The per-agent list is therefore colocated with the model and applies consistently to every launch of that configured backend, not only `action.agent`.

At every affected spawn boundary, Cowboy must call `env_clear()` and then copy only configured names that are present in Cowboy's current process environment. Values must be read at process-start time, forwarded unchanged, and never serialized into `RuntimeConfig`, workflow state, step output, events, or logs. Configuration and diagnostics may contain variable names, but never their values.

# Changes

- Extend `crates/tui/app/src/config.rs` with top-level `AppConfig.allowed_env` and per-agent `AgentConfig.allowed_env`, colocated with each role backend's model/watchdog configuration.
  - Preserve the current eight global names as the omitted-config default.
  - Default each omitted agent list to empty.
  - Validate that every variable name is non-empty and contains neither `=` nor NUL, and reject duplicate names independently within the global list and each agent list.
  - Keep `deny_unknown_fields` behavior and make explicit `allowed_env = []` distinct from omission.
- Extend `RuntimeConfig` in `crates/workflow/engine/src/runtime.rs` with the global list and extend `AgentRuntimeConfig` with the configured agent's list. Update `RuntimeConfig::new`, product construction, `engine-cli`, and test/runtime literals without persisting resolved environment values.
- Replace `COMMAND_ENV_ALLOW_LIST` in `crates/workflow/actions/src/command.rs` with allow-list data supplied to `CommandActionRunner`.
  - Update `CommandActionRunner` and `EngineActionDispatcher` constructors to retain the configured global names.
  - Keep `env_clear()`, direct executable/argument spawning, cwd, stdin, timeout, capture, cancellation, and output behavior unchanged.
- Extend `cowboy-agent-acp` stdio transport configuration so a caller can request a cleared child environment and provide allowed ambient variable names separately from explicit fixed `env` entries.
  - `StdioTransport::connect` must clear first, copy only present allow-listed ambient values, then apply any explicit transport entries.
  - Keep standalone ACP callers backward compatible unless they opt into clearing; Cowboy's workflow engine must always opt in.
  - Ensure reconnect, resume, watchdog recovery, and forced-restart paths reuse the same transport configuration and therefore the same allow-list.
- Update `crates/workflow/engine/src/runtime_dependencies.rs` and direct ACP construction in `runtime.rs`.
  - After `AgentResolver` selects an `AgentRuntimeConfig`, merge the global list with that agent entry's list, deduplicate names deterministically, and pass the result to the selected backend.
  - Use the same merge for selector, topic-generator, improvement/summarizer, and role-action clients so every launch of one configured agent receives the same policy.
  - Do not add environment metadata to `RoleDefinition` or `RoleDefinition.properties`; Lua roles continue to select a named `[[agents]]` entry through the existing `agent` field.
- Extend the existing deterministic `watchdog-fixture` binary in `crates/agent/acp/src/bin/watchdog-fixture.rs` with an environment-policy verifier rather than relying on a real external agent.
  - Add `probe-environment`, `serve-environment`, and `verify-environment` subcommands. The probe and ACP peer must record only `set`/`missing` states for four fixed synthetic names, never their values.
  - Generate `target/allowed-env-smoke/config.toml` with `default`, `planner`, and `implementer` `[[agents]]` entries, each using the fixture executable; put planner and implementer `allowed_env` additions directly on their entries before `[agents.model]`. Generate `target/allowed-env-smoke/workflows/allowed_env.lua` with Lua roles selecting the matching named entries.
  - The workflow must execute a command probe, a planner agent turn that deliberately triggers one recoverable retry, an `ask_user` boundary followed by a planner session load in a second Cowboy process, a planner watchdog hard-recovery/forced-restart turn, and an implementer agent turn.
  - Associate each fixture PID with the selected configured agent name from the generated command arguments and received prompt/session events, assert the initial and replacement process matrices, and prove the hard-recovery replacement receives `--resume=<session-id>`.
  - After the run, invoke `cowboy export <run-id>`, then search the generated SQLite database and any WAL/SHM files, persisted workflow event JSON, Cowboy logs, fixture JSONL, command-probe output, CLI stdout/stderr, and exported HTML for the four synthetic marker values. Variable names and `set`/`missing` states may appear; marker values must not.
  - Give both new verifier workspaces the existing authenticated marker `.cowboy-watchdog-smoke` with the exact `WORKSPACE_MARKER` contents and a root `identities/` directory. Every `serve-environment` process, including watchdog replacements, must receive `--identity-dir <workspace>/identities` and write `<workspace>/identities/<pid>.json`.
  - Extend `find_identity_files` to inspect the exact new root path `<workspace>/identities/*.json` in addition to the existing legacy `soft/identities`, `hard/identities`, and `end-turn-cancel/identities` paths. Do not recursively scan arbitrary directories, follow symlinks, or accept identities outside the marked workspace.
  - Preserve the existing authenticated cleanup and evidence-retention behavior: cleanup must validate the marker, challenge every live recorded process using endpoint, invocation token, start nonce, PID, and canonical executable, wait for each matching PID to exit, and remove only the named workspace after every identity succeeds. Any mismatch must preserve the workspace and all processes/evidence not proven safe to terminate.
- Update `demo-config.toml`, the README configuration example, and `docs/workflow-authoring.md` to document the top-level and per-`[[agents]]` keys, additive selected-agent semantics, shared policy when multiple roles select one agent, omitted versus explicit-empty behavior, process coverage, and the warning that required authentication/tool variables must be explicitly allow-listed.

# Tests to be added/updated

- Add `crates/tui/app/src/config.rs` tests named `configurable_allowed_env_parses_and_reaches_runtime`, `allowed_env_omission_preserves_platform_defaults`, `explicit_empty_allowed_env_is_preserved`, `rejects_invalid_allowed_env`, and `runtime_config_serialization_contains_environment_names_not_values`, covering:
  - parsing and runtime conversion of top-level and multiple `[[agents]].allowed_env` values beside `[agents.model]`;
  - the omitted-config eight-name global default, omitted empty agent defaults, and explicit empty global/agent lists;
  - rejection of blank/invalid variable names and duplicate names in global or agent lists;
  - preservation of variable names only, with an isolated subprocess setting synthetic ambient values before config loading and proving those values are absent from serialized `AppConfig` and `RuntimeConfig`.
- Update `crates/workflow/actions/src/command.rs` tests so the isolated subprocess probe supplies a configured allow-list to `CommandActionRunner`, observes approved names as set, and still observes unrelated names as missing. Remove the mirrored production constant from the test.
- Add `cowboy-agent-acp` stdio transport tests named `sanitized_environment_forwards_only_allowlisted_variables`, `explicit_environment_overrides_allowlisted_ambient_value`, and `sanitized_environment_is_preserved_when_resume_argument_restarts_stdio_transport`, using an isolated child process to prove clear-and-allow behavior, absence of an unapproved marker, missing-variable omission, explicit-entry ordering, and identical policy when a replacement transport is started with `--resume=<session-id>`.
- Add ACP client tests named `lazy_reconnect_reuses_original_environment_policy` and `hard_recovery_reuses_original_environment_policy`. Record the `TransportConfig` supplied for initial creation, `Client::close` followed by lazy reconnect, and watchdog replacement; assert all three carry the same clear flag and allow-list, while the replacement alone adds the session-resume argument.
- Expand `crates/workflow/engine/src/runtime_dependencies.rs` with `named_agent_client_uses_global_and_agent_allowed_env`, `different_agents_do_not_share_environment_additions`, and `default_agent_clients_use_global_and_default_agent_allowed_env` tests that record transport environment policy and prove:
  - a role selecting `planner` receives the deduplicated global-plus-planner list;
  - a role selecting `implementer` receives global plus implementer entries and no planner entries;
  - selector/topic/summarizer paths receive global plus the resolved default agent's entries;
  - two Lua roles selecting the same named agent receive the same per-agent policy.
- Update engine/action dispatcher construction tests and runtime fixtures to compile with and assert the new environment-policy fields.
- Add `watchdog-fixture` contract tests named `watchdog_fixture_generates_allowed_env_contract`, `watchdog_fixture_environment_matrix_rejects_cross_role_leakage`, and `watchdog_fixture_environment_artifact_scan_rejects_marker_values`.
- Add a `watchdog-fixture` contract test named `watchdog_fixture_omitted_allowed_env_starts_command_and_default_agent`. Its generated config must omit `allowed_env` at both top level and inside the sole `[[agents]]` entry, while the verifier process ensures all eight default names are present without recording their values and supplies one synthetic unapproved marker.
- Add cleanup contract tests named `watchdog_fixture_cleanup_authenticates_allowed_env_layout` and `watchdog_fixture_cleanup_authenticates_default_allowed_env_layout`. Each test must create the correct marker and root `identities/` path, launch a real fixture process that writes an identity record there, invoke `cleanup`, and assert the authenticated process exits and only the marked workspace is removed.
- Keep and run `watchdog_fixture_cleanup_refuses_unmarked_directory` to prove neither new layout weakens the marker gate.
- Add `watchdog_fixture_cleanup_preserves_workspace_on_identity_mismatch`: launch a real fixture process, save its valid identity, alter one authentication field in the on-disk record, invoke cleanup, and assert cleanup fails, the workspace remains, and the original PID is still alive. Restore the valid record and invoke cleanup again so the test authenticates and terminates its own fixture without leaking a process.
- Add `watchdog_fixture_identity_discovery_rejects_symlink_and_non_regular_entries`: create a marked workspace whose root `identities/` contains a symlinked `.json` entry targeting a file outside the workspace and a non-regular `.json` entry. Assert discovery/cleanup fails before reading the symlink target or removing either path. Use real symlink metadata where the platform permits deterministic symlink creation; otherwise exercise the same entry-classification branch through a test-only metadata seam while retaining a real non-regular filesystem entry.
- The `verify-environment` end-to-end scenario must prove each lifecycle boundary directly:
  - recoverable workflow retry sends the correction prompt to the same planner PID and preserves its matrix;
  - persisted CLI resume starts a new planner process, loads the recorded session, and reapplies the planner matrix;
  - explicit ACP lazy reconnect is covered by the named client test;
  - watchdog hard recovery force-terminates the stalled planner PID, starts exactly one replacement PID with `--resume=<session-id>`, and preserves the planner matrix;
  - the implementer backend process receives global plus its configured agent entries and never planner-agent entries.
- Keep environment tests value-safe: use synthetic variable names and marker values, assert only `set`/`missing` or exact synthetic markers, and never print or snapshot the host environment.

# How to verify

1. Run each config test exactly:
   - `cargo test -p cowboy --lib config::tests::configurable_allowed_env_parses_and_reaches_runtime -- --exact`
   - `cargo test -p cowboy --lib config::tests::allowed_env_omission_preserves_platform_defaults -- --exact`
   - `cargo test -p cowboy --lib config::tests::explicit_empty_allowed_env_is_preserved -- --exact`
   - `cargo test -p cowboy --lib config::tests::rejects_invalid_allowed_env -- --exact`
   - `cargo test -p cowboy --lib config::tests::runtime_config_serialization_contains_environment_names_not_values -- --exact`
2. Run `cargo test -p cowboy-workflow-actions --lib command::tests::command_runner_forwards_only_configured_environment_variables -- --exact`.
3. Run each ACP transport/client test exactly:
   - `cargo test -p cowboy-agent-acp --lib transport::stdio::tests::sanitized_environment_forwards_only_allowlisted_variables -- --exact`
   - `cargo test -p cowboy-agent-acp --lib transport::stdio::tests::explicit_environment_overrides_allowlisted_ambient_value -- --exact`
   - `cargo test -p cowboy-agent-acp --lib transport::stdio::tests::sanitized_environment_is_preserved_when_resume_argument_restarts_stdio_transport -- --exact`
   - `cargo test -p cowboy-agent-acp --lib client::tests::lazy_reconnect_reuses_original_environment_policy -- --exact`
   - `cargo test -p cowboy-agent-acp --lib client::tests::hard_recovery_reuses_original_environment_policy -- --exact`
4. Run each engine policy test exactly:
   - `cargo test -p cowboy-workflow-engine --lib runtime_dependencies::tests::named_agent_client_uses_global_and_agent_allowed_env -- --exact`
   - `cargo test -p cowboy-workflow-engine --lib runtime_dependencies::tests::different_agents_do_not_share_environment_additions -- --exact`
   - `cargo test -p cowboy-workflow-engine --lib runtime_dependencies::tests::default_agent_clients_use_global_and_default_agent_allowed_env -- --exact`
   - `cargo test -p cowboy-workflow-engine --lib runtime::tests::selector_and_improvement_clients_use_resolved_agent_allowed_env -- --exact`
5. Run each fixture contract test exactly:
   - `cargo test -p cowboy-agent-acp --bin watchdog-fixture tests::watchdog_fixture_generates_allowed_env_contract -- --exact`
   - `cargo test -p cowboy-agent-acp --bin watchdog-fixture tests::watchdog_fixture_environment_matrix_rejects_cross_role_leakage -- --exact`
   - `cargo test -p cowboy-agent-acp --bin watchdog-fixture tests::watchdog_fixture_environment_artifact_scan_rejects_marker_values -- --exact`
   - `cargo test -p cowboy-agent-acp --bin watchdog-fixture tests::watchdog_fixture_cleanup_authenticates_allowed_env_layout -- --exact`
   - `cargo test -p cowboy-agent-acp --bin watchdog-fixture tests::watchdog_fixture_cleanup_authenticates_default_allowed_env_layout -- --exact`
   - `cargo test -p cowboy-agent-acp --bin watchdog-fixture tests::watchdog_fixture_cleanup_refuses_unmarked_directory -- --exact`
   - `cargo test -p cowboy-agent-acp --bin watchdog-fixture tests::watchdog_fixture_cleanup_preserves_workspace_on_identity_mismatch -- --exact`
   - `cargo test -p cowboy-agent-acp --bin watchdog-fixture tests::watchdog_fixture_identity_discovery_rejects_symlink_and_non_regular_entries -- --exact`
6. Run the exact deterministic end-to-end verifier from the repository root:

   ```bash
   cargo build -p cowboy --bin cowboy
   cargo run -p cowboy-agent-acp --bin watchdog-fixture -- verify-environment \
     --cowboy target/debug/cowboy \
     --workspace target/allowed-env-smoke \
     --response-timeout-seconds 1 \
     --cancel-timeout-seconds 2 \
     --recovery-operation-timeout-seconds 3 \
     --soft-deadline-seconds 20 \
     --hard-deadline-seconds 30
   ```

   The verifier must create only these evidence roots under `target/allowed-env-smoke`: `.cowboy-watchdog-smoke`, `identities/`, `config.toml`, `workflows/allowed_env.lua`, `state/`, `fixture-events.jsonl`, `command-matrix.json`, `cowboy-run.stdout`, `cowboy-run.stderr`, `cowboy-answer.stdout`, `cowboy-answer.stderr`, and `export.html`. The marker contents must equal the existing `WORKSPACE_MARKER`; every fixture process must write `identities/<pid>.json`. The verifier supplies fixed synthetic values internally for `COWBOY_TEST_GLOBAL`, `COWBOY_TEST_PLANNER`, `COWBOY_TEST_IMPLEMENTER`, and `COWBOY_TEST_UNAPPROVED`, starts Cowboy with those four variables, and prints these exact success lines after reading the fixture artifacts:

   ```text
   command global=set planner=missing implementer=missing unapproved=missing
   planner.retry global=set planner=set implementer=missing unapproved=missing same_pid=true
   planner.resume global=set planner=set implementer=missing unapproved=missing session_loaded=true
   planner.replacement global=set planner=set implementer=missing unapproved=missing resumed_session=true old_pid_exited=true
   implementer global=set planner=missing implementer=set unapproved=missing
   artifacts sqlite=clean events=clean logs=clean fixture_jsonl=clean stdout_stderr=clean export=clean
   ```

   Before printing success, the verifier must:
   1. Generate a config with top-level `allowed_env = ["COWBOY_TEST_GLOBAL"]` plus `default`, `planner`, and `implementer` `[[agents]]` entries. Each entry uses the current fixture executable; planner and implementer declare their additions through `allowed_env` in the same agent table as their model.
   2. Run `target/debug/cowboy --config <workspace>/config.toml run --workflow allowed_env "allowed env smoke"` through the command, planner retry, and `ask_user` boundary.
   3. Parse the run id from stdout, then run `target/debug/cowboy --config <workspace>/config.toml answer <run-id> continue continue` in a new Cowboy process; the generated workflow must use the fixed prompt id `continue`.
   4. Run `target/debug/cowboy --config <workspace>/config.toml export <run-id>` from the workspace and rename the generated HTML to `export.html`.
   5. Assert the process/session/PID matrix above, including one hard-recovery replacement with `--resume=<session-id>`.
   6. Search SQLite `data.db` plus existing `data.db-wal`/`data.db-shm`, `state/events/*.json`, `state/logs/*.log`, fixture JSONL, command matrix, both CLI stdout/stderr pairs, and `export.html` for every synthetic marker value. Any occurrence is a failure. `AppConfig` and `RuntimeConfig` serialization are covered separately by the exact test in verification step 1.
   7. Remove `target/allowed-env-smoke` only on success. On failure, preserve it and print `allowed env evidence preserved at target/allowed-env-smoke`.

   If a failed run leaves the marked workspace, run:

   ```bash
   cargo run -p cowboy-agent-acp --bin watchdog-fixture -- cleanup \
     --workspace target/allowed-env-smoke
   ```

   Cleanup must first validate `target/allowed-env-smoke/.cowboy-watchdog-smoke`, then discover only `target/allowed-env-smoke/identities/*.json` plus the three legacy scenario identity paths supported by the same command. It must authenticate every live identity with the existing PID/endpoint protocol and remove only that marked workspace after all matching processes exit.
7. Run the exact omitted-key compatibility proof:

   ```bash
   cargo test -p cowboy-agent-acp --bin watchdog-fixture \
     tests::watchdog_fixture_omitted_allowed_env_starts_command_and_default_agent \
     -- --exact

   cargo build -p cowboy --bin cowboy
   cargo run -p cowboy-agent-acp --bin watchdog-fixture -- verify-default-allowed-env \
     --cowboy target/debug/cowboy \
     --workspace target/allowed-env-default-smoke \
     --deadline-seconds 20
   ```

   The contract test must report `running 1 test` and pass. The verifier must:
   1. Refuse to overwrite an existing workspace, then create `target/allowed-env-default-smoke/.cowboy-watchdog-smoke`, `identities/`, `config.toml`, `workflows/default_allowed_env.lua`, `state/`, `fixture-events.jsonl`, `command-matrix.json`, `cowboy.stdout`, and `cowboy.stderr`. The marker contents must equal `WORKSPACE_MARKER`, and every fixture process must write `identities/<pid>.json`.
   2. Write a config that contains no `allowed_env` text anywhere, has one `[[agents]]` entry named `default` using the current fixture executable, and has the workflow/store paths under the workspace.
   3. Launch Cowboy with all eight default names present plus synthetic `COWBOY_TEST_UNAPPROVED`. Preserve the verifier's current value for each default name when present; for a missing name, inject a workspace-local non-sensitive placeholder. On Windows, require and preserve the real `SystemRoot` rather than substituting a placeholder, so the proof cannot invalidate child runtime initialization. Do not write any of these values to fixture artifacts.
   4. Run a workflow whose first step starts the fixture's command probe and whose second step starts the default ACP fixture; each child records only `set`/`missing`, and the workflow completes successfully.
   5. Assert both children report this exact line:

      ```text
      PATH=set PATHEXT=set SystemRoot=set USERPROFILE=set LOCALAPPDATA=set APPDATA=set TEMP=set TMP=set COWBOY_TEST_UNAPPROVED=missing
      ```

   6. Print exactly:

      ```text
      omitted_allowed_env command=started default_agent=started defaults=8 unapproved=missing workflow=success
      ```

   7. Remove `target/allowed-env-default-smoke` only after all assertions pass. On failure, preserve the marked workspace and print `default allowed env evidence preserved at target/allowed-env-default-smoke`.

   If cleanup is needed, run:

   ```bash
   cargo run -p cowboy-agent-acp --bin watchdog-fixture -- cleanup \
     --workspace target/allowed-env-default-smoke
   ```

   Cleanup must validate `target/allowed-env-default-smoke/.cowboy-watchdog-smoke`, discover only `target/allowed-env-default-smoke/identities/*.json` plus the supported legacy paths, authenticate every live identity, and remove only the marked workspace after all matching processes exit.

8. Run `cargo test -p cowboy-workflow-actions -p cowboy-workflow-engine -p cowboy-agent-acp -p cowboy`.
9. Run `cargo fmt --all -- --check`.
10. Run `cargo clippy -p cowboy-workflow-actions -p cowboy-workflow-engine -p cowboy-agent-acp -p cowboy --all-targets -- -D warnings`.

# TODO

- [x] TODO-01: Add validated global and role-specific environment allow-list configuration to `AppConfig`.
  - Procedure: Add top-level `AppConfig.allowed_env` and `AgentConfig.allowed_env` beside each agent's model/watchdog fields, implement omitted-config defaults and validation, then run the first four exact `cargo test -p cowboy --lib config::tests::... -- --exact` commands in verification step 1 and both exact omitted-key commands in verification step 7.
  - Expected result: Valid TOML produces the exact global and per-agent `allowed_env` lists with the per-agent key accepted in the same `[[agents]]` table as `[agents.model]`; explicit `allowed_env = []` remains empty; every invalid or duplicate entry fails with a field-specific configuration error. With no `allowed_env` key anywhere, the contract test executes one matching test and the deterministic verifier completes a real command step and a real default ACP fixture step, with both children observing all eight current default names and rejecting the unapproved marker.
  - Observed result: All four exact configuration tests and the exact omitted-key fixture test were rerun as separately mapped fail-fast commands, each reported `running 1 test`, and passed. The separately mapped default verifier printed `omitted_allowed_env command=started default_agent=started defaults=8 unapproved=missing workflow=success`.

- [x] TODO-02: Propagate environment allow-list names through `RuntimeConfig` without resolving or persisting values.
  - Procedure: Extend `RuntimeConfig`, `RuntimeConfig::new`, `AppConfig::runtime_config`, `engine-cli`, and all runtime/test constructors. Add the isolated `config::tests::runtime_config_serialization_contains_environment_names_not_values` test and the `verify-environment` artifact scan described in verification step 6. Run all five exact config commands in verification step 1, then run the end-to-end verifier and inspect its `artifacts ...` success line.
  - Expected result: Runtime configuration contains only the global names and per-`AgentRuntimeConfig` names; the isolated serialization test executes one matching test and finds no synthetic values in `AppConfig` or `RuntimeConfig`; the verifier reports `sqlite=clean`, `events=clean`, `logs=clean`, `fixture_jsonl=clean`, `stdout_stderr=clean`, and `export=clean`, proving Cowboy's forwarding plumbing did not copy values into configuration, workflow state, persisted records, events, diagnostics, or exported output.
  - Observed result: All five exact configuration commands were rerun and separately mapped; each reported `running 1 test` and passed. The separately mapped lifecycle verifier printed `artifacts sqlite=clean events=clean logs=clean fixture_jsonl=clean stdout_stderr=clean export=clean`.

- [x] TODO-03: Make command actions consume the configured global allow-list instead of a hard-coded constant.
  - Procedure: Pass the global list through `EngineActionDispatcher` into `CommandActionRunner`, remove `COMMAND_ENV_ALLOW_LIST`, then run `cargo test -p cowboy-workflow-actions --lib command::tests::command_runner_forwards_only_configured_environment_variables -- --exact` followed by the package suite in verification step 8.
  - Expected result: A command child receives every configured-and-present global variable, receives no unapproved marker, and preserves all existing command execution, timeout, capture, status, and cancellation behavior.
  - Observed result: The separately mapped command-policy test reported `running 1 test` and passed. The separately mapped four-package suite also passed with the Zellij lifecycle shim interpreted by stable `sh`, preserving existing command execution, timeout, capture, status, and cancellation behavior.

- [x] TODO-04: Add opt-in sanitized ambient environment forwarding to ACP stdio transport.
  - Procedure: Extend `StdioConfig` with clear-environment and ambient allow-list settings, apply them in `StdioTransport::connect`, update every config literal, and add recording hooks for client transport recreation. Run all five exact commands in verification step 3.
  - Expected result: Opted-in ACP children start from an empty environment, receive only configured ambient names plus explicit transport entries, explicit entries win on duplicate names, and existing callers that do not opt in retain their prior inheritance behavior.
  - Observed result: All five exact ACP transport/client commands were rerun and separately mapped; each reported `running 1 test` and passed, covering sanitized forwarding, explicit override ordering, resume recreation, lazy reconnect, and hard recovery policy reuse.

- [x] TODO-05: Apply global and role-specific policies to every workflow-engine ACP launch path.
  - Procedure: Add deterministic global-plus-resolved-agent merging in `runtime_dependencies.rs` and direct ACP construction in `runtime.rs`, then extend `watchdog-fixture` with the exact `verify-environment` scenario from verification step 6. Run all exact engine commands in verification step 4, all exact ACP recreation commands in verification step 3, and the verifier.
  - Expected result: Each ACP launch receives global plus only the resolved `[[agents]]` entry's additions; selector, topic, and improvement clients use the resolved default agent's policy; multiple roles sharing an agent share its policy. The verifier proves one retry stays on the same correctly configured planner-agent PID, persisted CLI resume starts a correctly configured planner-agent process and loads the session, hard recovery exits the old PID and starts exactly one correctly configured `--resume=<session-id>` replacement, and the implementer agent never receives planner-only or unapproved names. The exact lazy-reconnect client test proves `Client::close` followed by reconnect reuses the original environment policy.
  - Observed result: All four exact engine commands and both required ACP recreation commands were rerun and separately mapped; each reported `running 1 test` and passed. The separately mapped verifier printed all five required process matrices with `same_pid=true`, `session_loaded=true`, `resumed_session=true`, and `old_pid_exited=true`.

- [x] TODO-06: Document the configurable environment contract and migration behavior.
  - Procedure: Update `demo-config.toml`, the README config example, and `docs/workflow-authoring.md` with synthetic variable names, top-level plus per-`[[agents]]` examples beside model configuration, additive selected-agent semantics, the shared-policy rule for multiple roles selecting one agent, omitted versus explicit-empty behavior, all covered spawn paths, and the security warning about explicitly allowing required authentication/tool variables.
  - Expected result: The three documents describe the same TOML keys and semantics, contain no environment values or private data, and no longer describe the command allow-list as a fixed code-owned policy.
  - Observed result: All three documents now use `allowed_env`, synthetic names only, and consistently describe global defaults, per-agent additive selection, shared-agent policy, explicit-empty behavior, covered spawn paths, and authentication/tool exposure risks.

- [x] TODO-07: Run focused and cross-crate verification for environment isolation.
  - Procedure: Execute verification steps 1 through 10 in order. For every exact command in steps 1 through 5 and the exact test in step 7, preserve Cargo output showing `running 1 test` and the named test passed; treat `running 0 tests` as failure. For step 6, preserve the verifier's six exact success lines or its retained evidence directory on failure. For step 7, preserve the exact `omitted_allowed_env ...` success line or its retained evidence directory on failure. Record exit status for every command.
  - Expected result: Every exact test invocation reports one matching passed test, both deterministic verifiers print their exact success evidence and remove their workspaces, and the package suites, Rustfmt, and Clippy exit successfully. The configured-policy verifier proves isolation and lifecycle behavior; the omitted-key verifier proves operational compatibility for both command and default ACP child startup. No zero-match Cargo filter is accepted as evidence.
  - Observed result: Every independently mapped exact filter in verification steps 1 through 5 and 7 reported `running 1 test` and passed. Both independently mapped deterministic verifiers printed their required success lines and removed their workspaces. The independently mapped four-package suite, Rustfmt check, and Clippy command each exited successfully.

- [x] TODO-08: Extend authenticated fixture cleanup for both allowed-env verifier workspace layouts.
  - Procedure: Make both verifiers create `.cowboy-watchdog-smoke` with `WORKSPACE_MARKER` and root `identities/<pid>.json` records; extend `find_identity_files` to inspect only regular `.json` files directly under root `identities/` plus the three existing legacy scenario identity directories, using symlink metadata and never following symlinks; then run the final five exact cleanup contract tests in verification step 5. Preserve output showing `running 1 test` for each command.
  - Expected result: Both layout tests launch a real fixture process, authenticate it through the recorded endpoint/token/nonce/PID/executable tuple, observe the PID exit, and remove only the marked workspace. The unmarked-directory test passes unchanged. The mismatch test returns an error while preserving the workspace and live unverified PID, then succeeds only after restoring the authentic record. The discovery test rejects symlinked and non-regular `.json` entries without opening their targets or deleting the workspace. Every exact command executes one matching passing test.
  - Observed result: All five exact cleanup contract commands were rerun and separately mapped; each reported `running 1 test` and passed, including authenticated cleanup for both layouts, marker refusal, mismatch preservation/recovery, and symlink/non-regular identity rejection.
