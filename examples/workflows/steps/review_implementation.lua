local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review", { role = roles.reviewer })
  review.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review the implementation and test results for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Test result:") .. [[

Inspect the working tree and plan document at the `Plan doc: ...` path. Verify every checked TODO item is actually completed, require unfinished work items to remain unchecked, preserve the plan-doc path in output `plan_doc`, and return "approved" only if the change is correct, scoped, sufficiently tested, and all required TODO items are complete. Return "changes_requested" with actionable feedback otherwise.]],
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string", plan_doc = "string" },
      },
    }
  end
  return review
end
