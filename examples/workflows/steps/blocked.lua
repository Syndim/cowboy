return function(body, id)
  local blocked = step(id or "blocked")
  blocked.run = function(ctx)
    local previous_fields = (ctx.prev and ctx.prev.fields) or {}
    if ctx.prev and ctx.prev.action == "ask_user" then
      local answer = previous_fields.answer
      if answer and tostring(answer) ~= "" then
        local fields = {}
        for key, value in pairs(previous_fields) do
          fields[key] = value
        end
        fields.blocked_response = tostring(answer)
        fields.blocked_from_step = previous_fields.blocked_from_step
        fields.blocked_from_status = previous_fields.blocked_from_status
        return action.status {
          status = "triaged",
          fields = fields,
          body = "Workflow was blocked. User response:\n" .. tostring(answer),
        }
      end
    end

    local prompt_id = "blocked_" .. tostring(ctx.steps_executed or 0)
    local fields = {}
    for key, value in pairs(previous_fields) do
      fields[key] = value
    end
    fields.blocked_from_step = ctx.prev and ctx.prev.step
    fields.blocked_from_status = ctx.prev and ctx.prev.status
    return action.ask_user {
      id = prompt_id,
      message = "The workflow is blocked. What should Cowboy do next?\n" .. tostring(body or "workflow blocked"),
      choices = {},
      fields = fields,
    }
  end
  return blocked
end

