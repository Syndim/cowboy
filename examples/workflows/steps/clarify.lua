return function(id)
  local clarify = step(id or "clarify")
  clarify.run = function(ctx)
    if ctx.prev and ctx.prev.action == "ask_user" then
      local fields = ctx.prev.fields or {}
      local answer = fields.answer
      if answer and tostring(answer) ~= "" then
        return action.status {
          status = "clarified",
          fields = {
            clarification = tostring(answer),
            goal = fields.goal,
            validation = fields.validation,
          },
          body = "received additional context",
        }
      end
    end

    local previous_fields = (ctx.prev and ctx.prev.fields) or {}
    local prompt_id = "clarification_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "Please provide enough context to plan this work: desired behavior, entrypoint, expected output/state changes, constraints, and verification criteria.",
      choices = {},
      fields = { goal = previous_fields.goal, validation = previous_fields.validation },
    }
  end
  return clarify
end
