local context = require("utils/context.lua")

return function(body, id)
  local blocked = step(id or "blocked")
  blocked.run = function(ctx)
    local previous_fields = (ctx.prev and ctx.prev.fields) or {}
    if ctx.prev and ctx.prev.action == "ask_user" then
      local answer = tostring(previous_fields.answer)
      if answer ~= "" then
        local fields = {}
        for key, value in pairs(previous_fields) do
          fields[key] = value
        end

        fields.user_feedback = context.copy_user_feedback(previous_fields)
        table.insert(fields.user_feedback, answer)
        fields.blocked_response = answer
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

    local message = table.concat({
      "The blocker reviewer determined that user action is required.",
      tostring(body or "workflow blocked"),
      "",
      "Blocker:",
      tostring(fields.blocker_statement or ""),
      "",
      "Reviewer analysis:",
      tostring(fields.blocker_reason or ""),
      "",
      "Required user action:",
      tostring(fields.blocker_resolution or ""),
      "",
      "To redirect instead of retrying the blocked step, reply with `/route <step>`.",
    }, "\n")
    return action.ask_user {
      id = prompt_id,
      message = message,
      choices = {},
      fields = fields,
    }
  end

  return blocked
end

