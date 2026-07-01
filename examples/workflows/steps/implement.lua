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

Make the change now. As you complete work, update the approved plan document's TODO list by changing each completed `- [ ]` item to `- [x]`; leave incomplete items unchecked. Preserve the `Plan doc: ...` path exactly in your output `plan_doc`. Return "implemented" only when all TODO items required for this implementation are completed and checked. Return "blocked" if you cannot proceed.]],
      output = {
        status = { "implemented", "blocked" },
        fields = { summary = "string", plan_doc = "string", files = "array" },
      },
    }
  end
  return implement
end
