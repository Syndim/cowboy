## Bug behavior

The result-feedback reviewer prompt displays literal artifact references such as ``Plan doc: ...`` and ``Work dir: ...`` even when the workflow has concrete document paths. The displayed text is the prompt sent to the agent, not a TUI-only abbreviation.

The concrete values are not lost: the same prompt contains the actual `work_dir`, `plan_doc`, `rca_doc`, and `repro_test` values earlier in its previous-step context. The defect is that the prompt also sends a later, contradictory-looking instruction containing literal placeholders.

## Root cause

`examples/workflows/steps/review_result_feedback.lua` appends a static instruction containing ``Plan doc: ...``, ``Work dir: ...``, ``RCA doc: ...``, and ``Repro test: ...``. `examples/workflows/utils/context.lua` correctly renders the concrete fields before that instruction.

The placeholders are not introduced by presentation code. `build_agent_prompt` includes the Lua action prompt verbatim, and the workflow agent executor clones that built prompt both into the visible prompt event and into `Client::prompt`. Therefore the TUI faithfully displays the same literal placeholders that the backend agent receives.

## Reproduction steps

1. Load the example `bugfix` workflow.
2. Execute `review_result_feedback` with a `confirm_result_answer` previous record containing concrete `work_dir`, `plan_doc`, `rca_doc`, and `repro_test` fields.
3. Inspect the resulting agent action prompt.
4. Observe that it contains both the concrete artifact references and the literal placeholder references from the static instruction.

## Regression test

- Test file: `crates/workflow/lua/src/loader.rs`
- Test name: `loader::tests::examples_workflows_review_result_feedback_prompt_uses_concrete_document_paths`
- Command: `cargo test -p cowboy-workflow-lua examples_workflows_review_result_feedback_prompt_uses_concrete_document_paths -- --nocapture`
- Expected failure before the fix: the prompt contains the concrete artifact paths but the assertion rejects the additional literal ``Work dir: ...`` placeholder (and, after that first assertion is resolved, the other document-path placeholders).

## Current failing result

The command exits with status 101. One test runs and fails:

```text
result-feedback prompt should not send placeholder artifact reference "`Work dir: ...`" when concrete paths are available

test result: FAILED. 0 passed; 1 failed; 0 ignored; 45 filtered out
```

The captured prompt in the failure includes concrete `Work dir`, `Plan doc`, `RCA doc`, and `Repro test` values followed later by the static sentence containing the corresponding `...` placeholders. This confirms the handoff values are present and the defect is in prompt construction, not display truncation.

## Fix constraints

- Preserve the concrete artifact values already rendered from the previous step.
- Preserve `user_feedback` exactly; do not append agent- or reviewer-generated feedback.
- Remove or replace the literal placeholder instruction at its source in `examples/workflows/steps/review_result_feedback.lua`; do not special-case TUI rendering.
- Keep both feature and bug-fix result-feedback routing and output-field contracts unchanged.
- Do not modify the investigator-added regression test while implementing the product fix.
