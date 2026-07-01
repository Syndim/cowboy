local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local revise = step(opts.id or "revise", { role = roles.implementer })
  revise.run = function(ctx)
    return action.agent {
      role = roles.implementer,
      prompt = [[Address reviewer feedback for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Reviewer feedback:") .. [[

Address only the reviewer feedback above. As you complete follow-up work, update the approved plan document's TODO list so completed items are checked and incomplete items remain unchecked. Preserve the `Plan doc: ...` path exactly in your output `plan_doc`. Return "implemented" only when the relevant TODO items are completed and checked, or "blocked" if you cannot proceed.]],
      output = {
        status = { "implemented", "blocked" },
        fields = { summary = "string", plan_doc = "string", files = "array" },
      },
    }
  end
  return revise
end
