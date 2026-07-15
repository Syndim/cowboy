local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local test = step(opts.id or "test", { role = roles.tester })
  local kind = opts.kind or "change"
  test.run = function(ctx)
    return action.agent {
      role = roles.tester,
      prompt = [[Run local tests for this ]] .. kind .. [[ request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Implementation result:") .. context.preserve_user_feedback_guidance() .. [[

Run the relevant local test commands for the changed files. Prefer focused tests first; include broader tests if needed. If a `Repro test: ...` path/name is present above, run that investigator-added regression test and require it to pass after the fix. Preserve the `Goal: ...`, `Validation: ...`, `Work dir: ...`, `Plan doc: ...`, `RCA doc: ...`, and `Repro test: ...` values exactly in your output fields when present. Return "passed" only if the relevant tests pass. Return "failed" with exact failures if tests fail, or "blocked" if tests cannot be run.]],
      output = {
        status = { "passed", "failed", "blocked" },
        fields = { summary = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", rca_doc = "string", repro_test = "string", commands = "array", failures = "array" },
      },
    }
  end
  return test
end
