-- A status-only workflow (no agent backend needed) that exercises the engine:
-- multiple steps, single-stepping, an ask_user boundary, branching, completion.
--
-- The id "00-demo" sorts before the built-in "default", so the engine's
-- deterministic selector picks it for `engine-cli run`.

local plan = step("plan")
plan.run = function(ctx)
  return action.status { status = "ready", body = "planned: " .. tostring(ctx.request) }
end

local confirm = step("confirm")
confirm.run = function(ctx)
  local answer = ctx.resume and ctx.resume.proceed
  if answer then
    return action.status { status = answer, body = "user chose " .. tostring(answer) }
  end
  return action.ask_user {
    id = "proceed",
    message = "Apply the plan?",
    choices = { "yes", "no" },
  }
end

local apply = step("apply")
apply.run = function(ctx)
  return action.status { status = "success", body = "applied the plan" }
end

local cancelled = step("cancelled")
cancelled.run = function(ctx)
  return action.status { status = "success", body = "cancelled by user" }
end

plan:on("ready", confirm)
confirm:on("yes", apply)
confirm:on("no", cancelled)

return workflow("00-demo", plan, {
  description = "plan, ask the user to confirm, then apply or cancel",
})
