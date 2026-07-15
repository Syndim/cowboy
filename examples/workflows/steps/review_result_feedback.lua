local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review_result_feedback", { role = roles.reviewer })
  review.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review the user's post-implementation feedback for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "User result feedback:") .. context.preserve_user_feedback_guidance() .. context.review_user_feedback_guidance() .. [[

Inspect the user's feedback, the implementation, and the plan document at the `Plan doc: ...` path. For bug fixes, also preserve the bug-fix work folder at `Work dir: ...`, the RCA document at `RCA doc: ...`, and the investigator-added regression test identified by `Repro test: ...`.

Decide where the user's feedback should go next. Return "changes_requested" with actionable feedback when the user is asking for implementation changes that can be made within the approved plan. Return "replan_requested" with actionable feedback for the planner when the user's feedback means the approved plan, scope, requirements, verification strategy, or safety constraints must change before implementation continues. Do not route by keyword alone; decide from the full feedback and plan context. Preserve the `Goal`, `Validation`, `Work dir`, `Plan doc`, `RCA doc`, and `Repro test` values exactly in output fields when present.]],
      output = {
        status = { "changes_requested", "replan_requested" },
        fields = { feedback = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", rca_doc = "string", repro_test = "string" },
      },
    }
  end
  return review
end
