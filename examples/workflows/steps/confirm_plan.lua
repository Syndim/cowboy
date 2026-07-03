local context = require("utils/context.lua")

return function(id)
  local confirm = step(id or "confirm_plan")
  confirm.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    if ctx.prev and ctx.prev.action == "ask_user" then
      local answer = fields.answer
      if answer and tostring(answer) ~= "" then
        local normalized = string.lower(tostring(answer))
        if normalized == "yes" or normalized == "y" or normalized == "approve" or normalized == "approved" then
          local reviewed_plan = fields.plan or context.previous_step_context(ctx, "Reviewed plan:")
          return action.status {
            status = "confirmed",
            fields = { plan = reviewed_plan, plan_doc = fields.plan_doc },
            body = tostring(reviewed_plan),
          }
        end
        return action.status {
          status = "changes_requested",
          fields = { feedback = tostring(answer), plan_doc = fields.plan_doc },
          body = "user requested plan changes",
        }
      end
    end

    local reviewed_plan = fields.plan or context.previous_step_context(ctx, "Reviewed plan:")
    local prompt_id = "plan_confirmation_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "Review the approved plan below. Type 'yes' to approve it, or describe the changes you want before implementation.\n" .. tostring(reviewed_plan),
      choices = {},
      fields = { plan = reviewed_plan, plan_doc = fields.plan_doc },
    }
  end
  return confirm
end
