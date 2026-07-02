return function(body)
  local blocked = step("blocked")
  blocked.run = function(ctx)
    local answered_prompt_id = "blocked_" .. tostring((ctx.steps_executed or 1) - 1)
    local answer = ctx.resume and ctx.resume[answered_prompt_id]
    local previous_fields = (ctx.prev and ctx.prev.fields) or {}
    if answer and tostring(answer) ~= "" then
      local fields = {}
      for key, value in pairs(previous_fields) do
        fields[key] = value
      end
      fields.blocked_response = tostring(answer)
      fields.blocked_from_step = ctx.prev and ctx.prev.step
      fields.blocked_from_status = ctx.prev and ctx.prev.status
      return action.status {
        status = "answered",
        fields = fields,
        body = "Workflow was blocked. User response:\n" .. tostring(answer),
      }
    end

    local prompt_id = "blocked_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "The workflow is blocked. What should Cowboy do next?\n" .. tostring(body or "workflow blocked"),
      choices = {},
    }
  end
  return blocked
end

