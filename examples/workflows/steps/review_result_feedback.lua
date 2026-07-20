local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review_result_feedback", { role = roles.reviewer })
  review.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review the user's post-implementation feedback for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "User result feedback:") .. context.preserve_user_feedback_guidance() .. context.review_user_feedback_guidance() .. context.preserve_evidence_guidance() .. [[

Inspect the user's feedback, the implementation, the concrete plan document, every source-labeled evidence array, and `reviewer_assessments` supplied in the previous-step context. Keep raw `user_feedback`, reviewer assessment rationale/issues, and reviewer rerun evidence distinct; never append evidence, issues, or reviewer-generated feedback to `user_feedback`. For bug fixes, also preserve the concrete bug-fix work-folder, RCA-document, and investigator-added regression-test references supplied in that context.

Decide where the user's feedback should go next. Return "changes_requested" with actionable feedback when the user is asking for implementation changes that can be made within the approved plan. Return "replan_requested" with actionable feedback for the planner when the user's feedback means the approved plan, scope, requirements, verification strategy, or safety constraints must change before implementation continues. Do not route by keyword alone; decide from the full feedback and plan context. Preserve all nine structured arrays with semantic deep equality and unchanged order. Preserve the `Goal`, `Validation`, `Work dir`, `Plan doc`, `Validation doc`, `RCA doc`, and `Repro test` values exactly in output fields when present.]],
      output = {
        status = { "changes_requested", "replan_requested" },
        fields = { feedback = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", implementation_commands = "array", implementation_evidence = "array", tester_commands = "array", tester_evidence = "array", validator_commands = "array", validator_evidence = "array", reviewer_commands = "array", reviewer_evidence = "array", reviewer_assessments = "array" },
      },
    }
  end
  return review
end
