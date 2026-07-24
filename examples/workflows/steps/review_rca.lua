local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local review_rca = step(opts.id or "review_rca", { role = roles.reviewer })
  review_rca.run = function(ctx)
    local prompt, errors = context.build_agent_prompt(ctx, {
      objective = "Review this bug Root Cause Analysis before fix planning.",
      heading = "RCA output:",
      require_previous = true,
      include_step = true,
      include_status = true,
      fields = { "user_feedback", "summary", "work_dir", "rca_doc", "repro_test", "files", "command", "failure", "failures" },
      required_fields = { "work_dir", "rca_doc", "repro_test" },
      include_body = true,
      guidance = { "preserve_user_feedback", "review_user_feedback" },
      instructions = [[Inspect the RCA document at the `RCA doc: ...` path and the regression test identified by `Repro test: ...`. Validate that the RCA explains the bug behavior, why it happens, and how to reproduce it. Validate that the Root cause evidence section proves the stated root cause with a traceable, step-by-step walkthrough of how the bug happens: an example flow from real log lines (each quoted line explained) or, when logs are unavailable, specific source locations that carry the flow. Return "changes_requested" when the root cause is asserted without this step-by-step evidence, or the evidence does not actually demonstrate the claimed flow. Validate that the regression test is focused on the reported issue and currently fails for the bug rather than for unrelated setup or assertion mistakes.

Verify the RCA document does not include sensitive user data; require redaction or generalization of secrets, credentials, personal data, private paths, and proprietary customer content.

Return "approved" only when the RCA is repository-grounded and the failing test correctly demonstrates the issue. Return "changes_requested" with actionable feedback otherwise. Preserve `work_dir`, `rca_doc`, and `repro_test` exactly from the RCA output.]],
    })
    if not prompt then return context.invalid_context_action(ctx, "changes_requested", errors) end
    return action.agent {
      role = roles.reviewer,
      prompt = prompt,
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string", user_feedback = "array", work_dir = "string", rca_doc = "string", repro_test = "string", commands = "array", failures = "array" },
      },
    }
  end
  return review_rca
end
