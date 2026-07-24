# Plan

Use the confirmed RCA in `docs/plans/loaded_agent_sessions_lose_model_descriptor/rca.md` as the source of truth. A fresh ACP `Client` starts with no session descriptor, and `Client::load_session` currently discards the successful `session/load` result, so agent-returned `configOptions` never reach the existing descriptor extraction path.

Fix the ACP boundary rather than adding a configured-model fallback or changing TUI behavior. Parse the successful `session/load` payload, derive `AgentSessionDescriptor` through the existing `Client::descriptor_from_config_options` helper, and replace `self.session_descriptor` before the loaded session is exposed to `AgentExecutor`. A successful load that returns `null`, an empty object, missing `configOptions`, an empty option list, or only unrelated options must leave the descriptor as `None` and clear any stale descriptor on the client. A malformed non-null payload must remain an explicit deserialization error.

Keep the investigator-added repro test unchanged as the primary regression input: `crates/agent/acp/src/client.rs::load_session_captures_descriptor_from_returned_config_options`. Do not replace it with a lower-level helper test or weaken its model and reasoning assertions.

# Changes

- Add a typed `SessionLoadResult` response model in `crates/agent/acp/src/messages.rs`, following the existing `SessionNewResult` and `SetSessionConfigOptionResult` conventions. Its camel-case `configOptions` field should default to an empty vector when the object omits the field.
- Update `Client::load_session` in `crates/agent/acp/src/client.rs` to bind the matching response `result` instead of discarding it. Check and propagate the existing JSON-RPC `error` first; then treat an absent or JSON `null` result as an empty load result, deserialize any non-null result as `SessionLoadResult`, and derive the descriptor only from the returned config options.
- Assign the derived `Option<AgentSessionDescriptor>` to `self.session_descriptor` on every successful load, including `None`, before setting `self.session_id` and returning replayed history. Preserve response-id matching, history collection and ordering, non-matching-message handling, error text, logging, and session reuse behavior.
- Do not pass configured `ModelInfo` into load-session descriptor extraction and do not change `descriptor_from_config_options`, `AgentExecutor`, workflow events, TUI descriptor snapshots, or card rendering. Those downstream layers already render a descriptor when the ACP client supplies one and intentionally clear stale values when it supplies `None`.
- Update the directly related sentence in `docs/architecture.md` so it states that agent card descriptors are captured from agent-returned ACP config options when a session is created or loaded.

# Tests to be added/updated

- Preserve `load_session_captures_descriptor_from_returned_config_options` exactly as added by the investigator. It must pass because production `load_session` captures the returned model and reasoning options.
- Add a focused `SessionLoadResult` deserialization test named `session_load_result_defaults_missing_config_options` that deserializes `{}` and asserts `config_options.is_empty()`.
- Extend focused `Client::load_session` coverage for descriptorless successful responses. Cover JSON `null`, `{}`, a missing or empty `configOptions` list, and config options with no model/context/reasoning facet; each case must complete successfully and expose no descriptor.
- Include a stale-state assertion by seeding or creating a prior descriptor before a descriptorless successful load and verifying that `session_descriptor()` becomes `None`. This proves the load path does not leak the previous session's title suffix.
- Preserve the existing replay-history assertions in `test_load_session` so parsing the response payload cannot change the two collected history events or their order.
- Add or update focused coverage proving that a malformed non-null `session/load` result is returned as an error rather than silently treated as descriptorless success.

# How to verify

1. Run the unchanged investigator regression:

   ```bash
   cargo test -p cowboy-agent-acp client::tests::load_session_captures_descriptor_from_returned_config_options -- --exact
   ```

   Expected result: the command exits successfully; `session_descriptor()` contains model `github-copilot/gpt-5.6-sol` and reasoning `high` after `session/load`.

2. Run all focused load-session tests:

   ```bash
   cargo test -p cowboy-agent-acp load_session
   ```

   Expected result: descriptor-bearing, descriptorless, stale-clearing, malformed-payload, unsupported-capability, and replay-history cases all pass.

3. Run the complete ACP crate suite:

   ```bash
   cargo test -p cowboy-agent-acp
   ```

   Expected result: all ACP unit, integration, and documentation tests pass without changing new-session model selection or prompt/session behavior.

4. Check formatting and warnings for the touched Rust crate:

   ```bash
   cargo fmt --all -- --check
   cargo clippy -p cowboy-agent-acp --all-targets -- -D warnings
   ```

   Expected result: both commands exit successfully with no formatting differences, compiler warnings, or Clippy warnings.

# TODO

- [x] TODO-01: Model the optional `session/load` response config options.
  - Procedure: Add `SessionLoadResult` to `crates/agent/acp/src/messages.rs` with `#[serde(rename_all = "camelCase")]` and a defaulted `Vec<SessionConfigOption>` field named `config_options`; add `session_load_result_defaults_missing_config_options` to deserialize `{}` and assert `config_options.is_empty()`; run `cargo test -p cowboy-agent-acp messages::tests::session_load_result_defaults_missing_config_options -- --exact`.
  - Expected result: the command exits successfully, directly proving that `{}` deserializes as a `SessionLoadResult` with an empty option list; object payloads with `configOptions` use the shared `SessionConfigOption` type.
  - Implementer-observed result: Added the typed camel-case `SessionLoadResult` with a defaulted shared `SessionConfigOption` vector; the exact focused test command exited 0 with 1 test passed and proved `{}` produces an empty option list.

- [x] TODO-02: Restore or clear the ACP session descriptor from every successful load response.
  - Procedure: Edit `Client::load_session` in `crates/agent/acp/src/client.rs` to capture the matching response result, preserve existing error handling, map absent or JSON `null` results to no options, deserialize non-null payloads as `SessionLoadResult`, call `descriptor_from_config_options`, and assign the result to `self.session_descriptor` before committing the loaded session id.
  - Expected result: a loaded session reports the model/context/reasoning returned by the agent; descriptorless success reports `None`; malformed non-null results return an error; replayed history, response matching, and session id behavior remain unchanged.
  - Implementer-observed result: `Client::load_session` now checks the existing JSON-RPC error first, maps absent and null results to the default empty load result, deserializes other payloads, replaces `self.session_descriptor` from agent-returned config options before setting the session id, and leaves history collection and response matching unchanged.

- [x] TODO-03: Keep the investigator repro unchanged and add load-response compatibility regressions.
  - Procedure: Do not edit `load_session_captures_descriptor_from_returned_config_options`; add or update neighboring `Client` tests for JSON `null`, `{}`, empty or unrelated `configOptions`, stale descriptor clearing, malformed non-null payloads, and unchanged history replay, then run `cargo test -p cowboy-agent-acp load_session`.
  - Expected result: the unchanged repro passes with the returned model and reasoning, all descriptorless variants succeed with `session_descriptor() == None`, stale state is cleared, malformed data fails explicitly, and replay events retain their original count and order.
  - Implementer-observed result: The pre-implementation worktree diff contained the investigator-added `load_session_captures_descriptor_from_returned_config_options` test as the only change in `client.rs`; implementation did not modify that test. Added neighboring coverage for absent results, JSON null, `{}`, empty and unrelated config options, stale descriptor clearing, and malformed non-null data. After correcting only the new malformed-payload fixture from a valid sequence representation to an invalid string payload, the exact focused command exited 0 with all 6 matching tests passed, including the existing replay-order assertions.

- [x] TODO-04: Document descriptor capture for both new and loaded ACP sessions.
  - Procedure: Update the agent-card descriptor sentence in `docs/architecture.md` to mention session creation and session loading while retaining the agent-returned-values-only trust boundary.
  - Expected result: the architecture documentation matches runtime behavior and does not imply that configured `ModelInfo` can supply a card title.
  - Implementer-observed result: Updated the architecture sentence to cover both created and loaded sessions while explicitly retaining agent-returned ACP config options as the only source and excluding configured `ModelInfo`.

- [x] TODO-05: Run the focused regression, ACP suite, formatter, and Clippy checks.
  - Procedure: Execute the four command groups under `How to verify` in order and resolve any production-code, test, formatting, or warning failures without weakening the investigator test.
  - Expected result: every command exits with status 0, the loaded-session descriptor regression is green, and no Rust or Clippy warnings are introduced.
  - Implementer-observed result: Executed the unchanged repro, all focused load-session tests, the complete ACP crate suite, formatting check, and Clippy check in the declared order; all final commands exited 0, with 90 ACP library tests plus binary and doc-test targets passing and no formatting or warning failures.
