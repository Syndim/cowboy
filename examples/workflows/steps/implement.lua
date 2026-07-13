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

Make the change now. As you complete work, update the approved plan document's TODO list by changing each completed `- [ ]` item to `- [x]`; leave incomplete items unchecked. If a `Repro test: ...` path/name is present above, do not edit that investigator-added test case; make product-code changes so that test passes. Preserve the `Goal: ...`, `Validation: ...`, `Work dir: ...`, `Plan doc: ...`, `RCA doc: ...`, and `Repro test: ...` values exactly in your output fields when present. Return "implemented" only when all TODO items required for this implementation are completed and checked. Return "blocked" if you cannot proceed.]],
      output = {
        status = { "implemented", "blocked" },
        fields = { summary = "string", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", rca_doc = "string", repro_test = "string", files = "array" },
      },
    }
  end
  return implement
end
