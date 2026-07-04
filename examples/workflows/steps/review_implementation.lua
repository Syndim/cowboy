local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review", { role = roles.reviewer })
  review.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review the implementation and test results for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Test result:") .. [[

Inspect the working tree and plan document at the `Plan doc: ...` path. For bug fixes, also inspect the bug-fix work folder at `Work dir: ...`, the RCA document at `RCA doc: ...`, and the investigator-added regression test identified by `Repro test: ...`; verify that test still validates the original issue and passed in the test step. Verify every checked TODO item is actually completed, require unfinished work items to remain unchecked, preserve the work-dir/plan-doc/RCA/repro-test paths in output fields, and return "approved" only if the change is correct, scoped, sufficiently tested, and all required TODO items are complete. Return "changes_requested" with actionable feedback otherwise.]],
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string", work_dir = "string", plan_doc = "string", rca_doc = "string", repro_test = "string" },
      },
    }
  end
  return review
end
