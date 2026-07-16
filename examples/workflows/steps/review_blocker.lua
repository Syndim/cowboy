local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review_blocker", { role = roles.blocker_reviewer })
  review.run = function(ctx)
    return action.agent {
      role = roles.blocker_reviewer,
      prompt = [[Review this workflow blocker before asking the user for help:

Request:
]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Captured blocker:") .. context.preserve_user_feedback_guidance() .. [[

Inspect the repository and every available `Plan doc: ...`, `Validation doc: ...`, `RCA doc: ...`, and `Repro test: ...` artifact. Return exactly one status:
- `recoverable` when the agent can safely clear the blocker from repository context and available tools. Set `blocker_resolution` to concrete, ordered, agent-executable recovery instructions.
- `user_required` only when a required decision, credential, permission, external resource, or manual action is unavailable to the agent. Set `blocker_resolution` to the exact minimal external input or action the user must provide.

For both statuses, set `blocker_reason` to the evidence-based recoverability analysis. Do not infer facts from generic feedback or body prose beyond the named `blocker_statement`. Preserve `blocker_statement`, `blocked_from_step`, `blocked_from_status`, `goal`, `validation`, `work_dir`, `plan_doc`, `validation_doc`, `rca_doc`, `repro_test`, and `files` exactly in output fields when present.]],
      output = {
        status = { "recoverable", "user_required" },
        fields = {
          summary = "string",
          user_feedback = "array",
          blocker_statement = "string",
          blocked_from_step = "string",
          blocked_from_status = "string",
          blocker_reason = "string",
          blocker_resolution = "string",
          goal = "string",
          validation = "string",
          work_dir = "string",
          plan_doc = "string",
          validation_doc = "string",
          rca_doc = "string",
          repro_test = "string",
          files = "array",
        },
      },
    }
  end

  return review
end
