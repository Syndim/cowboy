local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local test = step(opts.id or "test", { role = roles.tester })
  local kind = opts.kind or "change"
  test.run = function(ctx)
    return action.agent {
      role = roles.tester,
      prompt = [[Run local tests for this ]] .. kind .. [[ request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Implementation result:") .. [[

Run the relevant local test commands for the changed files. Prefer focused tests first; include broader tests if needed. Return "passed" only if the relevant tests pass. Return "failed" with exact failures if tests fail, or "blocked" if tests cannot be run.]],
      output = {
        status = { "passed", "failed", "blocked" },
        fields = { summary = "string", commands = "array", failures = "array" },
      },
    }
  end
  return test
end
