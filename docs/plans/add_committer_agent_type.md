# Plan

Define dedicated workflow agents with role-specific thinking levels: `planner` at `xhigh`,
`reviewer` at `high`, `implementer` at `medium`, and a lower-cost `committer` at `low`.
Configure both backend forms and route each workflow role to its named agent.

This revision replaces parse-only, machine-specific, and undefined checks with a layered,
falsifiable, reproducible acceptance-proof set that embeds no personal data. Each link in the
chain is proven independently:

1. The compiled `committer` role selects `agent = "committer"` — Lua loader tuple tests (TODO-07).
2. The `committer` role instructions are preserved **verbatim** — Lua exact-content assertion (TODO-03).
3. `AgentResolver` chooses the named `committer` config, carrying its complete lower-cost/low-thinking
   command and args — engine resolver unit test (TODO-10).
4. The shipped configs actually define a `committer` agent with the intended lower-cost model and
   low setting — durable `demo-config.toml` loader test asserting the complete ordered args (TODO-05),
   `tomllib` parse of the live config (TODO-01), full-contract parse of the README fenced TOML
   (TODO-06 + TODO-12), and a drift-independent pre/post render diff of both chezmoi branches (TODO-02).
5. The backend genuinely starts with that lower-cost model at the low setting and completes a
   harmless action in an isolated scratch directory — repeatable `acp-chat` smoke (TODO-11).
6. The chosen model and the exact configured flags are capability-backed with source-labeled
   provenance for the precise flags used (`--thinking`, `--reasoning-effort`) — captured command
   evidence (TODO-09).

The refined agent matrix is proven across the live OMP config, both rendered chezmoi backend
branches, the demo and README configs, the repository workflow roles, and their chezmoi mirrors.

Grounding facts that shape the design:

- Roles select a backend through the typed `RoleDefinition.agent` field, set in Lua with the
  table form `role("committer", { instructions = ..., agent = "committer" })`. The current
  `examples/workflows/roles/committer.lua` uses the plain-string form, which leaves
  `agent = nil`, so `AgentResolver` resolves it implicitly to the `default` agent. Adding a
  `committer` agent alone does **not** change routing; the role must name it.
- Reasoning/thinking level is a backend **launch argument**, not an `[agents.model]` field.
  The `omp` backend accepts `--thinking=<off|minimal|low|medium|high|xhigh|max|auto>`; the
  `agency copilot` backend accepts `--reasoning-effort=<none|minimal|low|medium|high|xhigh|max>`
  (the flag is spelled `--reasoning-effort` in its help; that exact spelling is what the config
  uses and what TODO-09 must substantiate). The existing `default`/`reviewer` agents use the
  maximum level. Because README documents that `[agents.model]` selects the ACP model while launch
  arguments remain authoritative for everything else, the low setting must be expressed through
  `args`, and the README example (via TODO-12) must configure it that way rather than implying
  `[agents.model]` sets reasoning.
- Lower-cost model: `claude-haiku-4.5` is the lowest-cost Claude tier available to the backend.
  Its supported thinking range is `minimal,low,medium,high,xhigh`; it does **not** support `max`.
  A lower thinking level is therefore both the intent and a hard requirement. Chosen value: `low`.
- The live config `~/.config/cowboy/config.toml` and the chezmoi source template
  `~/.local/share/chezmoi/dot_config/cowboy/config.toml.tmpl` are both chezmoi-managed. The live
  file currently uses the `omp` form; the template branches between a machine-specific
  `agency copilot` command and an `omp` fallback via an existing
  `{{ if (contains "<redacted-account>" .chezmoi.username) }}` / `{{ else }}` guard. The committer
  entry must appear in **both** branches. Both branches are verified without embedding the account
  identifier and without depending on the current machine by transforming the guard predicate
  structurally (matching `(contains "<any>" .chezmoi.username)` and forcing it to `true`/`false`)
  and rendering with `chezmoi execute-template`.
- Blast radius is contained: engine tests that run the example workflows through the commit step
  use a scripted agent factory that records only `role.id` and bypasses `AgentResolver`, so the
  role change breaks no engine test. The Lua loader assertion helper `assert_expected_role_agents`
  only validates the role tuples it is given, so it is safe to extend with a
  `("committer", "committer")` tuple.
- Coherence: because the example workflows will now require an agent named `committer`, the repo's
  runnable reference configs (`demo-config.toml`) and the documented config example (`README.md`)
  gain a matching `committer` agent so the example workflows remain runnable against the repo's own
  configs. This follows the established "migrate every caller" pattern from
  `docs/plans/add_named_agent_resolver.md`.

Chosen agent settings: planner = Claude Opus 4.8 / `xhigh`; reviewer = GPT 5.6 Sol / `high`;
implementer = Claude Opus 4.8 / `medium`; committer = Claude Haiku 4.5 / `low`.

# Changes

- Live user config `~/.config/cowboy/config.toml`: append a third `[[agents]]` entry named
  `committer` using the `omp` form:
  `command = "omp"`, `args = ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`.
- Chezmoi source template `~/.local/share/chezmoi/dot_config/cowboy/config.toml.tmpl`: append a
  `committer` `[[agents]]` entry that mirrors the existing machine-specific guard used by the
  `default` and `reviewer` entries, in **both** branches:
  - guarded branch (`agency copilot`): `command = "agency"`,
    `args = ["copilot", "--acp", "--model=claude-haiku-4.5", "--context=long_context", "--reasoning-effort=low"]`.
  - fallback branch (`omp`): `command = "omp"`,
    `args = ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`.
- OMP-backed `default` agents use `--thinking=auto` in the live config, the template's
  fallback branch, and `demo-config.toml`. The `agency copilot`/plain `copilot` forms retain
  their existing levels because their documented `--reasoning-effort` choices do not include
  `auto`.
- Add dedicated `planner` and `implementer` entries to the live OMP config and both chezmoi
  backend branches; reduce the existing reviewer from `max` to `high`. Preserve the cheaper
  committer at `low`.
- Route `planner.lua` to `agent = "planner"` and `implementer.lua` to
  `agent = "implementer"` in both the repository examples and chezmoi mirrors. Reviewer and
  committer already select their corresponding named agents.
- `examples/workflows/roles/committer.lua`: convert from the plain-string `role(id, instructions)`
  form to the table form, preserving the exact instruction text and adding `agent = "committer"`.
- Chezmoi-mirrored role `~/.local/share/chezmoi/dot_config/cowboy/workflows/roles/committer.lua`:
  apply the identical table-form change so the deployed committer role uses the new agent after
  `chezmoi apply` (this file is currently byte-identical to the repo example).
- `demo-config.toml`: append a `committer` `[[agents]]` entry mirroring the live-config `omp`
  form (`claude-haiku-4.5`, `--thinking=low`) so the repo's runnable demo config can serve the
  example workflows' committer role.
- `README.md`: add a `committer` `[[agents]]` entry to the Configuration example config block
  (TODO-06) and make that entry express the low reasoning/thinking level through a backend launch
  argument consistent with the live/chezmoi/demo configs (TODO-12), plus one prose sentence tying
  the low level to the launch argument rather than to `[agents.model]`.
- Expand `demo-config.toml` and the README example to define the complete four-agent matrix with
  the same models and OMP thinking levels.
- `crates/workflow/lua/src/loader.rs` (tests only):
  - add `("committer", "committer")` to the `assert_expected_role_agents` tuple lists for the
    `feature`, `bugfix`, and `dev-loop` workflows;
  - add a new test asserting the compiled `committer` role's `instructions` equal the exact
    expected string (verbatim-preservation proof).
- `crates/workflow/engine/src/agent_resolver.rs` (tests only): add a test proving a role with
  `agent = "committer"` resolves to a `committer` `AgentRuntimeConfig` carrying the exact command
  and complete lower-cost/low-thinking args.
- `crates/tui/app/src/config.rs` (tests only): assert the shipped `demo-config.toml` defines
  `planner`, `reviewer`, `implementer`, and `committer` with their complete commands and ordered
  model/thinking args, while preserving the OMP-backed default agent's `--thinking=auto` args.

Non-goals:

- No change to `AgentConfig`/`ModelConfig` structs or `AgentResolver` logic; the feature is
  entirely config + role metadata using existing mechanisms.
- No change to the commit step prompt, its output contract, or workflow transitions.
- No change to the built-in default workflow (it does not have a commit step).

# Tests to be added/updated

- Update `crates/workflow/lua/src/loader.rs`:
  - `examples_workflows_use_expected_named_agents`: add `("committer", "committer")` to both the
    `feature` and `bugfix` expected-agent tuple lists.
  - `dev_loop_gates_review_on_user_validation`: add `("committer", "committer")` to the `dev-loop`
    expected-agent tuple list.
  - Add `committer_role_preserves_instructions_verbatim`: compile an example workflow and assert
    the `committer` role's `instructions` equal the exact two-line string
    `"You are a release-minded committer.\nInspect the current changes, stage all request-related files including code, tests, documentation, generated plan documents, and other artifacts for the user's request, then create a local conventional commit. Never push, amend, rebase, or reset. Report the commit hash and message."`.
    This is a complete, deterministic, executable verbatim-preservation proof; no external command
    or baseline capture is needed.
- Add to `crates/workflow/engine/src/agent_resolver.rs`:
  - `resolve_selects_named_committer_agent_with_low_cost_args`: build `default`, `reviewer`, and
    `committer` `AgentRuntimeConfig`s (committer = `omp` with the complete args
    `["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`), then assert
    `resolve(role(agent = "committer"))` returns the committer config with matching `name`,
    `command`, and the complete `args` vector (full equality, not substring). This proves resolver
    selection and complete args carriage.
- Add/extend in `crates/tui/app/src/config.rs`:
  - `shipped_demo_config_defines_workflow_agents`: load the real `demo-config.toml` via
    `load_config` and assert complete ordered args for planner/Opus/`xhigh`,
    reviewer/GPT 5.6 Sol/`high`, implementer/Opus/`medium`, and
    committer/Haiku/`low`, plus the default agent's `['--thinking=auto', 'acp']` args.
- No new engine run test is required: existing example-workflow engine tests exercise the commit
  step through a scripted factory that ignores agent resolution, so they continue to pass unchanged
  and confirm no regression.

# How to verify

- Lua role/agent wiring and verbatim preservation (run each named test separately, because
  `cargo test` accepts only one positional filter):
  - `cargo test -p cowboy-workflow-lua examples_workflows_use_expected_named_agents`
  - `cargo test -p cowboy-workflow-lua dev_loop_gates_review_on_user_validation`
  - `cargo test -p cowboy-workflow-lua committer_role_preserves_instructions_verbatim`
- Resolver selection with complete args: `cargo test -p cowboy-workflow-engine resolve_selects_named_committer_agent_with_low_cost_args`.
- Shipped demo config (complete ordered args): `cargo test -p cowboy config::tests::shipped_demo_config_defines_workflow_agents`.
- No wider regression: `cargo test -p cowboy-workflow-lua`, `cargo test -p cowboy-workflow-engine`,
  and `cargo test -p cowboy`.
- Lint every touched Rust crate with test targets:
  - `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings`
  - `cargo clippy -p cowboy-workflow-engine --all-targets -- -D warnings`
  - `cargo clippy -p cowboy --all-targets -- -D warnings`
- Default-level capability: `omp --help` lists `auto` for `--thinking`; `copilot --help` and
  `agency copilot --help` omit `auto` from `--reasoning-effort`.
- Live config correctness (target-scoped, no personal data): `cargo run -p cowboy -- runs` parses
  without error, and the following prints the committer command and complete args:

  ```bash
  python3 - <<'PY'
  import tomllib, pathlib, os
  path = pathlib.Path(os.path.expanduser("~/.config/cowboy/config.toml"))
  data = tomllib.loads(path.read_text())
  committer = [a for a in data["agents"] if a["name"] == "committer"][0]
  assert committer["command"] == "omp", committer
  assert committer["args"] == ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"], committer
  print("live committer ok:", committer["command"], committer["args"])
  PY
  ```

- Demo config correctness: `cargo run -p cowboy -- --config demo-config.toml runs` parses without
  error (durable complete-array assertion covered by the demo-config test above).
- README fenced TOML correctness (real parse of the full contract, not a name grep): extract the
  single fenced TOML block from `README.md` and parse it, asserting the committer agent's command,
  Haiku model argument, and low launch flag. This avoids embedding literal fence markers by
  computing the fence at runtime (`chr(96)*3`):

  ```bash
  python3 - <<'PY'
  import tomllib, pathlib
  fence = chr(96) * 3
  lines = pathlib.Path("README.md").read_text().splitlines()
  block, cur, infence = None, [], False
  for line in lines:
      if not infence and line.startswith(fence + "toml"):
          infence, cur = True, []
      elif infence and line.startswith(fence):
          block = "\n".join(cur); break
      elif infence:
          cur.append(line)
  data = tomllib.loads(block)
  committer = [a for a in data["agents"] if a["name"] == "committer"][0]
  args = committer.get("args", [])
  assert committer["command"] == "omp", committer
  assert "--model=github-copilot/claude-haiku-4.5" in args, committer
  assert "--thinking=low" in args, committer
  print("README committer ok:", committer["command"], args)
  PY
  ```

- Chezmoi both-branch render, drift-independent and account-anonymized. Render both branches from
  the template into temp files **before** the edit and again **after** the edit, then diff pre-vs-post
  per branch. This never reads the deployed file, so the result is independent of any managed-file
  drift, and the guard predicate is transformed structurally so no username appears:

  ```bash
  TMPL="$HOME/.local/share/chezmoi/dot_config/cowboy/config.toml.tmpl"
  render() { sed -E "s/\(contains \"[^\"]*\" \.chezmoi\.username\)/$1/g" "$TMPL" | chezmoi execute-template; }
  # BEFORE editing the template:
  render true  > /tmp/committer_pre_true.toml
  render false > /tmp/committer_pre_false.toml
  # ... apply the TODO-02 edit to "$TMPL" ...
  # AFTER editing the template:
  render true  > /tmp/committer_post_true.toml
  render false > /tmp/committer_post_false.toml
  diff /tmp/committer_pre_true.toml  /tmp/committer_post_true.toml
  diff /tmp/committer_pre_false.toml /tmp/committer_post_false.toml
  ```

  The forced-`true` diff must show only the added `agency`/`--reasoning-effort=low` committer
  block. The forced-`false` diff must show the `omp`/`--thinking=low` committer block plus the
  requested default-agent change from `--thinking=max` to `--thinking=auto`.
- Backend capability provenance for the exact configured flags (TODO-09) and the isolated,
  repeatable live committer-backend smoke (TODO-11) as defined in their TODO procedures below.

# TODO

- [x] TODO-01: Add a `committer` `[[agents]]` entry to the live config `~/.config/cowboy/config.toml` using `command = "omp"` and `args = ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`.
  - Procedure: Append the new `[[agents]]` block after the `reviewer` entry. Then (a) run `cargo run -p cowboy -- runs`; and (b) run the target-scoped correctness check `python3 -c 'import tomllib,pathlib,os; d=tomllib.loads(pathlib.Path(os.path.expanduser("~/.config/cowboy/config.toml")).read_text()); c=[a for a in d["agents"] if a["name"]=="committer"][0]; print(c["command"], c["args"])'`.
  - Expected result: (a) exits without a config parse/validation error; (b) prints `omp` followed by an args list that contains exactly `--model=github-copilot/claude-haiku-4.5` and `--thinking=low`. Both must hold.
  - Evidence: `cargo run -p cowboy -- runs` exited successfully; `tomllib` asserted the exact
    committer command and args in the live file.
- [x] TODO-02: Add a `committer` `[[agents]]` entry to the chezmoi source template `~/.local/share/chezmoi/dot_config/cowboy/config.toml.tmpl`, mirroring the existing machine-specific `{{ if }}`/`{{ else }}` guard: guarded branch `command = "agency"`, `args = ["copilot", "--acp", "--model=claude-haiku-4.5", "--context=long_context", "--reasoning-effort=low"]`; fallback branch `command = "omp"`, `args = ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`.
  - Procedure: (1) Before editing, render both guard outcomes from the template into baseline temp files using an account-anonymized structural transform of the predicate: `TMPL="$HOME/.local/share/chezmoi/dot_config/cowboy/config.toml.tmpl"`, then `sed -E 's/\(contains "[^"]*" \.chezmoi\.username\)/true/g' "$TMPL" | chezmoi execute-template > /tmp/committer_pre_true.toml` and the same with `false` into `/tmp/committer_pre_false.toml`. (2) Append the guarded `committer` block after the `reviewer` entry in the template. (3) Re-render both outcomes into `/tmp/committer_post_true.toml` and `/tmp/committer_post_false.toml` with the identical commands. (4) Compare pre vs post per branch: `diff /tmp/committer_pre_true.toml /tmp/committer_post_true.toml` and `diff /tmp/committer_pre_false.toml /tmp/committer_post_false.toml`. This comparison never reads the deployed file, so it is independent of any managed-file drift, and the transform embeds no account identifier.
  - Expected result: the forced-`true` diff shows only an added block `name = "committer"` with `command = "agency"` and args `["copilot", "--acp", "--model=claude-haiku-4.5", "--context=long_context", "--reasoning-effort=low"]`; the forced-`false` diff shows an added block `name = "committer"` with `command = "omp"` and args `["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`, plus the TODO-13 default-agent level change from `--thinking=max` to `--thinking=auto`; no other lines change.
  - Evidence: account-anonymized pre/post renders produced exactly those branch-specific diffs.
- [x] TODO-03: Convert `examples/workflows/roles/committer.lua` to the table form `role("committer", { instructions = <existing text unchanged>, agent = "committer" })`, preserving the current instruction string verbatim.
  - Procedure: Read the current instruction text directly from `examples/workflows/roles/committer.lua`. Rewrite the file to the table form, pasting the identical instruction string and adding `agent = "committer"`. Prove verbatim preservation with the exact-content unit test only: `cargo test -p cowboy-workflow-lua committer_role_preserves_instructions_verbatim`, which asserts the compiled `committer` role's `instructions` equal the exact expected two-line string byte-for-byte.
  - Expected result: the test passes, asserting `role.instructions` equals the exact expected string; a single changed byte in the instruction text fails the test. This is the complete, deterministic comparison — no baseline capture or auxiliary command is used.
  - Evidence: `cargo test -p cowboy-workflow-lua committer_role_preserves_instructions_verbatim`
    passed.
- [x] TODO-04: Apply the identical table-form change (with `agent = "committer"`) to the chezmoi-mirrored role `~/.local/share/chezmoi/dot_config/cowboy/workflows/roles/committer.lua`.
  - Procedure: Edit the chezmoi source role to match `examples/workflows/roles/committer.lua`; then run `diff examples/workflows/roles/committer.lua ~/.local/share/chezmoi/dot_config/cowboy/workflows/roles/committer.lua`.
  - Expected result: `diff` reports no differences (the two committer role files are byte-identical again).
  - Evidence: `diff` exited zero with no output; the role files are byte-identical.
- [x] TODO-05: Add a `committer` `[[agents]]` entry to `demo-config.toml` using `command = "omp"` and `args = ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`.
  - Procedure: Append the block after the `reviewer` entry. Then add/run the durable loader test `cargo test -p cowboy config::tests::shipped_demo_config_defines_committer_agent`, which loads the real `demo-config.toml` through `load_config` and asserts the committer agent's `command` and the complete ordered `args` array; also run `cargo run -p cowboy -- --config demo-config.toml runs`.
  - Expected result: the test passes, asserting an agent named `committer` with `command == "omp"` and `args == ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]` (complete ordered array, including `acp`, so any extra or conflicting argument fails); the `runs` command loads `demo-config.toml` without a config parse/validation error.
  - Evidence: the named demo-config test passed and `cargo run -p cowboy -- --config
    demo-config.toml runs` exited successfully.
- [x] TODO-06: Add a `committer` `[[agents]]` entry to the Configuration example config block in `README.md` (following that block's `command = "copilot"` + `[agents.model]` style with `id = "claude-haiku-4.5"`, `provider = "github-copilot"`) and add one sentence stating the committer agent uses a lower-cost model at a lower reasoning/thinking level for the commit step.
  - Note: TODO-12 supersedes the command/launch-argument representation of this entry (converting it to the grounded `omp` launch-argument form). This TODO's durable subject is the presence of a committer entry in the README example that references the lower-cost Haiku model plus the explanatory prose sentence; TODO-12 owns the exact command/argument contract.
  - Procedure: Edit the fenced TOML config example to add a committer agent referencing model `claude-haiku-4.5`, and add the prose sentence. Then parse the fenced block and assert the committer agent exists and references the Haiku model (in `[agents.model].id` or a `--model` argument): `python3 - <<'PY'` / `import tomllib, pathlib` / `fence = chr(96)*3` / (extract the single fenced ```toml``` block as in the How-to-verify script) / `committer = [a for a in data["agents"] if a["name"]=="committer"][0]` / `model = committer.get("model", {}).get("id") if committer.get("model") else next((x.split("=",1)[1].split("/")[-1] for x in committer.get("args", []) if x.startswith("--model=")), None)` / `assert model == "claude-haiku-4.5", committer` / `print("README committer model ok:", model)` / `PY`. Re-read the edited prose sentence.
  - Expected result: the fenced TOML parses successfully; a committer agent is present and references `claude-haiku-4.5`; the script prints `README committer model ok: claude-haiku-4.5`; and the prose sentence describing the lower-cost/lower-reasoning committer agent for the commit step is present next to the config block.
  - Evidence: the full fenced TOML block parsed successfully; the committer model resolved to
    `claude-haiku-4.5`, and the adjacent prose documents the lower thinking level.
- [x] TODO-07: Add `("committer", "committer")` to the expected-agent tuple lists in `crates/workflow/lua/src/loader.rs` for the `feature` and `bugfix` cases in `examples_workflows_use_expected_named_agents` and the `dev-loop` case in `dev_loop_gates_review_on_user_validation`.
  - Procedure: Insert the tuple into each of the three lists; then run `cargo test -p cowboy-workflow-lua examples_workflows_use_expected_named_agents` and, separately, `cargo test -p cowboy-workflow-lua dev_loop_gates_review_on_user_validation`.
  - Expected result: both tests pass, asserting all three example workflows route the committer role to the `committer` agent (`role.agent == "committer"`).
  - Evidence: both focused Lua tests passed, covering all three workflow definitions.
- [x] TODO-08: Run the focused verification suite and lints for the touched crate.
  - Procedure: Run `cargo test -p cowboy-workflow-lua`, `cargo test -p cowboy-workflow-engine`, and `cargo test -p cowboy`; then lint every touched Rust crate with test targets: `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings`, `cargo clippy -p cowboy-workflow-engine --all-targets -- -D warnings`, and `cargo clippy -p cowboy --all-targets -- -D warnings`.
  - Expected result: all three test commands and all three clippy commands succeed with zero test failures and zero clippy warnings. Because tests were added in the `cowboy-workflow-lua`, `cowboy-workflow-engine`, and `cowboy` crates, `--all-targets` clippy is run for each so the new test modules are linted.
  - Evidence: Lua 56/56, engine 127/127, and Cowboy 312/312 tests passed (two existing Cowboy
    tests ignored); all three `cargo clippy --all-targets -- -D warnings` commands passed.
- [x] TODO-09: Capture source-labeled backend capability evidence proving `claude-haiku-4.5` supports the `low` reasoning/thinking level (and does not require `max`) and that the `omp --thinking` and `agency copilot --reasoning-effort` flags accept `low`.
  - Procedure: Run and record, each labeled with its exact command as the source: (1) `omp models` — capture the `claude-haiku-4.5` row; (2) `omp --help` — capture the `--thinking` line; (3) `agency copilot --help` — capture the line documenting the exact configured flag `--reasoning-effort`. Retain verbatim output as evidence with provenance.
  - Expected result: the `omp models` row for `claude-haiku-4.5` lists supported reasoning levels including `low` and not including `max`; `omp --help` shows `--thinking` accepting `low`; and `agency copilot --help` shows the exact string `--reasoning-effort` accepting `low`. Evidence must substantiate the exact `--reasoning-effort` flag the configuration uses; evidence limited to an alternate spelling (for example `--effort`) does not satisfy this step.
  - Evidence (`omp models`): `claude-haiku-4.5` lists
    `minimal,low,medium,high,xhigh`; `max` is absent.
  - Evidence (`omp --help`): `--thinking=<value>` accepts
    `off, minimal, low, medium, high, xhigh, max, auto`.
  - Evidence (`agency copilot --help`): `--effort, --reasoning-effort <level>` accepts
    `none, minimal, low, medium, high, xhigh, max`.
- [x] TODO-10: Add an engine `AgentResolver` test proving a role with `agent = "committer"` resolves to the committer `AgentRuntimeConfig`, carrying its exact command and lower-cost/low-thinking args.
  - Procedure: In `crates/workflow/engine/src/agent_resolver.rs` tests, construct `default`, `reviewer`, and `committer` configs (committer = `omp` with the complete args `["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`), build the resolver, resolve a `RoleDefinition` whose `agent = Some("committer")`, and assert the returned config's `name`, `command`, and complete `args` vector by full equality. Run `cargo test -p cowboy-workflow-engine resolve_selects_named_committer_agent_with_low_cost_args`.
  - Expected result: the test passes, asserting the resolver returns the config named `committer` with `command == "omp"` and `args == ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]` (complete vector equality) — proving named selection and that the resolved agent carries the intended lower-cost model and low setting.
  - Evidence: `cargo test -p cowboy-workflow-engine
    resolve_selects_named_committer_agent_with_low_cost_args` passed.
- [x] TODO-11: Perform a harmless minimal committer-backend smoke that launches the committer's exact command and args (not the real commit prompt) and observe the echoed command arguments, session creation with the lower-cost model at the low setting, and a successful agent response.
  - Procedure: (1) Build the `acp-chat` test binary idempotently (re-runnable, no `mkdir` that fails on re-run): `cargo build -p cowboy-agent-acp --bin acp-chat`. (2) Capture an absolute binary path: `BIN="$(realpath target/debug/acp-chat)"`. (3) Create an isolated scratch directory and use it as the ACP working directory so the repository is never exposed to the agent: `SCRATCH="$(mktemp -d)"`. (4) Launch the committer backend with the committer's exact command/args and a harmless prompt, piping input so it exits cleanly, from any directory using the absolute path and explicit cwd: `printf 'Reply with exactly: OK\n:quit\n' | COWBOY_ACP_COMMAND=omp COWBOY_ACP_ARGS='--model=github-copilot/claude-haiku-4.5 --thinking=low acp' COWBOY_ACP_MODEL=github-copilot/claude-haiku-4.5 COWBOY_ACP_PROVIDER=github-copilot COWBOY_ACP_CWD="$SCRATCH" "$BIN"`. (The "committer agent was selected by name" link is proven deterministically by TODO-10; this step proves the selected command/args actually start the lower-cost/low-setting backend and complete a turn.) Requires `omp` authenticated on PATH. `acp-chat` reads `COWBOY_ACP_CWD` for the ACP session working directory, so the agent operates only inside the scratch directory.
  - Expected result: stderr echoes `Starting ACP agent: omp --model=github-copilot/claude-haiku-4.5 --thinking=low acp` (the committer's exact command arguments), followed by a `Connected to ...` line and a `Session: <id>` line (session creation succeeded with the `claude-haiku-4.5` model at `--thinking=low`), and the harmless prompt returns a non-empty agent response before clean exit. The ACP working directory is the scratch dir, not the repository. A failure to start, an unsupported-model/level error, or an empty response falsifies the step.
  - Evidence: isolated scratch-directory execution echoed the exact OMP command, connected to
    `oh-my-pi 17.0.5`, created a session (identifier redacted), returned exactly `OK`, and exited
    successfully.
- [x] TODO-12: Make the `README.md` committer example express the low reasoning/thinking level through a backend launch argument consistent with the live, chezmoi, and demo configs — convert the committer entry added in TODO-06 to the `omp` form (`command = "omp"`, `args = ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]`) and update the accompanying prose to attribute the low reasoning/thinking level to the backend launch argument (`--thinking=low` for `omp`; `--reasoning-effort=low` for `agency copilot`), without implying `[agents.model]` controls the reasoning level.
  - Procedure: Edit the fenced TOML config example so the committer entry uses the `omp` launch-argument form, and adjust the prose. Then run the full-contract README parse from the "How to verify" section (the `python3 - <<'PY' ... PY` heredoc that extracts the single fenced TOML block via `chr(96)*3` and asserts `committer["command"] == "omp"`, `--model=github-copilot/claude-haiku-4.5` in `args`, and `--thinking=low` in `args`). Re-read the edited prose sentence.
  - Expected result: the parse script prints `README committer ok: omp [...]` with the assertions passing (command is `omp`, the Haiku model argument and `--thinking=low` are both present); and the prose ties the lower reasoning/thinking level to the backend launch argument, not to `[agents.model]`. This supersedes the placeholder command form from TODO-06 while keeping TODO-06's committer-presence and Haiku-model outcomes true.
  - Evidence: the full-contract README parse asserted the exact command and complete args array;
    the prose attributes the level to backend launch arguments.

- [x] TODO-13: Set the default agent's thinking/reasoning level to `auto` wherever the configured backend supports it.
  - Procedure: Inspect `omp --help`, `copilot --help`, and `agency copilot --help`; set
    `--thinking=auto` on OMP-backed default-agent entries in the live config, chezmoi fallback,
    and demo config; retain existing levels for Copilot-backed forms when `auto` is unsupported.
    Assert the exact live and demo args and smoke OMP with Claude Opus 4.8 at `--thinking=auto`.
  - Expected result: every OMP-backed default agent uses `--thinking=auto`; Copilot-backed forms
    remain valid without an unsupported `auto` effort; the exact-args assertions and smoke pass.
  - Evidence: `omp --help` lists `auto`, while both Copilot help commands omit it from reasoning
    effort choices; live/demo exact-args assertions passed; OMP returned `OK` with
    `--model=github-copilot/claude-opus-4.8 --thinking=auto`.

- [x] TODO-14: Define and route the planner agent at thinking level `xhigh`.
  - Procedure: Add planner entries using Claude Opus 4.8 and `xhigh` to the live OMP config,
    both chezmoi backend branches, demo config, and README; set repository and mirrored
    `planner.lua` files to `agent = "planner"`; update loader expectations and config assertions.
  - Expected result: every config resolves planner to the intended backend/model at `xhigh`, and
    every example workflow's planner role selects the named planner agent.
  - Evidence: exact TOML assertions passed for the live config, demo config, README, and both
    rendered template branches; focused Lua routing tests and mirrored-file diffs passed.

- [x] TODO-15: Define the reviewer agent at thinking level `high`.
  - Procedure: Change reviewer launch args from `max` to `high` in the live config and both
    chezmoi branches; align demo and README configs with GPT 5.6 Sol at `high`; retain the
    existing `agent = "reviewer"` role routing.
  - Expected result: reviewer-backed roles resolve to GPT 5.6 Sol at `high` in every config.
  - Evidence: exact TOML assertions passed for all four config surfaces; Lua routing tests confirm
    reviewer and validator roles continue selecting the named reviewer agent.

- [x] TODO-16: Define and route the implementer agent at thinking level `medium`.
  - Procedure: Add implementer entries using Claude Opus 4.8 and `medium` to the live OMP config,
    both chezmoi backend branches, demo config, and README; set repository and mirrored
    `implementer.lua` files to `agent = "implementer"`; update loader expectations and assertions.
  - Expected result: every config resolves implementer to the intended backend/model at `medium`,
    and every example workflow's implementer role selects the named implementer agent.
  - Evidence: exact TOML assertions passed for all config surfaces; focused Lua routing tests and
    mirrored-file diffs passed.

- [x] TODO-17: Preserve the cheaper committer agent at thinking level `low`.
  - Procedure: Keep Claude Haiku 4.5 with `low` in the live, template, demo, and README configs;
    retain `agent = "committer"` in repository and mirrored role files.
  - Expected result: every workflow routes commit steps to the cheaper Haiku agent at `low`.
  - Evidence: exact TOML assertions passed across all config surfaces, Lua routing tests passed,
    and the isolated committer ACP smoke returned `OK` with Haiku at `low`.

- Refined-matrix regression evidence: Lua 56/56, engine 127/127, and Cowboy 312/312 tests passed
  (two existing Cowboy tests ignored); Clippy passed with `-D warnings` for both touched crates.
