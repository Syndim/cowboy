local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local validate = step(opts.id or "validate", { role = roles.validator })
  validate.run = function(ctx)
    return action.agent {
      role = roles.validator,
      prompt = [[Validate whether the current implementation has achieved the user's Goal:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Current test result:") .. [[

Read the exact `Goal: ...` and `Validation: ...` values above. Inspect the current working tree and plan document. Execute the user-provided Validation method exactly as written; do not substitute preferred tests, infer success from implementation details, or treat supplementary checks as equivalent evidence. Return "achieved" only when that method successfully demonstrates the Goal. Return "not_achieved" with actionable feedback and exact evidence when the method runs but does not demonstrate the Goal. Return "blocked" with the concrete blocker when the prescribed method cannot be performed. Preserve the `Goal`, `Validation`, `Work dir`, `Plan doc`, `RCA doc`, and `Repro test` values exactly in output fields when present.]],
      output = {
        status = { "achieved", "not_achieved", "blocked" },
        fields = {
          summary = "string",
          feedback = "string",
          goal = "string",
          validation = "string",
          work_dir = "string",
          plan_doc = "string",
          rca_doc = "string",
          repro_test = "string",
          commands = "array",
          evidence = "array",
          failures = "array",
        },
      },
    }
  end
  return validate
end
