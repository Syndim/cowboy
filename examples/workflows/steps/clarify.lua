return function(id)
  local clarify = step(id or "clarify")
  clarify.run = function(ctx)
    local answered_prompt_id = "clarification_" .. tostring((ctx.steps_executed or 1) - 1)
    local answer = ctx.resume and ctx.resume[answered_prompt_id]
    if answer and tostring(answer) ~= "" then
      return action.status { status = "clarified", body = "received additional context" }
    end

    local prompt_id = "clarification_" .. tostring(ctx.steps_executed or 0)
    return action.ask_user {
      id = prompt_id,
      message = "Please provide enough context to plan this work: desired behavior, entrypoint, expected output/state changes, constraints, and verification criteria.",
      choices = {},
    }
  end
  return clarify
end
