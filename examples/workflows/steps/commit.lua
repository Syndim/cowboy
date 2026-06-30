local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local commit = step(opts.id or "commit", { role = roles.committer })
  commit.run = function(ctx)
    return action.agent {
      role = roles.committer,
      prompt = [[Commit the code changes for this request:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Approved implementation:") .. [[

Inspect the current diff, stage only relevant files, and create a local conventional commit. Do not push, amend, rebase, or reset. Return "committed" with the commit hash/message, or "blocked" if committing is unsafe.]],
      output = {
        status = { "committed", "blocked" },
        fields = { summary = "string", commit = "string" },
      },
    }
  end
  return commit
end
