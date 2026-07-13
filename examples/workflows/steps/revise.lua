local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local revise = step(opts.id or "revise", { role = roles.implementer })
  local feedback_source = opts.feedback_source or "reviewer"
  revise.run = function(ctx)
    return action.agent {
      role = roles.implementer,
      prompt = [[Address ]] .. feedback_source .. [[ feedback for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, feedback_source .. " feedback:") .. [[

Address only the feedback above. As you complete follow-up work, update the approved plan document's TODO list so completed items are checked and incomplete items remain unchecked. If a `Repro test: ...` path/name is present above, do not edit that investigator-added test case; fix product code or follow-up implementation instead. Preserve the `Goal: ...`, `Validation: ...`, `Work dir: ...`, `Plan doc: ...`, `RCA doc: ...`, and `Repro test: ...` values exactly in your output fields when present. Return "implemented" only when the relevant TODO items are completed and checked, or "blocked" if you cannot proceed.]],
      output = {
        status = { "implemented", "blocked" },
        fields = { summary = "string", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", rca_doc = "string", repro_test = "string", files = "array" },
      },
    }
  end
  return revise
end
