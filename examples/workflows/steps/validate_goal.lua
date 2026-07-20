local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local validate = step(opts.id or "validate", { role = roles.validator })
  validate.run = function(ctx)
    return action.agent {
      role = roles.validator,
      prompt = [[Validate whether the current implementation has achieved the user's Goal:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Current test result:") .. context.preserve_user_feedback_guidance() .. context.preserve_evidence_guidance() .. context.evidence_record_guidance() .. [[

Read the exact `Goal: ...`, `Validation: ...`, and `Validation doc: ...` values above. Inspect the current working tree, implementation plan, and validation guide. Preserve `implementation_commands`, `implementation_evidence`, `tester_commands`, and `tester_evidence` with semantic deep equality and unchanged array order. Execute the user-provided Validation method exactly as represented by the guide while following its complete ordered procedure. TODO evidence supplements but never replaces this acceptance contract.

For every ordered validation step and exit criterion, require the guide's stable `VAL-NN` identifier and exact criterion text. Emit ordered `validator_commands` plus exactly one `validator_evidence` record per criterion using `subject_kind: validation_criterion`, `source: validator`, the complete ordered procedure, expected and observed results, applicability, match outcome, and an explicitly rendered `comparisons: []`. Reject duplicate `(validator, validation_criterion, VAL-NN)` records, missing steps, or command records whose `procedure_index` does not map to the sole criterion record. A future validator record may contain comparisons only when its prompt explicitly names that source. Missing, duplicate, renumbered, vague, non-executable, unmapped, not-run, or mismatched criteria cannot achieve the Goal.

Return "achieved" only when the exact user Validation method, all ordered checks, and every exit criterion pass with the required evidence. Return "not_achieved" with actionable feedback, exact evidence, and failed continue/revise criteria when the procedure runs but any criterion fails. Return "blocked" with the concrete blocker when the procedure cannot be performed. Preserve the `Goal`, `Validation`, `Work dir`, `Plan doc`, `Validation doc`, `RCA doc`, and `Repro test` values exactly in output fields when present.]],
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
          implementation_commands = "array",
          implementation_evidence = "array",
          tester_commands = "array",
          tester_evidence = "array",
          validator_commands = "array",
          validator_evidence = "array",
          failures = "array",
        },
      },

    }
  end

  return validate
end
