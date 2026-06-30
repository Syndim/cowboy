local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review", { role = roles.reviewer })
  review.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review the implementation and test results for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Test result:") .. [[

Inspect the working tree. Return "approved" if the change is correct, scoped, and sufficiently tested. Return "changes_requested" with actionable feedback otherwise.]],
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string" },
      },
    }
  end
  return review
end
