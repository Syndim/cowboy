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

Before returning "ready", create or update a Markdown plan document at `docs/plans/<snake_case_summary>.md`. Generate `<snake_case_summary>` from the concise plan summary by lowercasing it, removing punctuation, and joining words with underscores. Create `docs/plans` if it does not exist.

The plan document must contain these sections exactly:
- Plan
- Changes
- Tests to be added/updated
- How to verify
- TODO

The TODO section must contain every implementation work item as Markdown task-list items (`- [ ] ...`). Return `plan_doc` exactly as the written `docs/plans/<snake_case_summary>.md` path, and include that same path in `files`.

Return status "ready" when the request is specific enough to implement, or "unclear" when more user context is needed.]],
      output = {
        status = { "ready", "unclear" },
        fields = { summary = "string", plan_doc = "string", files = "array", risks = "array", verification = "array" },
      },
    }
  end
  return plan
end
