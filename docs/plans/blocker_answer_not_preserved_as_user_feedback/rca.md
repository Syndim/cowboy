## Bug behavior

In reported run `run-cda5b279-58cd-4714-89bb-039d5491ab56`, the workflow asked the user for the next action after a blocker. The user answered with raw guidance to skip a specific TODO. The next `revise` prompt included that answer only as the previous step `Feedback:` value, not as an entry in `User feedback history:`.

After the implementer and tester preserved `user_feedback`, the implementation reviewer treated the skip instruction as not present in cumulative user feedback and requested the same TODO again. A later blocker-reviewer prompt also had a `User feedback history:` section that omitted the user's blocker answer. Prior unrelated feedback entries from the run are omitted here to avoid carrying user-specific content into this RCA.

## Root cause

`examples/workflows/steps/blocked.lua` handles an answered `ask_user` step by copying existing fields and storing the raw answer in `fields.blocked_response`, but it does not append that raw user answer to `fields.user_feedback`.

`examples/workflows/steps/triage_blocked.lua` then chooses the user answer as `fields.feedback` and copies `user_feedback` unchanged. The `revise` step can see `feedback`, but its declared output fields do not include `feedback`; it only preserves `user_feedback`. After `revise` and later steps run, the raw blocker answer is no longer available to downstream reviewer prompts as cumulative user direction.

The result is that reviewer prompts can see agent-produced summaries or evidence mentioning the answer, but cannot distinguish that text from agent/reviewer-generated feedback because the raw answer is absent from `user_feedback`.

## Reproduction steps

1. Inspect the reported event log for `run-cda5b279-58cd-4714-89bb-039d5491ab56`.
2. Confirm the blocker answer was recorded as the body of the answered `blocked` ask-user step.
3. Confirm the following `triage_blocked` step emitted `feedback` with the answer while preserving the old `user_feedback` array unchanged.
4. Confirm a later reviewer prompt's `User feedback history:` omitted the blocker answer, while the reviewer stated the skip instruction was not present in cumulative user feedback.
5. Run the focused regression test below.

## Regression test

Test file path: `crates/workflow/lua/src/loader.rs`

Test name: `loader::tests::bugfix_blocker_answer_becomes_cumulative_user_feedback`

Command: `cargo test -p cowboy-workflow-lua bugfix_blocker_answer_becomes_cumulative_user_feedback -- --nocapture`

Expected failure before the fix: the test asserts that the raw blocker answer is appended to `user_feedback` after `blocked_answer` and `triage_blocked`. Current product workflow behavior leaves `user_feedback` as only the pre-existing array.

## Current failing result

```text
running 1 test
thread 'loader::tests::bugfix_blocker_answer_becomes_cumulative_user_feedback' panicked at crates/workflow/lua/src/loader.rs:1535:9:
assertion `left == right` failed: blocker answers must travel as raw cumulative user feedback so downstream reviewers can distinguish user waivers from agent or reviewer feedback
  left: Array [String("keep the original raw request")]
 right: Array [String("keep the original raw request"), String("skip TODO-13")]
test loader::tests::bugfix_blocker_answer_becomes_cumulative_user_feedback ... FAILED

failures:

failures:
    loader::tests::bugfix_blocker_answer_becomes_cumulative_user_feedback

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 54 filtered out; finished in 0.06s

error: test failed, to rerun pass `-p cowboy-workflow-lua --lib`
```

## Fix constraints

- Edit product code only after this investigation phase.
- Preserve existing `user_feedback` entries exactly and in order.
- Append the blocker ask-user answer as raw user direction when present; do not prefix it with a stage label and do not add agent, tester, reviewer, or blocker-reviewer generated text.
- Keep `fields.feedback` available for the immediate retry step, but do not rely on it as the durable channel for user direction.
- Preserve evidence arrays, path fields, blocker metadata, and routing behavior through blocker detours.
- The focused regression test should pass after the fix without weakening the existing evidence-preservation tests.
