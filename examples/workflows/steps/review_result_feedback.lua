local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review_result_feedback", { role = roles.reviewer })
  review.run = function(ctx)
    local prompt, errors = context.build_agent_prompt(ctx, {
      objective = "Review the user's post-implementation feedback for this request.",
      heading = "User result feedback:",
      require_previous = true,
      include_step = true,
      include_status = true,
      fields = {
        "user_feedback", "summary", "feedback", "goal", "validation", "work_dir",
        "plan_doc", "validation_doc", "rca_doc", "repro_test", "files",
      },
      required_fields = { "user_feedback" },
      evidence = { "implementation", "tester", "validator", "reviewer" },
      reviewer_assessments = true,
      include_body = true,
      guidance = { "preserve_user_feedback", "review_user_feedback", "preserve_evidence" },
      instructions = [[Inspect the user's feedback, the implementation, the concrete plan document, and every selected source-labeled evidence array and reviewer assessment supplied in the previous-step context. Keep raw `user_feedback`, reviewer assessment rationale/issues, and reviewer rerun evidence distinct; never append evidence, issues, or reviewer-generated feedback to `user_feedback`. For bug fixes, also preserve the concrete bug-fix work-folder, RCA-document, and investigator-added regression-test references supplied in that context.

Decide where the user's feedback should go next. Return "changes_requested" with actionable feedback when the user is asking for implementation changes that can be made within the approved plan. Return "replan_requested" with actionable feedback for the planner when the user's feedback means the approved plan, scope, requirements, verification strategy, or safety constraints must change before implementation continues. Do not route by keyword alone; decide from the full feedback and plan context. Preserve every selected structured array with semantic deep equality and unchanged order. Preserve the `Goal`, `Validation`, `Work dir`, `Plan doc`, `Validation doc`, `RCA doc`, and `Repro test` values exactly in output fields when present.]],
    })
    if not prompt then return context.invalid_context_action(ctx, "changes_requested", errors) end
    return action.agent {
      role = roles.reviewer,
      prompt = prompt,
      output = {
        status = { "changes_requested", "replan_requested" },
        fields = { feedback = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", implementation_commands = "array", implementation_evidence = "array", tester_commands = "array", tester_evidence = "array", validator_commands = "array", validator_evidence = "array", reviewer_commands = "array", reviewer_evidence = "array", reviewer_assessments = "array" },
      },
    }
  end
  return review
end
