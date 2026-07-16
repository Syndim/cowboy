local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local validate = step(opts.id or "validate", { role = roles.validator })
  validate.run = function(ctx)
    return action.agent {
      role = roles.validator,
      prompt = [[Validate whether the current implementation has achieved the user's Goal:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Current test result:") .. context.preserve_user_feedback_guidance() .. [[

Read the exact `Goal: ...`, `Validation: ...`, and `Validation doc: ...` values above. Inspect the current working tree, implementation plan, and validation guide. Execute the user-provided Validation method exactly as represented by the guide while following the guide's complete ordered procedure. Capture the evidence required for every step and evaluate every exit criterion; supplementary checks cannot substitute for the prescribed procedure. Return "achieved" only when all ordered checks and every exit criterion pass with the required evidence. Return "not_achieved" with actionable feedback, exact evidence, and failed continue/revise criteria when the procedure runs but any criterion fails. Return "blocked" with the concrete blocker when the procedure cannot be performed. Preserve the `Goal`, `Validation`, `Work dir`, `Plan doc`, `Validation doc`, `RCA doc`, and `Repro test` values exactly in output fields when present.]],
      output = {
        status = { "achieved", "not_achieved", "blocked" },
        fields = {
          summary = "string",
          feedback = "string",
          user_feedback = "array",
          goal = "string",
          validation = "string",
          work_dir = "string",
          plan_doc = "string",
          validation_doc = "string",
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
