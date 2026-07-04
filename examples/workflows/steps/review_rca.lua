local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review_rca = step(opts.id or "review_rca", { role = roles.reviewer })
  review_rca.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review this bug Root Cause Analysis before fix planning:

Request:
]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "RCA output:") .. [[

Inspect the RCA document at the `RCA doc: ...` path and the regression test identified by `Repro test: ...`. Validate that the RCA explains the bug behavior, why it happens, and how to reproduce it. Validate that the regression test is focused on the reported issue and currently fails for the bug rather than for unrelated setup or assertion mistakes.

Return "approved" only when the RCA is repository-grounded and the failing test correctly demonstrates the issue. Return "changes_requested" with actionable feedback otherwise. Preserve `work_dir`, `rca_doc`, and `repro_test` exactly from the RCA output.]],
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string", work_dir = "string", rca_doc = "string", repro_test = "string", commands = "array", failures = "array" },
      },
    }
  end
  return review_rca
end
