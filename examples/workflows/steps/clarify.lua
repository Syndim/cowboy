return function(id)
  local clarify = step(id or "clarify")
  clarify.run = function(ctx)
    if ctx.prev and ctx.prev.action == "ask_user" then
      local fields = ctx.prev.fields or {}
      local answer = fields.answer
      if answer and tostring(answer) ~= "" then
        return action.status { status = "clarified", fields = { clarification = tostring(answer) }, body = "received additional context" }
      end
    end

    local prompt_id = "clarification_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "Please provide enough context to plan this work: desired behavior, entrypoint, expected output/state changes, constraints, and verification criteria.",
      choices = {},
    }
  end
  return clarify
end
