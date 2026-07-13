return function(id)
  local collect = step(id or "collect_validation")
  collect.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    local goal = fields.goal or tostring(ctx.request)

    if ctx.prev and ctx.prev.action == "ask_user" then
      local answer = fields.answer
      if answer and tostring(answer) ~= "" then
        return action.status {
          status = "captured",
          fields = {
            goal = goal,
            validation = tostring(answer),
          },
          body = "captured user-provided goal and validation method",
        }
      end
    end

    local prompt_id = "validation_context_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "The run request below is the Goal. How should Cowboy validate that this Goal has been achieved? Provide the exact command, test, manual procedure, or observable result that must be used.\n\nGoal:\n" .. goal,
      choices = {},
      fields = { goal = goal },
    }
  end
  return collect
end
