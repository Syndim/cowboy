local reviewer = role("reviewer", "Review changes for correctness and clarity")

local inspect = step("inspect", { role = reviewer })
inspect.run = function(ctx)
  return action.agent {
    role = reviewer,
    prompt = "Review the change for: " .. ctx.request,
    output = {
      status = { "approve", "request_changes" },
      fields = { summary = "string" },
    },
  }
end

return workflow("review", inspect, {
  description = "Reviews a code change and approves or requests changes",
})
