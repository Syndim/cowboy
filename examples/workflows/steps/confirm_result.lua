local context = require("utils/context.lua")

return function(id)
  local confirm = step(id or "confirm_result")

  local function result_fields(fields, user_feedback)
    return context.copy_evidence_fields(fields, {
      user_feedback = user_feedback,
      goal = fields.goal,
      validation = fields.validation,
      work_dir = fields.work_dir,
      plan_doc = fields.plan_doc,
      validation_doc = fields.validation_doc,
      rca_doc = fields.rca_doc,
      repro_test = fields.repro_test,
    })
  end

  confirm.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    if ctx.prev and ctx.prev.action == "wait_for_input" then
      local value = fields.input
      if value and tostring(value) ~= "" then
        local normalized = string.lower(tostring(value))
        if normalized == "yes" or normalized == "y" or normalized == "approve" or normalized == "approved" then
          return action.status {
            status = "confirmed",
            fields = result_fields(fields, context.copy_user_feedback(fields)),
            body = "user approved implementation",
          }
        end

        local copied = result_fields(fields, context.append_user_feedback(fields, "Result confirmation", value))
        copied.feedback = tostring(value)
        copied.changes_needed = { tostring(value) }
        copied.change_context = "The user requested these implementation changes during result confirmation."
        return action.status {
          status = "changes_requested",
          fields = copied,
          body = "user requested implementation changes",
        }
      end
    end

    local review = context.previous_step_context(ctx, "Approved review:")
    local input_id = "result_confirmation_" .. tostring(ctx.steps_executed or 0)
    return action.wait_for_input {
      id = input_id,
      message = "Review the implementation summary below. Type 'yes' to approve and commit it, or describe the changes you want before committing.\n" .. tostring(review),
      choices = {},
      fields = result_fields(fields, context.copy_user_feedback(fields)),
    }
  end

  return confirm
end
