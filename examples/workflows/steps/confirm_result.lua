local context = require("utils/context.lua")

return function(id)
  local confirm = step(id or "confirm_result")
  confirm.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    if ctx.prev and ctx.prev.action == "ask_user" then
      local answer = fields.answer
      if answer and tostring(answer) ~= "" then
        local normalized = string.lower(tostring(answer))
        if normalized == "yes" or normalized == "y" or normalized == "approve" or normalized == "approved" then
          return action.status { status = "confirmed", fields = { goal = fields.goal, validation = fields.validation, work_dir = fields.work_dir, plan_doc = fields.plan_doc, rca_doc = fields.rca_doc, repro_test = fields.repro_test }, body = "user approved implementation" }
        end
        return action.status {
          status = "changes_requested",
          fields = { feedback = tostring(answer), goal = fields.goal, validation = fields.validation, work_dir = fields.work_dir, plan_doc = fields.plan_doc, rca_doc = fields.rca_doc, repro_test = fields.repro_test },
          body = "user requested implementation changes",
        }
      end
    end

    local review = context.previous_step_context(ctx, "Approved review:")
    local prompt_id = "result_confirmation_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "Review the implementation summary below. Type 'yes' to approve and commit it, or describe the changes you want before committing.\n" .. tostring(review),
      choices = {},
      fields = { goal = fields.goal, validation = fields.validation, work_dir = fields.work_dir, plan_doc = fields.plan_doc, rca_doc = fields.rca_doc, repro_test = fields.repro_test },
    }
  end
  return confirm
end
