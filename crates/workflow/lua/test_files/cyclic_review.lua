local developer = role("developer", "Implement code changes")
local reviewer = role("reviewer", "Review code changes")

local implement = step("implement", { role = developer })
implement.run = function(ctx)
  return action.agent {
    role = developer,
    prompt = "Implement: " .. ctx.request,
    output = {
      status = { "success", "failed" },
      fields = { summary = "string" },
    },
  }
end

local review = step("review", { role = reviewer })
review.run = function(ctx)
  return action.agent {
    role = reviewer,
    prompt = "Review previous output: " .. (ctx.prev and ctx.prev.body or ""),
    output = {
      status = { "approved", "needs_fix" },
      fields = { comments = "string" },
    },
  }
end

implement:on("success", review)
review:on("needs_fix", implement)

return workflow("cyclic-review", implement)
