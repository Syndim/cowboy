# Plan
Both planner artifacts are subject to the same sensitive-data policy. They must redact, generalize, or omit credentials, secrets, personal data, private paths, and proprietary content. When the user's Goal or Validation method contains such values, the artifacts must preserve the validation procedure semantically with safe placeholders or environment-variable references rather than copying the sensitive values verbatim.

Extend the starter workflows at their existing Lua step seam. The `dev-loop` planner will continue to treat the user's exact Goal and Validation method as fixed inputs, but it will produce two durable planning artifacts: the existing implementation plan and a sibling validation guide at `docs/plans/<snake_case_summary>_validation.md`. The guide will turn the exact user-provided validation method into an executable decision procedure without weakening or replacing it: prerequisites, ordered commands or manual checks, evidence to capture, explicit success criteria for leaving the development loop, and failure criteria that send the run back through revision. Replanning must update the existing `plan_doc` and `validation_doc` paths rather than creating duplicate artifacts.

Add blocker review as a shared workflow module rather than duplicating policy in each caller. Every `blocked` transition in `feature`, `bugfix`, and `dev-loop` will first capture the originating step and blocker statement, then invoke a dedicated blocker-reviewer role. The reviewer will inspect the blocker plus any available `plan_doc`, `rca_doc`, and `validation_doc`, and return one of two decisions: `recoverable`, with concrete agent-side recovery instructions, or `user_required`, with evidence that the prerequisite cannot be resolved using available context and tools and the exact user input or action required. Recoverable blockers will be routed back to the originating workflow step with the review instructions as feedback. Only `user_required` blockers will enter the existing ask-user flow; the prompt shown to the user will include the reviewer's reasoning and precise request.

Keep this policy in the example workflow modules, not in the core runtime. `blocked` is a workflow-declared output status, and the built-in `default` workflow does not declare it; changing core execution would introduce implicit routing outside the Lua graph. The shared capture, review, confirmation, and routing steps provide one explicit implementation used by every current blocked-capable workflow.

# Changes

- Add `examples/workflows/roles/blocker_reviewer.lua` with its own `blocker-reviewer` role id, backed by the existing `reviewer` agent preset. Its instructions will require repository-grounded blocker analysis, inspection of available planning/RCA/validation artifacts, safe self-service recovery assessment, and a precise explanation when human intervention is unavoidable.
- Add a shared blocker-capture step under `examples/workflows/steps/` that copies the blocked action's fields and deterministically records the original prose in `blocker_statement` plus `blocked_from_step` and `blocked_from_status` before any review or user prompt.
- Add `examples/workflows/steps/review_blocker.lua` as the agent-action module for blocker review.
  - Render the request, blocker statement, originating step, cumulative user feedback, and artifact paths through the existing context helpers.
  - Declare only `recoverable` and `user_required` outcomes.
  - Require every result to populate `blocker_reason` with the reviewer's evidence-based recoverability analysis.
  - Define `blocker_resolution` by status: agent-executable recovery instructions for `recoverable`, or the exact external input/action required for `user_required`.
  - Preserve `blocker_statement`, `blocked_from_step`, `blocked_from_status`, `blocker_reason`, `blocker_resolution`, `goal`, `validation`, `work_dir`, `plan_doc`, `validation_doc`, `rca_doc`, `repro_test`, and `files` for downstream routing.
- Update `examples/workflows/steps/blocked.lua` so it remains the user-resolution step but is reached only after `user_required`; construct the ask-user message from `blocker_statement`, `blocker_reason`, and `blocker_resolution`, with `blocker_resolution` supplying the exact requested action, while preserving the captured origin and artifact fields.
- Update `examples/workflows/steps/triage_blocked.lua` to consume `blocker_resolution` for reviewer-directed recovery or the explicit user answer for human-directed recovery, preserve every named blocker and artifact field, and route deterministically back to `blocked_from_step` when no explicit user redirection applies. Do not infer routing or recovery instructions from agent body prose or generic `feedback`. Extend each workflow's transitions for every step that can currently return `blocked`, including test and commit retries, while retaining user-directed plan/implementation/revision routing.
- Wire the blocker-reviewer role and shared blocker flow into `examples/workflows/workflows/feature.lua`, `bugfix.lua`, and `dev-loop.lua`. Replace every direct `blocked`-to-user transition with capture then blocker review; route `recoverable` through triage and `user_required` to the existing ask-user/answer path. Do not change non-blocked success, failure, review, or confirmation routes.
- Extend `examples/workflows/steps/plan.lua` with a dev-loop-only validation-guide option.
  - Require creation of both `plan_doc` and `validation_doc`, with the validation guide stored beside the ordinary plan as `docs/plans/<snake_case_summary>_validation.md`.
  - Require the guide to contain the exact Goal and user Validation method, prerequisites, ordered validation steps, evidence requirements, exit criteria, and continue/revise criteria.
  - Apply the plan-document sensitive-data rule equally to `plan_doc` and `validation_doc`; neither artifact may copy secrets, credentials, personal data, private paths, or proprietary content.
  - When redaction is required, preserve the validation procedure semantically with explicit placeholders or environment-variable references so the guide remains executable without retaining the sensitive value.
  - Require both paths in `files`; on replanning, preserve and update existing paths from prior feedback.
  - Keep feature and bug-fix planner behavior unchanged when the option is absent.
- Update `examples/workflows/steps/review_plan.lua` for the dev-loop option so plan review reads and approves both artifacts, rejects sensitive data in either artifact, verifies that any redaction preserves the user's validation procedure semantically, and rejects missing, ambiguous, non-executable, or weakened exit criteria. Ensure plan confirmation presents the reviewed planning artifacts and preserves `validation_doc`.
- Update `examples/workflows/steps/validate_goal.lua` so the dev-loop validator reads `validation_doc`, executes its ordered procedure including the exact user Validation method, records the required evidence, returns `achieved` only when every exit criterion passes, and otherwise returns `not_achieved` or a reviewed `blocked` result as appropriate.
- Extend `examples/workflows/utils/context.lua` to render `Validation doc: ...` alongside the existing plan/RCA/repro context. Propagate the optional `validation_doc` field through the shared clarification, confirmation, implementation, test, validation, implementation-review, feedback-review, revision, blocked-triage, and commit steps so all three workflows retain artifact access across normal paths and detours.
- Update the README's starter-workflow description after the behavior is working to document the dev-loop's two planning artifacts and the blocker-review-before-user policy.

# Tests to be added/updated

- Extend `crates/workflow/lua/src/loader.rs` workflow-definition tests to assert that `feature`, `bugfix`, and `dev-loop` all declare the blocker-reviewer role with the `reviewer` agent preset and route every blocked-capable step through capture and blocker review before the ask-user step.
- Add graph assertions for both blocker-review outcomes: `recoverable` reaches triage and can retry each valid originating step; `user_required` reaches the user-resolution step; answered user prompts return through triage without losing the origin.
- Add focused step-execution tests proving blocker capture records `blocker_statement`, `blocked_from_step`, and `blocked_from_status`; blocker-review prompts include available `plan_doc`, `rca_doc`, and `validation_doc`; and the output contract requires status-independent `blocker_reason` plus status-dependent `blocker_resolution`.
- Add focused routing and ask-user tests proving recoverable blockers do not prompt the user, triage consumes `blocker_resolution` without parsing body prose, and nonrecoverable prompts are constructed from `blocker_statement`, `blocker_reason`, and the exact external action in `blocker_resolution`.
- Extend artifact-context tests so `validation_doc` is rendered and preserved through planner review, plan confirmation, implement, test, validate, implementation review, revision, commit, clarification, blocker review, user resolution, and triage detours.
- Extend dev-loop planner tests to assert the prompt and output schema require both stable document paths, both entries in `files`, exact preservation of prior paths during replanning, the validation guide's ordered evidence/exit/continue contract, and the same redaction/generalization policy for both artifacts. Use only synthetic sensitive-looking Goal/Validation fixtures and assert safe placeholders or environment-variable references are required; never place real secrets or personal data in fixtures. Keep feature and bug-fix planner prompts unchanged.
- Extend dev-loop plan-review and validator tests to assert they inspect `validation_doc`, require rejection when either planning artifact contains synthetic sensitive-looking data, preserve the validation procedure semantically after redaction, reject weakened or incomplete guidance, and treat all exit criteria as mandatory before returning `achieved`.

# How to verify

- Run `cargo test -p cowboy-workflow-lua loader::tests::dev_loop` for the dev-loop planning and validation-guide contract.
- Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows` for shared artifact propagation, blocker-review prompts, and all-workflow graph routing.
- Run `cargo test -p cowboy-workflow-lua` to compile and execute the complete Lua loader/runtime test surface against all starter workflows.
- Run `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings`.
- Inspect the generated workflow definitions in the focused tests to confirm no blocked-capable step has a direct path to `ask_user`, recoverable reviews retry the captured origin with actionable feedback, and only `user_required` reaches the user prompt.

# TODO

- [x] Add the dedicated blocker-reviewer role using the reviewer agent preset.
- [x] Add the shared blocker capture step with deterministic `blocker_statement`, `blocked_from_step`, and `blocked_from_status` fields.
- [x] Add the shared blocker-review agent step with required `blocker_reason`, status-dependent `blocker_resolution`, and `recoverable`/`user_required` outcomes.
- [x] Gate the existing blocked ask-user step behind `user_required` and build its prompt from the named blocker fields.
- [x] Extend blocked triage to consume named reviewer fields or user answers without parsing prose and retry every valid originating step.
- [x] Rewire feature, bug-fix, and dev-loop blocked transitions through capture, review, recovery, and user-resolution paths.
- [x] Add dev-loop planner generation and stable path preservation for the validation guide.
- [x] Apply one sensitive-data redaction/generalization contract to `plan_doc` and `validation_doc`, preserving executable validation semantics with safe placeholders.
- [x] Add validation-guide review and plan-confirmation behavior that rejects sensitive data in either artifact without changing feature or bug-fix planning.
- [x] Make dev-loop validation execute the guide and enforce its evidence-based exit criteria.
- [x] Propagate and render `validation_doc` through all shared workflow steps and detours.
- [x] Add workflow graph, named blocker-field, user-prompt, artifact-propagation, dual-artifact privacy, planner, plan-review, and validator regression tests using only synthetic sensitive-looking fixtures.
- [x] Run focused dev-loop and shared example-workflow tests and fix all failures.
- [x] Run the complete workflow-Lua test suite and Clippy with warnings denied.
- [x] Update the starter-workflow README description after behavioral verification passes.
