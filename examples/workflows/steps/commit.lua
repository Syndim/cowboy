local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local commit = step(opts.id or "commit", { role = roles.committer })
  commit.run = function(ctx)
    return action.agent {
      role = roles.committer,
      prompt = [[Commit the request-related changes for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Approved implementation:") .. context.preserve_user_feedback_guidance() .. [[

Inspect the current diff, stage all request-related files, explicitly including `docs/plans/*.md` plan and validation-guide documents and `docs/plans/*/*.md` bug-fix work-folder documents when they were created or updated for this change, and create a local conventional commit. Do not push, amend, rebase, or reset. Preserve the `Goal: ...`, `Validation: ...`, `Work dir: ...`, `Plan doc: ...`, `Validation doc: ...`, `RCA doc: ...`, and `Repro test: ...` values exactly in output fields when present. Return "committed" with the commit hash/message, or "blocked" if committing is unsafe.]],
      output = {
        status = { "committed", "blocked" },
        fields = { summary = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", commit = "string" },
      },
    }
  end
  return commit
end
