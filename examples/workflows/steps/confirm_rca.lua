local context = require("utils/context.lua")

return function(id)
  local confirm = step(id or "confirm_rca")
  confirm.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    if ctx.prev and ctx.prev.action == "ask_user" then
      local answer = fields.answer
      if answer and tostring(answer) ~= "" then
        local normalized = string.lower(tostring(answer))
        if normalized == "yes" or normalized == "y" or normalized == "approve" or normalized == "approved" then
          return action.status {
            status = "confirmed",
            fields = { user_feedback = context.copy_user_feedback(fields), summary = fields.summary, work_dir = fields.work_dir, rca_doc = fields.rca_doc, repro_test = fields.repro_test, files = fields.files, command = fields.command, commands = fields.commands, failure = fields.failure, failures = fields.failures },
            body = "user approved RCA",
          }
        end
        return action.status {
          status = "changes_requested",
          fields = { feedback = tostring(answer), user_feedback = context.append_user_feedback(fields, "RCA confirmation", answer), summary = fields.summary, work_dir = fields.work_dir, rca_doc = fields.rca_doc, repro_test = fields.repro_test, files = fields.files, command = fields.command, commands = fields.commands, failure = fields.failure, failures = fields.failures },
          body = "user requested RCA changes",
        }
      end
    end

    local rca = context.previous_step_context(ctx, "Reviewer-approved RCA for user review:")
    local prompt_id = "rca_confirmation_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "The reviewer approved this RCA. Review the RCA below and the RCA document path. Type 'yes' if the RCA makes sense, or describe what should change before planning.\n" .. tostring(rca),
      choices = {},
      fields = { user_feedback = context.copy_user_feedback(fields), summary = fields.summary, work_dir = fields.work_dir, rca_doc = fields.rca_doc, repro_test = fields.repro_test, files = fields.files, command = fields.command, commands = fields.commands, failure = fields.failure, failures = fields.failures },
    }
  end
  return confirm
end
