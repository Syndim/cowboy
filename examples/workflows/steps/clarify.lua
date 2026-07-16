local context = require("utils/context.lua")

return function(id)
  local clarify = step(id or "clarify")
  clarify.run = function(ctx)
    if ctx.prev and ctx.prev.action == "ask_user" then
      local fields = ctx.prev.fields or {}
      local answer = fields.answer
      if answer and tostring(answer) ~= "" then
        return action.status {
          status = "clarified",
          fields = {
            clarification = tostring(answer),
            user_feedback = context.copy_user_feedback(fields),
            goal = fields.goal,
            validation = fields.validation,
            work_dir = fields.work_dir,
            plan_doc = fields.plan_doc,
            validation_doc = fields.validation_doc,
            rca_doc = fields.rca_doc,
            repro_test = fields.repro_test,
          },
          body = "received additional context",
        }
      end
    end

    local previous_fields = (ctx.prev and ctx.prev.fields) or {}
    local prompt_id = "clarification_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "Please provide enough context to plan this work: desired behavior, entrypoint, expected output/state changes, constraints, and verification criteria.",
      choices = {},
      fields = { user_feedback = context.copy_user_feedback(previous_fields), goal = previous_fields.goal, validation = previous_fields.validation, work_dir = previous_fields.work_dir, plan_doc = previous_fields.plan_doc, validation_doc = previous_fields.validation_doc, rca_doc = previous_fields.rca_doc, repro_test = previous_fields.repro_test },
    }
  end
  return clarify
end
