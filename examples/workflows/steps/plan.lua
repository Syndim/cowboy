local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local plan = step(opts.id or "plan", { role = roles.planner })
  local kind = opts.kind or "change"
  local validation_guidance = ""
  if opts.require_user_validation then
    validation_guidance = [[

The prior context contains `Goal: ...` and `Validation: ...` values supplied by the user. Treat the Goal as the implementation contract. Copy the user's Validation method exactly into the plan's `How to verify` section; do not replace it with a preferred test strategy. Additional tests may supplement, but never substitute for, that method. Preserve both values exactly in the `goal` and `validation` output fields.]]
  end
  plan.run = function(ctx)
    return action.agent {
      role = roles.planner,
      prompt = [[Create a concrete plan for this ]] .. kind .. [[ request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Previous feedback:") .. validation_guidance .. context.preserve_user_feedback_guidance() .. [[

If previous feedback includes `Plan doc: ...`, update that existing plan document instead of creating a separate plan path, preserve the same `plan_doc` value exactly in output fields, and include that same path in `files`.

For bug fix requests, base the plan on the reviewed RCA document and investigator-added regression test from the prior step. The plan must reference the RCA doc, keep the repro test as an input to the fix, and must not ask the implementer to rewrite or replace that test. If `Work dir: ...` is present above, write the plan to `<work_dir>/plan.md` in that same bug-fix work folder; otherwise create `docs/plans/<snake_case_bug_summary>/plan.md` next to the RCA. Preserve `work_dir`, `rca_doc`, and `repro_test` exactly in output fields when present.

Before returning "ready" for ordinary non-bug-fix work, create or update a Markdown plan document at `docs/plans/<snake_case_summary>.md`. Generate snake_case names by lowercasing the concise summary, removing punctuation, and joining words with underscores. Create `docs/plans` if it does not exist.

The plan document must contain these sections exactly:
- Plan
- Changes
- Tests to be added/updated
- How to verify
- TODO

The plan document must not include sensitive user data. Redact, generalize, or omit secrets, credentials, personal data, private paths, and proprietary customer content while preserving actionable engineering detail.

The TODO section must contain every implementation work item as Markdown task-list items (`- [ ] ...`). Return `plan_doc` exactly as the written plan path, and include that same path in `files`.

Return status "ready" when the request is specific enough to implement, or "unclear" when more user context is needed.]],
      output = {
        status = { "ready", "unclear" },
        fields = { summary = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", rca_doc = "string", repro_test = "string", files = "array", risks = "array", verification = "array" },
      },
    }
  end
  return plan
end
