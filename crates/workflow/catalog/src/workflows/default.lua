local developer = role("developer", {
  instructions = [[You are a careful software engineer. Implement the user's request completely, keep changes focused, and report exactly what changed.]]
})

local implement = step("implement", { role = developer })
implement.run = function(ctx)
  local request = ctx.request or ctx.goal or ctx.input or ""
  return action.agent {
    role = developer,
    prompt = [[Implement the user's request in the current project.

Request:
]] .. tostring(request) .. [[

Return a structured result with status success, failed, or needs_fix. Include a concise summary and the files you changed or inspected.]],
    output = {
      status = { "success", "failed", "needs_fix" },
      fields = {
        summary = "string",
        files = "array"
      }
    }
  }
end

local finish = step("finish")
finish.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.status {
    status = "success",
    fields = {
      summary = fields.summary or "Workflow completed",
      files = fields.files or {}
    }
  }
end

local failed = step("failed")
failed.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.status {
    status = "failed",
    fields = {
      summary = fields.summary or "Workflow failed",
      files = fields.files or {}
    }
  }
end

local needs_fix = step("needs_fix")
needs_fix.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.status {
    status = "needs_fix",
    fields = {
      summary = fields.summary or "Workflow needs follow-up fixes",
      files = fields.files or {}
    }
  }
end

implement:on("success", finish)
implement:on("failed", failed)
implement:on("needs_fix", needs_fix)

return workflow("default", implement)
