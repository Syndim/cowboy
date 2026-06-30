local start = step("start")
start.run = function(ctx)
  return action.status { status = "success" }
end

-- No description declared: the catalog reports it as <none>.
return workflow("plain", start)
