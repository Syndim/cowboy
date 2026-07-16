local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review_plan = step(opts.id or "review_plan", { role = roles.reviewer })
  local validation_guidance = ""
  if opts.require_user_validation then
    validation_guidance = [[

The prior context contains the user's exact `Goal: ...` and `Validation: ...` contract. Require the plan's `How to verify` section to use that Validation method without substitution, and reject a plan that weakens, rewrites, or omits it. Preserve both values exactly in the `goal` and `validation` output fields.]]
  end
  local validation_guide_guidance = ""
  if opts.require_validation_guide then
    validation_guide_guidance = [[

Read and review both `plan_doc` and `validation_doc`. Reject the output unless both paths are stable, both artifacts are included in `files`, and the validation guide contains prerequisites, ordered executable steps, evidence requirements, mandatory exit criteria, and continue/revise criteria. Require every exit criterion to pass before the development loop can end; reject ambiguous, incomplete, weakened, or substituted validation guidance.

Reject credentials, secrets, personal data, private paths, or proprietary content in either artifact. When redaction is necessary, verify that explicit safe placeholders or environment-variable references preserve the user's Goal and Validation procedure semantically and keep the guide executable. Include the reviewed content of both planning artifacts in the `plan` field shown for confirmation, and preserve `validation_doc` exactly in output fields.]]
  end
  review_plan.run = function(ctx)
    return action.agent {
      role = roles.reviewer,
      prompt = [[Review this plan before implementation:

Request:
]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Plan output:") .. validation_guidance .. validation_guide_guidance .. context.preserve_user_feedback_guidance() .. context.review_user_feedback_guidance() .. [[

Verify the plan document does not include sensitive user data; require redaction or generalization of secrets, credentials, personal data, private paths, and proprietary customer content.

Return "approved" only if the plan is specific, scoped, verifiable, and the plan document path is correct. For ordinary work, the plan path is `docs/plans/<snake_case_summary>.md`; for bug fixes with `Work dir: ...`, the plan path is `<work_dir>/plan.md` in the same `docs/plans/<snake_case_bug_summary>/` folder as the RCA. Verify the plan document contains the required Plan, Changes, Tests to be added/updated, How to verify, and TODO sections with Markdown task-list items. For bug fix plans, verify the plan references the reviewed RCA doc and treats the investigator-added repro test as an unchanged regression guard. Return "changes_requested" with feedback otherwise. In both cases, include a concise `plan` field containing the plan content that should be shown to the user for confirmation, preserve `plan_doc` exactly from the plan output, and preserve `work_dir`, `rca_doc`, and `repro_test` when present.]],
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string", plan = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string" },
      },
    }
  end
  return review_plan
end
