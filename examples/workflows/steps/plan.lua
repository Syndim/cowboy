local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local plan = step(opts.id or "plan", { role = roles.planner })
  local kind = opts.kind or "change"
  plan.run = function(ctx)
    return action.agent {
      role = roles.planner,
      prompt = [[Create a concrete plan for this ]] .. kind .. [[ request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Previous feedback:") .. [[

Return status "ready" when the request is specific enough to implement, or "unclear" when more user context is needed.]],
      output = {
        status = { "ready", "unclear" },
        fields = { summary = "string", files = "array", risks = "array", verification = "array" },
      },
    }
  end
  return plan
end
