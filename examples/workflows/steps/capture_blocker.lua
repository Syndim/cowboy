return function(id)
  local capture = step(id or "capture_blocker")
  capture.run = function(ctx)
    local previous = ctx.prev or {}
    local fields = {}
    for key, value in pairs(previous.fields or {}) do
      fields[key] = value
    end

    fields.blocker_statement = tostring(previous.body or "")
    fields.blocked_from_step = tostring(previous.step or "")
    fields.blocked_from_status = tostring(previous.status or "")

    return action.status {
      status = "captured",
      fields = fields,
      body = fields.blocker_statement,
    }
  end

  return capture
end
