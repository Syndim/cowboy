local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local commit = step(opts.id or "commit", { role = roles.committer })
  commit.run = function(ctx)
    return action.agent {
      role = roles.committer,
      prompt = [[Commit the request-related changes for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Approved implementation:") .. [[

Inspect the current diff, stage all request-related files, explicitly including `docs/plans/*.md` plan documents and `docs/plans/*/*.md` bug-fix work-folder documents when they were created or updated for this change, and create a local conventional commit. Do not push, amend, rebase, or reset. Preserve the `Goal: ...` and `Validation: ...` values exactly in output fields when present. Return "committed" with the commit hash/message, or "blocked" if committing is unsafe.]],
      output = {
        status = { "committed", "blocked" },
        fields = { summary = "string", goal = "string", validation = "string", commit = "string" },
      },
    }
  end
  return commit
end
