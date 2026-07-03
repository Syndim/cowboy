# Plan

Add named ACP agent configurations and resolve the backend per workflow role. Replace the single `[agent]` config with a `[[agents]]` list. Each entry must have a non-empty unique `name`, plus the existing `command`, `args`, and `model` fields. Defaults should create one agent named `default` using the current `copilot --acp` model defaults.

Workflow roles can opt into a configured agent with `role("developer", { instructions = "...", agent = "planner" })`. The agent name is role metadata, not an action override: every agent action using that role resolves the same backend.

Put the resolution rule behind a small engine-owned `AgentResolver` interface so lookup policy is implemented once:

1. If a role specifies an agent name, resolve only an exact configured-name match; otherwise fail before starting an ACP process.
2. If a role does not specify an agent name:
   1. use the configured agent named `default` when present;
   2. otherwise use the only configured agent when the list length is one;
   3. otherwise fail with an actionable ambiguous-agent error.

Do a clean config cutover. Update repository examples and README to `[[agents]]`; do not keep a long-lived `[agent]` compatibility shim.

# Changes

- Update `cowboy-workflow-core`:
  - add `agent: Option<String>` to `RoleDefinition`;
  - validate non-empty role agent names when present;
  - preserve existing role id, instructions, properties, and step role validation behavior.
- Update `cowboy-workflow-lua`:
  - accept `agent = "name"` in role table config;
  - store it in the typed `RoleDefinition.agent` field instead of generic `properties`;
  - reject non-string or blank role agent names with a clear Lua conversion error;
  - keep `role("id", "instructions")` and roles without `agent` working unchanged.
- Update `cowboy-workflow-engine`:
  - change `RuntimeConfig` from `agent: AgentRuntimeConfig` to `agents: Vec<AgentRuntimeConfig>`;
  - add `name: String` to `AgentRuntimeConfig`;
  - add an `AgentResolver` module with validation for empty lists, blank names, and duplicate names;
  - make `WorkflowRuntime` build an ACP client factory from the resolver;
  - ensure explicit missing agents and ambiguous implicit agents fail before spawning a client.
- Update `cowboy-workflow-agent`:
  - pass the compiled `RoleDefinition` into agent execution through the core execution path;
  - change the client factory interface so it can resolve from role metadata rather than only `role_id`;
  - carry the resolved agent's `model` into `new_session` so different agents can use different models;
  - keep session reuse keyed by `(run_id, role_id)` so one workflow role keeps one durable backend session;
  - build prompts from the real role instructions while touching this path, instead of synthesizing an empty role.
- Update `cowboy` TUI config loading:
  - replace `AppConfig.agent` with `AppConfig.agents`;
  - parse TOML `[[agents]]` with nested `[agents.model]` / inline model support matching serde's list-of-tables shape;
  - default to a single `default` agent;
  - convert the list to engine `RuntimeConfig` without adding runtime logic to the TUI crate.
- Update test apps and docs:
  - update `crates/workflow/engine/src/bin/engine-cli.rs` to emit one named default agent from env/preset values;
  - update `README.md`, `demo-config.toml`, and the loaded `AGENTS.md` config example in the implementation branch to show `[[agents]]`;
  - update any runtime tests constructing `AgentRuntimeConfig` literals.

# Tests to be added/updated

- Add `cowboy-workflow-engine` unit tests for `AgentResolver`:
  - explicit agent name resolves by exact match;
  - explicit missing name fails;
  - implicit resolution prefers `default` when present;
  - implicit resolution uses the only configured agent when there is exactly one;
  - implicit resolution fails when multiple non-default agents exist;
  - blank, duplicate, and empty agent lists fail during resolver construction.
- Add `cowboy-workflow-lua` tests proving role table config converts `agent = "planner"` into `RoleDefinition.agent`, leaves it out of `properties`, and rejects blank/non-string values.
- Add `cowboy-workflow-core` validation tests for blank role agent names.
- Update `cowboy-workflow-agent` tests so fake factories receive the role metadata and session creation uses the resolved model for that role.
- Update `cowboy` config tests to parse `[[agents]]`, verify default config contains one `default` agent, and verify runtime config conversion preserves every named agent.
- Update engine runtime tests to cover agent resolution failure surfacing as a run failure/error before client spawn where practical with fake/missing backends.

# How to verify

- Run `cargo test -p cowboy-workflow-core` for role definition validation.
- Run `cargo test -p cowboy-workflow-lua` for Lua role metadata conversion.
- Run `cargo test -p cowboy-workflow-agent` for factory interface, model propagation, prompt construction, and session reuse.
- Run `cargo test -p cowboy-workflow-engine agent_resolver` for resolver branch coverage.
- Run `cargo test -p cowboy config::tests` for TUI config parsing and runtime conversion.
- Run `cargo test` after the focused tests pass because the config and factory interface touch multiple crates.
- Manual smoke check with a temporary config containing two agents (`default` and another named agent) and a workflow role using `agent = "other"`; confirm logs/errors show the named backend is selected. Repeat with an unknown agent name and with two unnamed-default candidates to confirm actionable failures.

# TODO

- [x] Add `RoleDefinition.agent: Option<String>` in `cowboy-workflow-core`.
- [x] Add core validation for blank role agent names.
- [x] Update Lua role conversion to parse typed `agent` metadata.
- [x] Update Lua conversion errors and tests for blank/non-string role agent values.
- [x] Replace engine `RuntimeConfig.agent` with `RuntimeConfig.agents`.
- [x] Add `AgentRuntimeConfig.name` and update constructors/literals.
- [x] Implement engine-owned `AgentResolver` with exact/default/singleton/ambiguous rules.
- [x] Validate resolver inputs for empty lists, blank names, and duplicate names.
- [x] Change agent client factory interface to receive role metadata and return resolved model/client metadata.
- [x] Thread compiled `RoleDefinition` from core `execute_step` into agent execution context.
- [x] Update `AgentExecutor` to use real role instructions in prompts.
- [x] Update `AgentExecutor` session creation to use the resolved agent model.
- [x] Preserve `(run_id, role_id)` session reuse behavior after resolver changes.
- [x] Update ACP factory construction to resolve role agent names before process spawn.
- [x] Update TUI `AppConfig` to parse `[[agents]]` and default one `default` agent.
- [x] Update TUI runtime-config conversion to pass all agents into the engine.
- [x] Update `engine-cli` preset/env config to produce one named default agent.
- [x] Update README configuration examples to `[[agents]]`.
- [x] Update `demo-config.toml` to `[[agents]]`.
- [x] Update loaded project guide config examples to `[[agents]]` during implementation.
- [x] Update all tests and test fixtures constructing agent runtime configs.
- [x] Add resolver branch tests in `cowboy-workflow-engine`.
- [x] Add Lua role agent conversion tests in `cowboy-workflow-lua`.
- [x] Add config parsing/conversion tests in `cowboy`.
- [x] Add agent executor tests for role metadata and resolved model propagation.
- [x] Run focused crate tests for core, Lua, agent, engine resolver, and TUI config.
- [x] Run full `cargo test` after focused coverage passes.
- [x] Perform the manual multi-agent config smoke checks.
- [x] Require explicit `name` in every configured `[[agents]]` entry.
- [x] Reject blank and duplicate configured agent names during config loading.
- [x] Reject legacy `[agent]` config tables after the `[[agents]]` cutover.
