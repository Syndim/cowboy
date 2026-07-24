local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local implement = step(opts.id or "implement", { role = roles.implementer })
  local kind = opts.kind or "change"
  implement.run = function(ctx)
    local prompt, errors = context.build_agent_prompt(ctx, {
      objective = "Implement this " .. kind .. " request.",
      heading = "Approved plan:",
      require_previous = true,
      include_step = true,
      include_status = true,
      fields = {
        "user_feedback", "summary", "goal", "validation", "work_dir", "plan_doc",
        "validation_doc", "rca_doc", "repro_test", "files",
      },
      required_fields = { "plan_doc" },
      include_body = true,
      guidance = { "preserve_user_feedback", "evidence_records" },
      instructions = [[Make the change now. As you complete work, update the approved plan document's TODO list by changing each completed `- [ ] TODO-NN` item to `- [x] TODO-NN`; leave incomplete items unchecked. For every checked TODO, add its implementer-observed result beneath the declared procedure and expected result in the plan, then emit exactly one matching `implementation_evidence` record in plan order. That sole record must use `subject_kind: todo`, the stable ID and exact task text, `source: implementer`, the complete ordered procedure, expected and observed results, applicability, match outcome, and `comparisons: []`. Emit every executed command in ordered `implementation_commands`; every command must map to a command step in that sole record through the same subject keys and its one-based `procedure_index`. Duplicate evidence records, missing procedure steps, unmapped commands, a changed file alone, or an unsupported completion claim are invalid. Leave incomplete, mismatched, not-run, duplicate, or unproven required TODOs unchecked, and do not return `implemented` while any required TODO lacks exactly one valid record.

If a `Repro test: ...` path/name is present above, do not edit that investigator-added test case; make product-code changes so that test passes. Preserve the `Goal: ...`, `Validation: ...`, `Work dir: ...`, `Plan doc: ...`, `Validation doc: ...`, `RCA doc: ...`, and `Repro test: ...` values exactly in your output fields when present. Return "implemented" only when all TODO items required for this implementation are completed, checked, and evidenced. Return "blocked" if you cannot proceed.]],
    })
    if not prompt then return context.invalid_context_action(ctx, "blocked", errors) end
    return action.agent {
      role = roles.implementer,
      prompt = prompt,
      output = {
        status = { "implemented", "blocked" },
        fields = { summary = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", files = "array", implementation_commands = "array", implementation_evidence = "array" },
      },

    }
  end
  return implement
end
