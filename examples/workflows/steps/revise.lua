local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local revise = step(opts.id or "revise", { role = roles.implementer })
  local feedback_source = opts.feedback_source or "reviewer"
  revise.run = function(ctx)
    return action.agent {
      role = roles.implementer,
      prompt = [[Address ]] .. feedback_source .. [[ feedback for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, feedback_source .. " feedback:") .. context.preserve_user_feedback_guidance() .. context.evidence_record_guidance() .. [[

Address only the feedback above. As you complete follow-up work, update the approved plan document's stable `TODO-NN` list so completed items are checked and incomplete items remain unchecked. Each checked item must contain its current implementer-observed result beneath the declared procedure and expected result. Emit complete replacement `implementation_commands` and `implementation_evidence` arrays in plan order with exactly one record per checked TODO. For every affected ID, replace its sole stale implementation record with current `source: implementer`, `subject_kind: todo` evidence and `comparisons: []`. For every untouched ID, preserve the prior parsed record with semantic deep equality, including its array position, recursively nested values, and scalar types/contents. Reject duplicate records, missing procedure steps, and command records whose `procedure_index` does not map to a command step in the sole evidence record. YAML formatting and object-key order are irrelevant. Never renumber subjects or reuse IDs. Do not carry stale tester, validator, or reviewer claims forward as new implementation evidence.

If a `Repro test: ...` path/name is present above, do not edit that investigator-added test case; fix product code or follow-up implementation instead. Preserve the `Goal: ...`, `Validation: ...`, `Work dir: ...`, `Plan doc: ...`, `Validation doc: ...`, `RCA doc: ...`, and `Repro test: ...` values exactly in your output fields when present. Return "implemented" only when the relevant TODO items are completed, checked, and evidenced, or "blocked" if you cannot proceed.]],
      output = {
        status = { "implemented", "blocked" },
        fields = { summary = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", files = "array", implementation_commands = "array", implementation_evidence = "array" },
      },

    }
  end
  return revise
end
