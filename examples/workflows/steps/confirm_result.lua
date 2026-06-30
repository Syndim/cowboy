local context = require("utils/context.lua")

return function(id)
  local confirm = step(id or "confirm_result")
  confirm.run = function(ctx)
    local answered_prompt_id = "result_confirmation_" .. tostring((ctx.steps_executed or 1) - 1)
    local answer = ctx.resume and ctx.resume[answered_prompt_id]
    if answer and tostring(answer) ~= "" then
      local normalized = string.lower(tostring(answer))
      if normalized == "yes" or normalized == "y" or normalized == "approve" or normalized == "approved" then
        return action.status { status = "confirmed", body = "user approved implementation" }
      end
      return action.status {
        status = "changes_requested",
        fields = { feedback = tostring(answer) },
        body = "user requested implementation changes",
      }
    end

    local review = context.previous_step_context(ctx, "Approved review:")
    local prompt_id = "result_confirmation_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "Review the implementation summary below. Type 'yes' to approve and commit it, or describe the changes you want before committing.\n" .. tostring(review),
      choices = {},
    }
  end
  return confirm
end
