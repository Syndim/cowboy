local start = step("start")
start.run = function(ctx)
  return action.status { status = "success", body = "fixed" }
end

-- The third argument may be a plain string as a description shorthand.
return workflow("fix", start, "Quick auto-fix workflow")
