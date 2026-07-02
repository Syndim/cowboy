return function(id)
  local triage = step(id or "triage_blocked")
  triage.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    local response = tostring(fields.blocked_response or "")
    local normalized = string.lower(response)
    local blocked_from_step = tostring(fields.blocked_from_step or "")
    local next_step = "implement"

    if string.match(normalized, "plan") or string.match(normalized, "scope") or string.match(normalized, "requirement") or string.match(normalized, "start over") then
      next_step = "plan"
    elseif blocked_from_step == "revise" or string.match(normalized, "revise") or string.match(normalized, "review feedback") then
      next_step = "revise"
    end

    return action.status {
      status = next_step,
      fields = {
        summary = "Blocked workflow triaged to " .. next_step,
        feedback = response,
        plan_doc = fields.plan_doc,
        files = fields.files or {},
        blocked_from_step = fields.blocked_from_step,
        blocked_from_status = fields.blocked_from_status,
      },
      body = "Blocked workflow user response:\n" .. response,
    }
  end
  return triage
end
