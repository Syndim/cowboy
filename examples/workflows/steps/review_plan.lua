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

Return "approved" if the plan is specific, scoped, and verifiable. Return "changes_requested" with feedback otherwise. In both cases, include a concise `plan` field containing the plan content that should be shown to the user for confirmation.]],
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string", plan = "string" },
      },
    }
  end
  return review_plan
end
