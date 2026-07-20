local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local plan = step(opts.id or "plan", { role = roles.planner })
  local kind = opts.kind or "change"
  local validation_guidance = ""
  if opts.require_user_validation then
    validation_guidance = [[

The prior context contains `Goal: ...` and `Validation: ...` values supplied by the user. Treat the Goal as the implementation contract. Preserve the user's Validation method in the plan's `How to verify` section; do not replace it with a preferred test strategy. Copy non-sensitive values exactly; when a value is sensitive, preserve its semantics with a safe placeholder or environment-variable reference instead of retaining the literal. Additional tests may supplement, but never substitute for, that method. Preserve both values exactly in the `goal` and `validation` output fields.]]
  end
  local validation_guide_guidance = ""
  if opts.require_validation_guide then
    validation_guide_guidance = [[

For every dev-loop planning pass, create or update one durable planning work folder and two artifacts inside it:
- `work_dir` at `docs/plans/<snake_case_summary>`
- `plan_doc` at `<work_dir>/plan.md`
- `validation_doc` at `<work_dir>/validation.md`

This nested layout is mandatory for both initial planning and replanning. Reuse an existing tuple only when it already has this exact layout. Never return flat, mismatched, or incomplete dev-loop artifact paths. Return `work_dir`, `plan_doc`, and `validation_doc` exactly as written, and include both document paths in `files`.

The validation guide must represent the user's exact Goal and Validation contract and contain prerequisites, ordered commands or manual checks, evidence to capture, explicit exit criteria for leaving the development loop, and continue/revise criteria for every failed check. Assign every ordered validation step and every exit criterion a stable `VAL-NN` identifier in document order. Each identified criterion must retain its exact text and define an executable command or ordered manual procedure plus an observable expected result. Never renumber existing `VAL-NN` identifiers during replanning; assign each new criterion the next unused number. Every exit criterion is mandatory.

Apply one sensitive-data policy to both artifacts: redact, generalize, or omit credentials, secrets, personal data, private paths, and proprietary content. Preserve a redacted Goal or Validation procedure semantically with explicit safe placeholders such as `<REDACTED_VALUE>` or environment-variable references so the guide remains executable without retaining the sensitive literal.]]
  end
  local ordinary_plan_guidance = [[

Before returning "ready" for ordinary non-bug-fix work, create or update a Markdown plan document at `docs/plans/<snake_case_summary>.md`. Generate snake_case names by lowercasing the concise summary, removing punctuation, and joining words with underscores. Create `docs/plans` if it does not exist.]]
  if opts.require_validation_guide then
    ordinary_plan_guidance = ""
  end
  local existing_plan_guidance = [[

If previous feedback includes `Plan doc: ...`, update that existing plan document instead of creating a separate plan path, preserve the same `plan_doc` value exactly in output fields, and include that same path in `files`.]]
  if opts.require_validation_guide then
    existing_plan_guidance = [[

If previous feedback includes a valid nested dev-loop `Work dir: ...`, `Plan doc: ...`, and `Validation doc: ...` tuple, update those exact documents and preserve all three values. If the prior tuple is flat, mismatched, or incomplete, replace it with the required nested tuple. Include both current document paths in `files`.]]
  end
  plan.run = function(ctx)
    return action.agent {
      role = roles.planner,
      prompt = [[Create a concrete plan for this ]] .. kind .. [[ request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Previous feedback:") .. validation_guidance .. validation_guide_guidance .. context.preserve_user_feedback_guidance() .. existing_plan_guidance .. [[

For bug fix requests, base the plan on the reviewed RCA document and investigator-added regression test from the prior step. The plan must reference the RCA doc, keep the repro test as an input to the fix, and must not ask the implementer to rewrite or replace that test. If `Work dir: ...` is present above, write the plan to `<work_dir>/plan.md` in that same bug-fix work folder; otherwise create `docs/plans/<snake_case_bug_summary>/plan.md` next to the RCA. Preserve `work_dir`, `rca_doc`, and `repro_test` exactly in output fields when present.]] .. ordinary_plan_guidance .. [[

The plan document must contain these sections exactly:
- Plan
- Changes
- Tests to be added/updated
- How to verify
- TODO

The plan document must not include sensitive user data. Redact, generalize, or omit secrets, credentials, personal data, private paths, and proprietary customer content while preserving actionable engineering detail.

The TODO section must contain every implementation work item as a Markdown task-list item in the form `- [ ] TODO-NN: <exact task text>`. Assign IDs in document order, never renumber or reuse existing IDs during replanning, and give every newly added task the next unused number. Beneath each TODO, define an executable command or ordered manual procedure and an observable expected result. The procedure and expected result are part of the stable subject definition. Do not use vague completion checks. Return `plan_doc` exactly as the written plan path, and include that same path in `files`.

Return status "ready" when the request is specific enough to implement, or "unclear" when more user context is needed.]],
      output = {
        status = { "ready", "unclear" },
        fields = { summary = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", files = "array", risks = "array", verification = "array" },
      },
    }
  end
  return plan
end
