local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review", { role = roles.reviewer })
  review.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review the implementation and test results for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Test result:") .. [[

Inspect the working tree and plan document at the `Plan doc: ...` path. For bug fixes, also inspect the bug-fix work folder at `Work dir: ...`, the RCA document at `RCA doc: ...`, and the investigator-added regression test identified by `Repro test: ...`; verify that test still validates the original issue and passed in the test step. Verify every checked TODO item is actually completed, require unfinished work items to remain unchecked, preserve the `Work dir`, `Plan doc`, `RCA doc`, and `Repro test` values exactly in output fields when present, and return "approved" only if the change is correct, scoped, sufficiently tested, and all required TODO items are complete. Return "changes_requested" with actionable feedback when the implementation has defects that can be fixed within the approved plan. Return "replan_requested" with actionable feedback for the planner when the implementation follows the approved plan but that plan is incomplete, unsafe, incorrectly scoped, unverifiable, or otherwise not solid.]],
      output = {
        status = { "approved", "changes_requested", "replan_requested" },
        fields = { feedback = "string", work_dir = "string", plan_doc = "string", rca_doc = "string", repro_test = "string" },
      },
    }
  end
  return review
end
