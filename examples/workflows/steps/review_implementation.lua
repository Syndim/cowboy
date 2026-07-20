local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review", { role = roles.reviewer })
  local evidence_heading = opts.evidence_heading or "Test result:"
  local review_subject = opts.review_subject or "implementation and test results"
  local validation_guidance = ""
  if opts.require_user_validation then
    validation_guidance = [[

The validator result above must preserve the implementation and tester evidence and show that the validation guide's complete ordered procedure, including the exact user-provided Validation method, was executed. During Pass 1, assess every required `VAL-NN` proof and validator submission without executing any reviewer command or manual procedure. Execution remains forbidden until the complete global assessment set passes. The explicit Pass 2 section below defines the only permitted criterion reproduction. Do not approve unless every exit criterion demonstrated the Goal; substitute checks and reviewer inference do not count. Preserve the `Goal`, `Validation`, and `Validation doc` values exactly in output fields.]]
  end
  review.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review the ]] .. review_subject .. [[ for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, evidence_heading) .. validation_guidance .. context.preserve_user_feedback_guidance() .. context.review_user_feedback_guidance() .. context.preserve_evidence_guidance() .. context.evidence_record_guidance() .. [[

When a `Validation doc: ...` path is present, inspect that guide and preserve the path exactly in output fields. Inspect the working tree and plan document at the `Plan doc: ...` path. Require exactly one upstream evidence record per source and subject; reject duplicate records rather than selecting, merging, or reproducing them.

Review is a globally gated two-pass process. Read the approved plan and complete Pass 1 for every required `TODO-NN` in plan order—whether checked or unchecked—followed by every required `VAL-NN` in validation-guide order, before running any reviewer command or manual procedure. An unchecked required TODO must remain visible in the assessment set and makes the submission invalid; it must never disappear from the gate. Emit exactly one `reviewer_assessments` record per required subject:
- `subject_kind`, `subject_id`, and exact `subject`
- `source: reviewer`
- `completion_state`: `checked`, `unchecked`, or `not_applicable` (`validation_criterion` uses `not_applicable` only when the guide explicitly permits it)
- `proof_verdict`: `sound` or `unsound`
- six required, nonempty, subject-specific rationale strings: `relevance`, `sufficiency`, `safety_and_executability`, `currentness`, `falsifiability`, and `non_circularity`
- `submission_verdict`: `valid` or `invalid`
- `submission_issues`: an ordered array

Assess proof soundness independently of whether submitted evidence claims success. The procedure must prove the exact subject, cover its material acceptance boundary, be safe and executable with available prerequisites, match the current plan/code/evidence, fail on a plausible defect, and avoid circularly trusting an oracle produced by the code under test. Do not improvise substitute checks. Separately assess whether the upstream implementer/tester/validator records faithfully represent the assessed procedure, have exactly-one cardinality, exact subject association, complete/matching procedures and expected results, current observations, valid comparisons, and fully mapped commands.

For each required TODO, compare `completion_state` with the plan checkbox and the submitted evidence. `completion_state: unchecked` always requires `submission_verdict: invalid`, a `missing_record` or `stale_record` issue against the actual deficient implementer/tester field, globally empty reviewer command/evidence arrays, and `changes_requested` unless an unsound proof requires `replan_requested`. A checked TODO without exactly one matching implementer and tester record is likewise invalid.

Each submission issue is `{ source, code, field, message }`. `source` is the actual defective upstream source; TODO issues may name only `implementer` or `tester`, while validation-criterion issues may name only `validator`. `code` must be one of: `missing_record`, `duplicate_record`, `subject_mismatch`, `procedure_mismatch`, `expected_result_mismatch`, `observed_result_mismatch`, `invalid_comparison`, `stale_record`, `malformed_field`, or `unmapped_command`. `field` is the exact defective field/path. `message` is nonempty and actionable. Reject unknown codes, missing fields, invalid source/subject combinations, or empty messages. `submission_verdict: valid` requires `submission_issues: []`; `submission_verdict: invalid` requires at least one issue. Keep issue messages out of raw `user_feedback` and general reviewer `feedback`.

Apply global precedence after every assessment exists. If any `proof_verdict` is `unsound`, return `replan_requested`, even if another submission is invalid. If all proofs are sound but any `submission_verdict` is `invalid`, return `changes_requested`. Both early-return statuses must contain the complete assessment array and globally empty `reviewer_commands: []` and `reviewer_evidence: []`; execute no reviewer command or manual step.

Use "replan_requested" when any proof procedure makes the plan or validation guide incomplete, unsafe, incorrectly scoped, unverifiable, or non-reproducible. Otherwise use "changes_requested" when submitted evidence or implementation defects can be fixed within the approved plan. This routing explanation does not alter the global precedence above.

Only when every assessment is `sound` and `valid` may Pass 2 begin. Then independently reproduce every subject's sole complete procedure in the same order. Emit exactly one reviewer evidence record per subject and mapped reviewer command records. Reviewer TODO evidence compares against exactly the matching implementer and tester observations; reviewer validation evidence compares against exactly the matching validator observation. Approval requires every rerun to match. Missing, duplicate, stale, reordered, non-reproducible, contradictory, falsely relabeled, unmapped, not-run, or mismatched evidence cannot be approved.

For bug fixes, also inspect the bug-fix work folder at `Work dir: ...`, the RCA document at `RCA doc: ...`, and the investigator-added regression test identified by `Repro test: ...`; verify that test still validates the original issue and passes because product code was fixed. Preserve all incoming structured arrays with semantic deep equality and preserve the `Goal`, `Validation`, `Work dir`, `Plan doc`, `Validation doc`, `RCA doc`, and `Repro test` values exactly. Return `approved` only after both passes succeed; otherwise use the global routing rules above.]],
      output = {
        status = { "approved", "changes_requested", "replan_requested" },
        fields = { feedback = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", implementation_commands = "array", implementation_evidence = "array", tester_commands = "array", tester_evidence = "array", validator_commands = "array", validator_evidence = "array", reviewer_commands = "array", reviewer_evidence = "array", reviewer_assessments = "array" },
      },
    }
  end
  return review
end
