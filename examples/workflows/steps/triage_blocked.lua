local context = require("utils/context.lua")

return function(id)
  local opts = type(id) == "table" and id or { id = id }
  local allowed_steps = {}
  for _, step_id in ipairs(opts.retry_steps or { "plan", "implement", "revise" }) do
    allowed_steps[step_id] = true
  end

  local function user_requested_step(response)
    local trimmed = string.match(response, "^%s*(.-)%s*$") or ""
    local requested_step = string.match(trimmed, "^/route%s+([%w_-]+)$")
    if not requested_step then
      requested_step = string.match(trimmed, "^route:%s*([%w_-]+)$")
    end

    if requested_step then
      requested_step = string.lower(requested_step)
    end

    if requested_step and allowed_steps[requested_step] then
      return requested_step
    end

    return nil
  end

  local triage = step(opts.id or "triage_blocked")
  triage.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    local blocked_from_step = tostring(fields.blocked_from_step or "")
    local user_response = tostring(fields.blocked_response or "")
    local recovery = user_response ~= "" and user_response or tostring(fields.blocker_resolution or "")
    local next_step = allowed_steps[blocked_from_step] and blocked_from_step or "implement"

    if user_response ~= "" then
      next_step = user_requested_step(user_response) or next_step
    end

    if next_step == "revise" then
      local valid, errors = context.validate_evidence_source(fields, "implementation", true)
      if not valid then
        local diagnostics = table.concat(context.format_validation_errors(errors), "; ")
        next_step = "implement"
        local reconstruction = "Implementation context must be reconstructed before revision: "
          .. diagnostics
        recovery = recovery ~= "" and (recovery .. "\n\n" .. reconstruction) or reconstruction
      end
    end

    local output_fields = context.copy_evidence_fields(fields, {
      summary = "Blocked workflow triaged to " .. next_step,
      feedback = recovery,
      user_feedback = context.copy_user_feedback(fields),
      blocker_statement = fields.blocker_statement,
      blocked_from_step = fields.blocked_from_step,
      blocked_from_status = fields.blocked_from_status,
      blocker_reason = fields.blocker_reason,
      blocker_resolution = fields.blocker_resolution,
      blocked_response = fields.blocked_response,
      goal = fields.goal,
      validation = fields.validation,
      work_dir = fields.work_dir,
      plan_doc = fields.plan_doc,
      validation_doc = fields.validation_doc,
      rca_doc = fields.rca_doc,
      repro_test = fields.repro_test,
      files = fields.files or {},
    })
    return action.status {
      status = next_step,
      fields = output_fields,
      body = "Blocker recovery instructions:\n" .. recovery,
    }
  end

  return triage
end
