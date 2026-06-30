local developer = role("developer", "Implement small changes")

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

return workflow("basic", implement)
