return function(body)
  local blocked = step("blocked")
  blocked.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    return action.status {
      status = "success",
      fields = fields,
      body = body or "workflow blocked",
    }
  end
  return blocked
end
