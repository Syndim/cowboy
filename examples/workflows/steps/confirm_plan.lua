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
          local reviewed_plan = fields.plan or context.previous_step_context(ctx, "Reviewed planning artifacts:")
          return action.status {
            status = "confirmed",
            fields = { plan = reviewed_plan, user_feedback = context.copy_user_feedback(fields), goal = fields.goal, validation = fields.validation, work_dir = fields.work_dir, plan_doc = fields.plan_doc, validation_doc = fields.validation_doc, rca_doc = fields.rca_doc, repro_test = fields.repro_test },
            body = tostring(reviewed_plan),
          }
        end

        return action.status {
          status = "changes_requested",
          fields = { feedback = tostring(answer), user_feedback = context.append_user_feedback(fields, "Plan confirmation", answer), goal = fields.goal, validation = fields.validation, work_dir = fields.work_dir, plan_doc = fields.plan_doc, validation_doc = fields.validation_doc, rca_doc = fields.rca_doc, repro_test = fields.repro_test },
          body = "user requested plan changes",
        }
      end
    end

    local reviewed_plan = fields.plan or context.previous_step_context(ctx, "Reviewed planning artifacts:")
    local prompt_id = "plan_confirmation_" .. tostring(ctx.steps_executed or 0)
    local subject = fields.validation_doc and "planning artifacts" or "plan"
    return action.ask_user {
      id = prompt_id,
      message = "Review the approved " .. subject .. " below. Type 'yes' to approve, or describe the changes you want before implementation.\n" .. tostring(reviewed_plan),
      choices = {},
      fields = { plan = reviewed_plan, user_feedback = context.copy_user_feedback(fields), goal = fields.goal, validation = fields.validation, work_dir = fields.work_dir, plan_doc = fields.plan_doc, validation_doc = fields.validation_doc, rca_doc = fields.rca_doc, repro_test = fields.repro_test },
    }
  end

  return confirm
end
