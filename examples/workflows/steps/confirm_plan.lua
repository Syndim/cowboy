local context = require("utils/context.lua")

return function(id)
  local confirm = step(id or "confirm_plan")
  confirm.run = function(ctx)
    local answered_prompt_id = "plan_confirmation_" .. tostring((ctx.steps_executed or 1) - 1)
    local answer = ctx.resume and ctx.resume[answered_prompt_id]
    if answer and tostring(answer) ~= "" then
      local normalized = string.lower(tostring(answer))
      if normalized == "yes" or normalized == "y" or normalized == "approve" or normalized == "approved" then
        return action.status { status = "confirmed", body = "user approved plan" }
      end
      return action.status {
        status = "changes_requested",
        fields = { feedback = tostring(answer) },
        body = "user requested plan changes",
      }
    end

    local fields = (ctx.prev and ctx.prev.fields) or {}
    local reviewed_plan = fields.plan or context.previous_step_context(ctx, "Reviewed plan:")
    local prompt_id = "plan_confirmation_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "Review the approved plan below. Type 'yes' to approve it, or describe the changes you want before implementation.\n" .. tostring(reviewed_plan),
      choices = {},
    }
  end
  return confirm
end
