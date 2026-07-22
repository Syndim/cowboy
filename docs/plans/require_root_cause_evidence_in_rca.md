# Require root cause evidence in the bugfix RCA

## Plan

Update the `bugfix` example workflow so the investigator must prove the stated
root cause is correct, not merely assert it. The proof is a new mandatory
**Root cause evidence** section in the RCA document that contains a concrete,
step-by-step walkthrough of how the bug happens — preferably an example flow
reconstructed from real log lines (each quoted line explained as the flow
advances toward the failure), falling back to specific source locations (file /
function / line) when logs are unavailable. The RCA reviewer must reject an RCA
whose root cause is asserted without this traceable, step-by-step evidence.

This is a prompt/instruction change confined to three Lua files in
`examples/workflows/`. No runtime, engine, or core code changes are required.
The workflow graph, step ids, transitions, and output field contracts are
unchanged; only the natural-language instructions the investigator and reviewer
roles receive change, plus the exact ordered RCA section list.

Verification strategy (revised after plan review). Two independent layers, so no
TODO's proof depends on an artifact created by a later TODO:

- **Per-edit source assertions (TODO-01–03).** Each prompt edit is verified
  in-place by `grep` on the edited `.lua` source for the exact new wording, and
  by `git diff` on that same file to prove the declared `status`/`fields` output
  contract lines are unchanged. These checks are self-contained and independent
  of the focused test.
- **Compiled-contract test (TODO-04).** A single focused Lua loader test
  compiles the `bugfix` workflow and asserts the generated investigator role
  instructions plus the `investigate` and `review_rca` prompts carry the new
  requirement, and that both steps keep their exact status set and complete
  output-field schema. This test is created and run in TODO-04, after all three
  prompt edits exist; its passing run is not claimed as proof for any earlier
  TODO.

Current repository state to reconcile:

- The working tree already contains an in-progress edit of all three target
  files. That edit is functionally close to the goal but introduces a **Lua
  syntax error** in `examples/workflows/roles/investigator.lua`: the trailing
  `]]` of the `instructions` string is no longer followed by a comma before
  `agent = "default"`, so the table entry fails to parse
  (`'}' expected ... near 'agent'`). Any workflow load that requires the
  investigator role currently fails. The implementer must land the intended
  wording *and* restore valid Lua so the example compiles.
- No existing test asserts the RCA section list or the evidence requirement, so
  the change is currently unguarded. TODO-04 adds the focused test that locks
  the new contract in.

Affected files:

- `examples/workflows/roles/investigator.lua` — investigator role instructions.
- `examples/workflows/steps/investigate_bug.lua` — investigator step prompt and
  RCA section checklist.
- `examples/workflows/steps/review_rca.lua` — RCA reviewer step prompt.
- `crates/workflow/lua/src/loader.rs` — new focused test (tests only).

Scope boundaries:

- Do not change the `bugfix.lua` graph, transitions, or any output `fields`
  spec. The investigator still returns `documented` / `unclear` / `blocked` with
  fields `summary, user_feedback, work_dir, rca_doc, repro_test, files, command,
  failure`; the reviewer still returns `approved` / `changes_requested` with
  fields `feedback, user_feedback, work_dir, rca_doc, repro_test, commands,
  failures`.
- Do not alter the `feature` or `dev-loop` workflows; they have no RCA step.
- Do not weaken any existing preservation requirement (`user_feedback`,
  `work_dir`, `rca_doc`, `repro_test`, redaction guidance).

## Changes

### `examples/workflows/roles/investigator.lua`

- In the exact RCA section list, insert `Root cause evidence` between
  `Root cause` and `Reproduction steps`, so the list reads verbatim: `Bug
  behavior, Root cause, Root cause evidence, Reproduction steps, Regression
  test, Current failing result, and Fix constraints`.
- Add a sentence requiring the Root cause evidence section to prove the root
  cause with a step-by-step walkthrough of how the bug happens, preferring an
  example flow reconstructed from real log lines (each quoted line explained as
  the flow advances toward the failure) and falling back to specific source
  locations when logs are unavailable.
- Restore valid Lua: the `instructions = [[ ... ]]` value MUST be followed by a
  comma before `agent = "default"`. Confirm the file parses.

### `examples/workflows/steps/investigate_bug.lua`

- Add `- Root cause evidence` to the ordered RCA section checklist, positioned
  between `- Root cause` and `- Reproduction steps`.
- Add an instruction paragraph directing the investigator to make the Root cause
  evidence section a concrete, step-by-step walkthrough tracing the flow from
  trigger to observed failure. The paragraph MUST contain these exact literal
  phrases so they are individually greppable:
  - `step-by-step walkthrough` — the required walkthrough form.
  - `quote the relevant log lines` and `explain what the line shows and how it advances`
    — each quoted log line must be explained (preferred evidence form).
  - `When logs are unavailable, ground the walkthrough in specific source locations`
    — the source-location fallback.
  - `Do not assert the root cause without this traceable evidence` — the
    prohibition on unsupported root causes.
- Leave the redaction guidance, regression-test instruction, the
  `status = { "documented", "unclear", "blocked" }` set, and the
  `fields = { ... }` output schema unchanged.

### `examples/workflows/steps/review_rca.lua`

- Extend the reviewer prompt to require validation of the Root cause evidence
  section. The prompt MUST contain these exact literal phrases so they are
  individually greppable:
  - `Validate that the Root cause evidence section proves the stated root cause`
    — the validation requirement.
  - `step-by-step walkthrough` — the required evidence form.
  - `Return "changes_requested" when the root cause is asserted without this step-by-step evidence`
    — the explicit rejection sentence (distinct from the generic
    `changes_requested` status token in the output declaration).
- Leave the sensitive-data check, the `status = { "approved",
  "changes_requested" }` set, and the `fields = { ... }` preserved-field schema
  unchanged.

## Tests to be added/updated

- Add one focused test in `crates/workflow/lua/src/loader.rs`,
  `bugfix_investigation_requires_root_cause_evidence`, following the existing
  `load_example_compiled_workflow` / `run_step` patterns. It must, for the
  compiled `bugfix` workflow, assert:
  - **Investigator role:** `compiled.definition.roles["investigator"].instructions`
    contains the ordered substring `Bug behavior, Root cause, Root cause
    evidence, Reproduction steps` and the evidence wording (`step-by-step`,
    `example flow`, and the source-location fallback phrase).
  - **`investigate` step:** the generated agent prompt contains the
    `- Root cause evidence` bullet and the step-by-step / example-flow-from-log
    walkthrough language; the declared output status set equals exactly
    `["documented", "unclear", "blocked"]`; and the declared output fields equal
    exactly `summary, user_feedback, work_dir, rca_doc, repro_test, files,
    command, failure` (each with its HEAD type).
  - **`review_rca` step:** the generated reviewer prompt contains the
    evidence-validation requirement and the `changes_requested` condition; the
    declared output status set equals exactly `["approved",
    "changes_requested"]`; and the declared output fields equal exactly
    `feedback, user_feedback, work_dir, rca_doc, repro_test, commands, failures`
    (each with its HEAD type).
- Confirm the pre-existing
  `examples_workflows_agent_steps_preserve_and_render_user_feedback` test (which
  loads the `bugfix` workflow and exercises `investigate` / `review_rca`) passes
  again; it currently fails only because of the working-tree Lua syntax error.
  This test is a Lua-load/feedback-preservation guard only, not proof of the new
  evidence contract.

## How to verify

1. Direct source-wording assertions on the three edited prompts (TODO-01–03):
   per-clause `grep -F` for each exact required phrase, plus a zero-context
   `git diff --unified=0` filtered to changed (`+`/`-`) records to prove no
   changed record touches the `status`/`fields` declaration text. `--unified=0`
   emits no context lines and the `^[+-]` / `^[+-][+-]` filters keep only real
   added/removed records, so the check inverts cleanly: no output and exit
   status 1 means neither declaration changed (contract intact), while matching
   output and exit status 0 means at least one declaration was changed.
2. Focused compiled-contract test that observably runs one test:
   `cargo test -p cowboy-workflow-lua bugfix_investigation_requires_root_cause_evidence -- --nocapture`
   — output must include `running 1 test` and `test result: ok. 1 passed`.
3. Pre-existing bugfix step coverage no longer blocked by the syntax error:
   `cargo test -p cowboy-workflow-lua examples_workflows_agent_steps_preserve_and_render_user_feedback -- --nocapture`
   — output must include `running 1 test` and `test result: ok. 1 passed`.
4. Full crate suite plus lints on the changed crate:
   `cargo test -p cowboy-workflow-lua` and
   `cargo clippy -p cowboy-workflow-lua -- -D warnings`.

## TODO

- [x] TODO-01: Add the "Root cause evidence" section requirement to the investigator role and restore valid Lua.
  - Procedure:
    1. Edit `examples/workflows/roles/investigator.lua`: insert `Root cause evidence` into the exact section list between `Root cause` and `Reproduction steps`; add the sentence requiring a step-by-step walkthrough that prefers an example flow reconstructed from real log lines and falls back to specific source locations when logs are unavailable; and ensure the `instructions = [[ ... ]]` value is followed by a comma before `agent = "default"`.
    2. Assert the ordered section list: `grep -c "Bug behavior, Root cause, Root cause evidence, Reproduction steps, Regression test, Current failing result, and Fix constraints" examples/workflows/roles/investigator.lua`.
    3. Assert the step-by-step requirement and log-flow preference with source-location fallback: `grep -n "step-by-step" examples/workflows/roles/investigator.lua`, `grep -n "example flow" examples/workflows/roles/investigator.lua`, and `grep -ni "source location" examples/workflows/roles/investigator.lua`.
    4. Prove the file parses (comma restored) by compiling the workflow: `cargo test -p cowboy-workflow-lua examples_workflows_agent_steps_preserve_and_render_user_feedback -- --nocapture`.
  - Expected result: step 2 prints `1` (exactly one occurrence of the ordered list line); step 3 each print at least one matching line (step-by-step requirement, `example flow` preference, and a source-location fallback clause all present); step 4 output includes `running 1 test` and `test result: ok. 1 passed; 0 failed` with no `'}' expected ... near 'agent'` Lua syntax error.
  - Observed result: added the missing comma after the `instructions = [[ ... ]]` value (the only defect; the wording was already present in the working tree). Section-list grep printed `1`; `step-by-step` and `example flow` each matched line 7; `source location` matched (case-insensitive) line 7; `cargo test -p cowboy-workflow-lua examples_workflows_agent_steps_preserve_and_render_user_feedback -- --nocapture` printed `running 1 test` and `test result: ok. 1 passed; 0 failed; 0 ignored` with no Lua syntax error.

- [x] TODO-02: Add the "Root cause evidence" step-by-step instruction to the investigate_bug step prompt.
  - Procedure:
    1. Edit `examples/workflows/steps/investigate_bug.lua`: add `- Root cause evidence` to the RCA section checklist between `- Root cause` and `- Reproduction steps`, and add the walkthrough paragraph containing the four exact literal phrases listed in Changes.
    2. Assert the bullet: `grep -c -- "- Root cause evidence" examples/workflows/steps/investigate_bug.lua`.
    3. Assert each required clause literally (fixed-string, so no regex/substring ambiguity): `grep -Fc "step-by-step walkthrough" examples/workflows/steps/investigate_bug.lua`; `grep -Fc "quote the relevant log lines" examples/workflows/steps/investigate_bug.lua`; `grep -Fc "explain what the line shows and how it advances" examples/workflows/steps/investigate_bug.lua`; `grep -Fc "When logs are unavailable, ground the walkthrough in specific source locations" examples/workflows/steps/investigate_bug.lua`; `grep -Fc "Do not assert the root cause without this traceable evidence" examples/workflows/steps/investigate_bug.lua`.
    4. Prove the output contract is unchanged with a zero-context diff filtered to changed records only: `git diff --unified=0 -- examples/workflows/steps/investigate_bug.lua | grep -E '^[+-]' | grep -v '^[+-][+-]' | grep -E 'status = \{|fields = \{'`.
  - Expected result: step 2 prints `1`; every command in step 3 prints a count `>= 1` (all four clause phrases plus the walkthrough phrase present); step 4 prints no output and exits non-zero (no added or removed line touches a `status = {` or `fields = {` declaration — the changed records are confined to the prompt string, so both declarations are unchanged from HEAD).
  - Observed result: bullet grep printed `1`; each of the five fixed-string clause greps printed `1`; the zero-context contract diff produced no output and exited `1`, confirming the `status = {` / `fields = {` declarations are unchanged. (Working-tree already carried this edit; verified in place.)

- [x] TODO-03: Require the RCA reviewer to validate the root cause evidence walkthrough.
  - Procedure:
    1. Edit `examples/workflows/steps/review_rca.lua`: extend the reviewer prompt with the three exact literal phrases listed in Changes (validation requirement, `step-by-step walkthrough`, and the explicit rejection sentence), keeping the sensitive-data check and `work_dir`/`rca_doc`/`repro_test` preservation instruction intact.
    2. Assert each required clause literally: `grep -Fc "Validate that the Root cause evidence section proves the stated root cause" examples/workflows/steps/review_rca.lua`; `grep -Fc "step-by-step walkthrough" examples/workflows/steps/review_rca.lua`; `grep -Fc 'Return "changes_requested" when the root cause is asserted without this step-by-step evidence' examples/workflows/steps/review_rca.lua`.
    3. Prove the output contract is unchanged with a zero-context diff filtered to changed records only: `git diff --unified=0 -- examples/workflows/steps/review_rca.lua | grep -E '^[+-]' | grep -v '^[+-][+-]' | grep -E 'status = \{|fields = \{'`.
  - Expected result: every command in step 2 prints a count `>= 1` (the validation requirement, the `step-by-step walkthrough` form, and the explicit rejection sentence are all present — the rejection is matched by its full sentence, not the bare `changes_requested` token); step 3 prints no output and exits non-zero (no added or removed line touches a `status = {` or `fields = {` declaration, so the reviewer status set and preserved-field schema are unchanged from HEAD).
  - Observed result: each of the three fixed-string clause greps printed `1`; the zero-context contract diff produced no output and exited `1`, confirming the reviewer `status = {` / `fields = {` declarations are unchanged. (Working-tree already carried this edit; verified in place.)

- [x] TODO-04: Add a focused Lua loader test locking in the root-cause-evidence contract.
  - Procedure:
    1. Add `bugfix_investigation_requires_root_cause_evidence` to `crates/workflow/lua/src/loader.rs` per "Tests to be added/updated": assert the investigator instructions ordered list + evidence wording; assert the `investigate` prompt bullet + walkthrough language and its exact unchanged status set `["documented", "unclear", "blocked"]` and complete output-field schema (`summary, user_feedback, work_dir, rca_doc, repro_test, files, command, failure`); assert the `review_rca` prompt evidence-validation + `changes_requested` condition and its exact unchanged status set `["approved", "changes_requested"]` and complete preserved-field schema (`feedback, user_feedback, work_dir, rca_doc, repro_test, commands, failures`).
    2. Run `cargo test -p cowboy-workflow-lua bugfix_investigation_requires_root_cause_evidence -- --nocapture`.
  - Expected result: the run output includes `running 1 test` and `test result: ok. 1 passed; 0 failed; 0 ignored` (proving exactly one matching test executed, not a zero-match filter). Matching the assertions specified in step 1, the test fails if the investigator instructions or the `investigate` prompt drops the `Root cause evidence` section or the step-by-step / example-flow-from-log walkthrough language, if the `review_rca` prompt drops the evidence-validation requirement or the `changes_requested` rejection condition, or if either step's exact status set or complete output-field schema changes. (The `review_rca` assertion covers the evidence-validation requirement and rejection condition; the example-log-flow/source-location wording is asserted for the investigator role and `investigate` prompt, where TODO-01/TODO-02 introduce it.)
  - Observed result: added `bugfix_investigation_requires_root_cause_evidence` to `crates/workflow/lua/src/loader.rs`; `cargo test -p cowboy-workflow-lua bugfix_investigation_requires_root_cause_evidence -- --nocapture` printed `running 1 test` and `test result: ok. 1 passed; 0 failed; 0 ignored`.

- [x] TODO-05: Run the full changed-crate checks and resolve warnings.
  - Procedure: Run `cargo test -p cowboy-workflow-lua` and `cargo clippy -p cowboy-workflow-lua -- -D warnings`.
  - Expected result: the test summary reports `0 failed` and includes `bugfix_investigation_requires_root_cause_evidence` among the passed tests; Clippy exits 0 with no warnings for the crate.
  - Observed result: `cargo test -p cowboy-workflow-lua` reported `test result: ok. 57 passed; 0 failed; 0 ignored` with `bugfix_investigation_requires_root_cause_evidence ... ok` among them; `cargo clippy -p cowboy-workflow-lua -- -D warnings` finished with no warnings (exit 0).
