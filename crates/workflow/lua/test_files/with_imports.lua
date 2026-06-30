local roles = require("shared/roles.lua")

local implement = step("implement", { role = roles.developer })
implement.run = function(ctx)
  return action.status {
    status = "success",
    fields = { summary = "implemented with imported role" },
    body = "done",
  }
end

return workflow("with-imports", implement)
