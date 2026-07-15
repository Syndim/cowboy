local context = require("utils/context.lua")

return function(id)
  local opts = type(id) == "table" and id or { id = id }
  local triage = step(opts.id or "triage_blocked")
  triage.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    local response = tostring(fields.blocked_response or "")
    local normalized = string.lower(response)
    local blocked_from_step = tostring(fields.blocked_from_step or "")
    local next_step = "implement"

    if blocked_from_step == "investigate" or string.match(normalized, "investigat") or string.match(normalized, "rca") or string.match(normalized, "root cause") or string.match(normalized, "repro") then
      next_step = "investigate"
    elseif string.match(normalized, "plan") or string.match(normalized, "scope") or string.match(normalized, "requirement") or string.match(normalized, "start over") then
      next_step = "plan"
    elseif blocked_from_step == "revise" or string.match(normalized, "revise") or string.match(normalized, "review feedback") then
      next_step = "revise"
    elseif opts.validation_step and (blocked_from_step == opts.validation_step or string.match(normalized, "validat")) then
      next_step = opts.validation_step
    end

    return action.status {
      status = next_step,
      fields = {
        summary = "Blocked workflow triaged to " .. next_step,
        feedback = response,
        user_feedback = context.copy_user_feedback(fields),
        goal = fields.goal,
        validation = fields.validation,
        work_dir = fields.work_dir,
        plan_doc = fields.plan_doc,
        rca_doc = fields.rca_doc,
        repro_test = fields.repro_test,
        files = fields.files or {},
        blocked_from_step = fields.blocked_from_step,
        blocked_from_status = fields.blocked_from_status,
      },
      body = "Blocked workflow user response:\n" .. response,
    }
  end
  return triage
end
