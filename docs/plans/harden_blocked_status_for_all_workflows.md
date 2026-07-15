# Plan

Harden the shared agent deliverable contract so `blocked` means a last-resort dependency on human help, not a substitute for investigation or problem solving. Every agent action is rendered through `crates/workflow/agent/src/prompt.rs::build_agent_prompt`, and retry prompts reuse the same output instructions through `build_retry_nudge`; applying the rule there covers every current and future workflow whose `OutputSpec` allows the exact `blocked` status without duplicating policy across Lua step files.

The policy will require the agent to exhaust reasonable, safe, in-scope actions available through the repository, supplied context, and tools before choosing `blocked`. A crash, failing command or test, unfamiliar code, or unsuccessful first approach remains work to diagnose and resolve. `blocked` is appropriate only when the remaining prerequisite is unavailable to the agent and requires a concrete human action, decision, credential, permission, or external resource. A blocked response must summarize what was tried, the evidence that ruled out self-service recovery, and the exact help needed to continue.

Keep the existing workflow graphs, status sets, retry behavior, and output field schemas unchanged. The feature, bug-fix, and dev-loop examples already route legitimate blocked results to the shared user-input and triage flow; the defect is the weak status-selection guidance before that route is taken. The built-in default workflow does not expose a `blocked` status and therefore should not receive irrelevant blocking instructions.

# Changes

- Update `crates/workflow/agent/src/prompt.rs` to append a blocked-status policy when, and only when, an action's declared output statuses include the exact `blocked` value.
  - Put the rule in the shared output-instruction rendering path used by both initial prompts and frontmatter retry nudges.
  - Define the required pre-blocking behavior: inspect available context, diagnose failures, try reasonable safe fixes, and try viable in-scope alternatives.
  - Explicitly state that crashes, failing commands/tests, unfamiliar code, and failed first attempts do not by themselves justify `blocked`.
  - Reserve `blocked` for prerequisites that cannot be obtained or resolved with the available tools and context and that require human intervention.
  - Require the final blocked explanation to identify attempts/evidence and the precise human action needed.
- Do not edit the example Lua workflow routing or add new status fields. `examples/workflows/steps/{investigate_bug,implement,revise,test,validate_goal,commit}.lua` already declare `blocked`, and central prompt rendering will apply the same contract to all of them without creating per-step wording drift.

# Tests to be added/updated

- Extend the focused prompt tests in `crates/workflow/agent/src/prompt.rs` with an action whose output statuses include `blocked`; assert that the assembled agent prompt contains the last-resort threshold, the requirement to try reasonable self-service recovery, examples that are not blockers, and the required human-help explanation.
- Add a negative assertion for an action without `blocked` to prove unrelated workflows, including the built-in default workflow's `success`/`failed`/`needs_fix` contract, do not receive blocked-only guidance.
- Extend retry-nudge coverage with a blocked-capable action and assert the same policy is present after a frontmatter parse retry, preventing retries from weakening the status contract.

# How to verify

- Run `cargo test -p cowboy-workflow-agent prompt::tests`.
- Run `cargo clippy -p cowboy-workflow-agent --all-targets -- -D warnings`.
- Inspect the prompt test fixtures to confirm both boundaries: `blocked`-capable actions receive the policy on initial and retry prompts, while actions without `blocked` do not.

# TODO

- [x] Add conditional blocked-status guidance to the shared agent output-instruction renderer.
- [x] Encode the exhaustion threshold, non-blocker examples, and required human-help evidence in the guidance.
- [x] Add positive and negative initial-prompt tests for the blocked-status policy.
- [x] Add retry-nudge coverage for blocked-capable actions.
- [x] Run the focused workflow-agent prompt tests.
- [x] Run Clippy for all workflow-agent targets with warnings denied.

## Review follow-up TODO

- [x] Add an exact-status near-miss fixture with `needs_fix` and `unblocked`.
- [x] Assert blocked prompts require an account of attempted recovery.
- [x] Assert retry nudges contain the complete shared blocked-status policy.
- [x] Re-run the focused workflow-agent prompt tests.
- [x] Re-run Clippy for all workflow-agent targets with warnings denied.
