local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local implement = step(opts.id or "implement", { role = roles.implementer })
  local kind = opts.kind or "change"
  implement.run = function(ctx)
    return action.agent {
      role = roles.implementer,
      prompt = [[Implement this ]] .. kind .. [[ request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Approved plan:") .. [[

Make the change now. Return "implemented" with a summary and files, or "blocked" if you cannot proceed.]],
      output = {
        status = { "implemented", "blocked" },
        fields = { summary = "string", files = "array" },
      },
    }
  end
  return implement
end
