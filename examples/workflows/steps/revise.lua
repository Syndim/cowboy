local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local revise = step(opts.id or "revise", { role = roles.implementer })
  revise.run = function(ctx)
    return action.agent {
      role = roles.implementer,
      prompt = [[Address reviewer feedback for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Reviewer feedback:") .. [[

Address only the reviewer feedback above. Return "implemented" with a summary, or "blocked" if you cannot proceed.]],
      output = {
        status = { "implemented", "blocked" },
        fields = { summary = "string", files = "array" },
      },
    }
  end
  return revise
end
