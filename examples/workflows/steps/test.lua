local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local test = step(opts.id or "test", { role = roles.tester })
  local kind = opts.kind or "change"
  test.run = function(ctx)
    local prompt, errors = context.build_agent_prompt(ctx, {
      objective = "Run local tests for this " .. kind .. " request.",
      heading = "Implementation result:",
      require_previous = true,
      include_step = true,
      include_status = true,
      fields = {
        "user_feedback", "summary", "goal", "validation", "work_dir", "plan_doc",
        "validation_doc", "rca_doc", "repro_test", "files", "failures",
      },
      evidence = { { name = "implementation", required = true } },
      include_body = true,
      guidance = { "preserve_user_feedback", "preserve_evidence", "evidence_records" },
      instructions = [[Read the approved plan. Verify every checked `TODO-NN` contains an implementer-observed result and exactly one matching implementation record with identical task text, complete ordered procedure, and expected result. Reject duplicate `(source, subject_kind, subject_id)` records, missing steps, and implementation command records that do not map through `procedure_index` to the sole record. Preserve `implementation_commands` and `implementation_evidence` with semantic deep equality and unchanged array order. Independently rerun every applicable TODO procedure in declared order; do not treat an implementer claim as a test result. Emit ordered `tester_commands` and exactly one `tester_evidence` record per checked TODO using `subject_kind: todo`, the same stable ID and exact task text, `source: tester`, the complete procedure, tester-observed result, and exactly one comparison against the sole implementer observed result. A required TODO marked not applicable does not pass silently.

The output MUST include all four source-specific arrays: preserved `implementation_commands` and `implementation_evidence`, plus complete `tester_commands` and `tester_evidence`. A generic `commands` list or prose pass report does not satisfy this contract. For checked work, both evidence arrays must be nonempty and contain exactly one subject-keyed record per checked TODO. Command arrays may be empty only when every corresponding evidence record declares `procedure.kind: manual` and no procedure step is a command. When any checked record declares `procedure.kind: command`, the corresponding command array must contain every executed command mapped to that sole evidence record by `subject_kind`, `subject_id`, and `procedure_index`; mixed manual/command plans need command entries only for command procedures. Do not return `passed` when a mandatory array is missing or malformed, an evidence array is empty for checked work, a command array is empty despite a command procedure, or any checked TODO lacks its sole tester record.

Run focused tests first and broader tests when needed. If a `Repro test: ...` path/name is present above, run that investigator-added regression test and require it to pass after the fix. Return `failed` for missing, duplicate, stale, unsafe, non-executable, reordered, unmapped, not-run, or mismatched evidence, with exact failures; never select or merge duplicate records. Preserve the `Goal: ...`, `Validation: ...`, `Work dir: ...`, `Plan doc: ...`, `Validation doc: ...`, `RCA doc: ...`, and `Repro test: ...` values exactly in your output fields when present. Return "passed" only if every required reproduction and relevant test passes, or "blocked" only when they cannot be run.]],
    })
    if not prompt then return context.invalid_context_action(ctx, "failed", errors) end
    return action.agent {
      role = roles.tester,
      prompt = prompt,
      output = {
        status = { "passed", "failed", "blocked" },
        fields = { summary = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", implementation_commands = "array", implementation_evidence = "array", tester_commands = "array", tester_evidence = "array", failures = "array" },
        required_fields = { "implementation_commands", "implementation_evidence", "tester_commands", "tester_evidence" },
      },
    }
  end
  return test
end
