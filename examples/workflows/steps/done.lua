return function(body)
  local done = step("done")
  done.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    return action.status {
      status = "success",
      fields = fields,
      body = body or "workflow completed",
    }
  end
  return done
end
