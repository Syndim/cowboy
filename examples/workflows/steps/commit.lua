local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local commit = step(opts.id or "commit", { role = roles.committer })
  commit.run = function(ctx)
    local prompt, errors = context.build_agent_prompt(ctx, {
      objective = "Commit the request-related changes.",
      heading = "Approved implementation:",
      require_previous = true,
      include_step = true,
      include_status = true,
      fields = {
        "user_feedback", "summary", "goal", "validation", "work_dir", "plan_doc",
        "validation_doc", "rca_doc", "repro_test", "files",
      },
      required_fields = { "plan_doc" },
      include_body = true,
      guidance = { "preserve_user_feedback" },
      instructions = [[Inspect the current diff, stage all request-related files, explicitly including `docs/plans/*.md` ordinary feature-plan documents and `docs/plans/*/*.md` dev-loop planning-folder or bug-fix work-folder documents when they were created or updated for this change, and create a local conventional commit. Do not push, amend, rebase, or reset. Preserve the `Goal: ...`, `Validation: ...`, `Work dir: ...`, `Plan doc: ...`, `Validation doc: ...`, `RCA doc: ...`, and `Repro test: ...` values exactly in output fields when present. Return "committed" with the commit hash/message, or "blocked" if committing is unsafe.]],
    })
    if not prompt then return context.invalid_context_action(ctx, "blocked", errors) end
    return action.agent {
      role = roles.committer,
      prompt = prompt,
      output = {
        status = { "committed", "blocked" },
        fields = { summary = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string", commit = "string" },
      },
    }
  end
  return commit
end
