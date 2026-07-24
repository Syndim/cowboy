local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review = step(opts.id or "review_blocker", { role = roles.blocker_reviewer })
  review.run = function(ctx)
    local prompt, errors = context.build_agent_prompt(ctx, {
      objective = "Review this workflow blocker before asking the user for help.",
      heading = "Captured blocker:",
      require_previous = true,
      include_step = true,
      include_status = true,
      fields = {
        "user_feedback", "summary", "blocker_statement", "blocked_from_step",
        "blocked_from_status", "goal", "validation", "work_dir", "plan_doc",
        "validation_doc", "rca_doc", "repro_test", "files",
      },
      required_fields = { "blocker_statement" },
      evidence = { "implementation", "tester", "validator", "reviewer" },
      reviewer_assessments = true,
      include_body = true,
      guidance = { "preserve_user_feedback", "preserve_evidence" },
      instructions = [[Inspect the repository and every available `Plan doc: ...`, `Validation doc: ...`, `RCA doc: ...`, and `Repro test: ...` artifact. Return exactly one status:
- `recoverable` when the agent can safely clear the blocker from repository context and available tools. Set `blocker_resolution` to concrete, ordered, agent-executable recovery instructions.
- `user_required` only when a required decision, credential, permission, external resource, or manual action is unavailable to the agent. Set `blocker_resolution` to the exact minimal external input or action the user must provide.

For both statuses, set `blocker_reason` to the evidence-based recoverability analysis. Do not infer facts from generic feedback or body prose beyond the named `blocker_statement`. Preserve every source-specific command and evidence array with semantic deep equality and unchanged array order. Preserve `blocker_statement`, `blocked_from_step`, `blocked_from_status`, `goal`, `validation`, `work_dir`, `plan_doc`, `validation_doc`, `rca_doc`, `repro_test`, and `files` exactly in output fields when present.]],
    })
    if not prompt then return context.invalid_context_action(ctx, "user_required", errors) end
    return action.agent {
      role = roles.blocker_reviewer,
      prompt = prompt,
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
          implementation_commands = "array",
          implementation_evidence = "array",
          tester_commands = "array",
          tester_evidence = "array",
          validator_commands = "array",
          validator_evidence = "array",
          reviewer_commands = "array",
          reviewer_evidence = "array",
          reviewer_assessments = "array",


        },
      },
    }
  end

  return review
end
