local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review_plan = step(opts.id or "review_plan", { role = roles.reviewer })
  review_plan.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review this plan before implementation:

Request:
]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Plan output:") .. [[

Return "approved" only if the plan is specific, scoped, verifiable, and the plan document path is `docs/plans/<snake_case_summary>.md` with snake_case generated from the summary. Verify the plan document contains the required Plan, Changes, Tests to be added/updated, How to verify, and TODO sections with Markdown task-list items. Return "changes_requested" with feedback otherwise. In both cases, include a concise `plan` field containing the plan content that should be shown to the user for confirmation, and preserve `plan_doc` exactly from the plan output.]],
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string", plan = "string", plan_doc = "string" },
      },
    }
  end
  return review_plan
end
