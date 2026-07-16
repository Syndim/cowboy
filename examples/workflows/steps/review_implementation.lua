local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review", { role = roles.reviewer })
  local evidence_heading = opts.evidence_heading or "Test result:"
  local review_subject = opts.review_subject or "implementation and test results"
  local validation_guidance = ""
  if opts.require_user_validation then
    validation_guidance = [[

The validator result above must show that the validation guide's complete ordered procedure, including the exact user-provided Validation method, was executed and that every exit criterion demonstrated the Goal. Do not approve based on substitute checks or reviewer inference. Preserve the `Goal`, `Validation`, and `Validation doc` values exactly in output fields.]]
  end
  review.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review the ]] .. review_subject .. [[ for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, evidence_heading) .. validation_guidance .. context.preserve_user_feedback_guidance() .. context.review_user_feedback_guidance() .. [[

When a `Validation doc: ...` path is present, inspect that guide and preserve the path exactly in output fields.

Inspect the working tree and plan document at the `Plan doc: ...` path. For bug fixes, also inspect the bug-fix work folder at `Work dir: ...`, the RCA document at `RCA doc: ...`, and the investigator-added regression test identified by `Repro test: ...`; verify that test still validates the original issue and passed in the test step. Verify every checked TODO item is actually completed, require unfinished work items to remain unchecked, preserve the `Work dir`, `Plan doc`, `RCA doc`, and `Repro test` values exactly in output fields when present, and return "approved" only if the change is correct, scoped, sufficiently tested, and all required TODO items are complete. Return "changes_requested" with actionable feedback when the implementation has defects that can be fixed within the approved plan. Return "replan_requested" with actionable feedback for the planner when the implementation follows the approved plan but that plan is incomplete, unsafe, incorrectly scoped, unverifiable, or otherwise not solid.]],
      output = {
        status = { "approved", "changes_requested", "replan_requested" },
        fields = { feedback = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string" },
      },
    }
  end
  return review
end
