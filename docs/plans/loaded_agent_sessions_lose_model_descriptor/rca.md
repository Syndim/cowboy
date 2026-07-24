# Bug behavior

Agent tool, response, thought, prompt, and plan cards show the model descriptor after a new ACP session is created, but the descriptor disappears when Cowboy resumes a persisted ACP session through `session/load`.

The reported card titles retain the time, action title, step, and run metadata but omit the expected model suffix:

```text
16:15 (+04:04:02) · ✓ • Resolve published Android revision · ↳ implement · ▶ <run>
16:15 (+04:04:09) · ✓ • apply_patch · ↳ implement · ▶ <run>
16:19 (+04:07:43) · ● • Resolve package and build Android APK · ↳ implement · ▶ <run>
```

The behavior is intermittent from the user's perspective because newly created sessions capture the descriptor, while sessions loaded into a fresh ACP client do not.

# Root cause

`cowboy-agent-acp::Client::load_session` ignores the successful `session/load` response result. In particular, it does not deserialize the returned `configOptions` or rebuild `self.session_descriptor`, although `Client::new_session` does both for `session/new`.

A fresh client used to load a persisted session therefore keeps `session_descriptor` as `None`. The workflow agent emits `AgentSessionReady { descriptor: None }`; TUI state clears the descriptor for that run and step; subsequent agent cards render without a model suffix.

# Root cause evidence

No diagnostic log bundle was supplied, so the flow is grounded in the reported card output and specific source locations.

1. The reported title has normal agent-tool metadata but no descriptor:

   ```text
   16:15 (+04:04:09) · ✓ • apply_patch · ↳ implement · ▶ <run>
   ```

   This shows the event reached the agent-card renderer. It is not a generic workflow card and is not missing the rest of its title metadata.

2. A fresh ACP client starts without a session descriptor in `crates/agent/acp/src/client.rs` (`Client::connect_with_transport` initialization):

   ```rust
   session_descriptor: None,
   ```

   Loading a persisted session must therefore populate the descriptor on this new client.

3. New-session handling does populate it in `crates/agent/acp/src/client.rs`, `Client::new_session`:

   ```rust
   let session: SessionNewResult = serde_json::from_value(result)?;
   let mut descriptor_options = session.config_options.clone();
   // ...
   self.session_descriptor = Self::descriptor_from_config_options(&descriptor_options);
   ```

   This is why cards can show model names for newly created sessions.

4. Loaded-session handling in `crates/agent/acp/src/client.rs`, `Client::load_session`, matches the response by id but discards its `result`:

   ```rust
   Message::Response {
       id: resp_id, error, ..
   } if resp_id == id => {
       // ...
       self.session_id = Some(session_id.to_string());
       return Ok(history);
   }
   ```

   The `..` drops the successful response payload. Even when the agent returns `configOptions` containing the active model and reasoning level, `self.session_descriptor` remains `None`.

5. `crates/workflow/agent/src/executor.rs`, `AgentExecutor::execute_agent`, reads only the backend-reported descriptor:

   ```rust
   let descriptor = active
       .client
       .session_descriptor()
       .and_then(aggregate_session_descriptor);
   ```

   It then emits `AgentProgressKind::SessionReady` with that `None` value. The configured `ModelInfo` is intentionally not used as a fallback.

6. `crates/workflow/engine/src/runtime.rs`, `WorkflowRuntime::workflow_event_kind_from_agent_progress`, preserves the missing value in `WorkflowEventKind::AgentSessionReady`.

7. `crates/tui/app/src/app/state.rs`, `AppState::apply_workflow_event_metadata`, handles `AgentSessionReady { descriptor: None }` by removing the current `(run_id, step_id)` descriptor:

   ```rust
   None => {
       self.agent_descriptors.remove(&key);
   }
   ```

   Later tool events snapshot no descriptor.

8. `crates/tui/app/src/app/events.rs`, `render_workflow_event_width`, adds the title suffix only when the snapshot is present:

   ```rust
   if let Some(descriptor) = agent_descriptor
       && agent_card_step_id(&event.kind).is_some()
   {
       card = card.title_suffix(descriptor);
   }
   ```

   Because the loaded-session path produced `None`, the reported tool cards render with no model name.

# Reproduction steps

1. Create a mock ACP client whose initialize response advertises `loadSession`.
2. Return a successful `session/load` response containing `configOptions` with model `github-copilot/gpt-5.6-sol` and reasoning `high`.
3. Call `Client::load_session`.
4. Read the provider-neutral descriptor through `cowboy_agent_client::Client::session_descriptor`.
5. Observe that the call returns `None` instead of the model and reasoning returned by the ACP agent.

# Regression test

- Test file: `crates/agent/acp/src/client.rs`
- Test name: `client::tests::load_session_captures_descriptor_from_returned_config_options`
- Command: `cargo test -p cowboy-agent-acp client::tests::load_session_captures_descriptor_from_returned_config_options -- --exact`
- Expected failure before the fix: the test panics because `session_descriptor()` is `None` after a successful `session/load` response that includes model config options.

# Current failing result

```text
running 1 test
test client::tests::load_session_captures_descriptor_from_returned_config_options ... FAILED

thread 'client::tests::load_session_captures_descriptor_from_returned_config_options' panicked at crates/agent/acp/src/client.rs:2899:14:
session/load config options should restore the model descriptor

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 86 filtered out
error: test failed, to rerun pass `-p cowboy-agent-acp --lib`
```

# Fix constraints

- Preserve the trust boundary: derive the title descriptor only from agent-returned ACP values, never from configured `ModelInfo`.
- Handle successful `session/load` responses that contain `configOptions` while remaining compatible with agents that return `null`, an empty object, or no descriptor facets.
- Do not change session history replay, response-id matching, error propagation, or persisted session reuse behavior.
- Keep TUI snapshot semantics unchanged: `descriptor: None` must still clear stale data when the backend genuinely reports no descriptor.
- Product code was not changed during this investigation.
