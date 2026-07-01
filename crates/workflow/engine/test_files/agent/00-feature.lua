-- A multi-role agent workflow for exercising the engine with a real backend.
--
-- Requires an authenticated ACP coding agent (e.g. COWBOY_ENGINE_BACKEND=omp).
-- Each step runs the agent in the current directory; routing is driven by the
-- YAML-frontmatter `status` each agent returns. Run it pointed at this dir:
--
--   COWBOY_ENGINE_BACKEND=omp \
--   COWBOY_ENGINE_WORKFLOWS=engine-workflows/agent \
--   engine-cli run "add a /healthz route that returns 200"
--
-- Agent steps only see ctx.request (the original goal). State flows between
-- steps through the shared per-role backend session and the real repository,
-- not through ctx.prev. The review/revise loop repeats until the reviewer
-- approves (bounded by the runner's max_visits_per_step budget).

local planner = role("planner", [[You are a senior engineer who turns a request into a concrete, repository-grounded plan.
Inspect the repository before planning. Write the plan to a Markdown document before returning ready. The document must include these sections exactly: Plan, Changes, Tests to be added/updated, How to verify, and TODO. The TODO section must list every implementation work item as checkable Markdown tasks. Do not change code, tests, configs, or workflow logic while planning.]])

local developer = role("developer", [[You are a careful software engineer.
Implement the approved plan in the current repository, keeping the diff focused and consistent with existing conventions. Mark each TODO item in the approved plan document completed as you finish it, and do not report implemented while relevant TODO items remain unchecked. Report exactly what you changed.]])

local reviewer = role("reviewer", [[You are a meticulous code reviewer.
Inspect the working tree, plan document, and TODO checklist for correctness, scope creep, and obvious bugs. Verify each checked TODO item is actually complete, and do not approve while required TODO items are unchecked or falsely checked; otherwise give specific, actionable feedback.]])

local function clarification_context(ctx)
  local resume = ctx.resume or {}
  local clarifications = {}

  for key, value in pairs(resume) do
    local order = tonumber(string.match(tostring(key), "^clarification_(%d+)$"))
    if order and value and tostring(value) ~= "" then
      table.insert(clarifications, { order = order, value = tostring(value) })
    end
  end

  table.sort(clarifications, function(a, b) return a.order < b.order end)
  if #clarifications == 0 then
    return ""
  end

  local lines = { "", "Additional user context:" }
  for _, item in ipairs(clarifications) do
    table.insert(lines, "- " .. item.value)
  end
  return table.concat(lines, "\n")
end

local function request_context(ctx)
  return tostring(ctx.request) .. clarification_context(ctx)
end

local function previous_step_context(ctx, heading)
  local prev = ctx.prev
  if not prev then
    return ""
  end

  local lines = { "", heading }
  if prev.step then
    table.insert(lines, "Step: " .. tostring(prev.step))
  end
  if prev.status then
    table.insert(lines, "Status: " .. tostring(prev.status))
  end

  local fields = prev.fields or {}
  if fields.summary then
    table.insert(lines, "Summary: " .. tostring(fields.summary))
  end
  if fields.feedback then
    table.insert(lines, "Feedback: " .. tostring(fields.feedback))
  end
  if fields.plan_doc then
    table.insert(lines, "Plan doc: " .. tostring(fields.plan_doc))
  end
  if fields.files and #fields.files > 0 then
    table.insert(lines, "Files:")
    for _, file in ipairs(fields.files) do
      table.insert(lines, "- " .. tostring(file))
    end
  end
  if prev.body and tostring(prev.body) ~= "" then
    table.insert(lines, "Body:")
    table.insert(lines, tostring(prev.body))
  end

  return table.concat(lines, "\n")
end

local plan = step("plan", { role = planner })
plan.run = function(ctx)
  return action.agent {
    role = planner,
    prompt = [[Produce a concrete implementation plan for this request:

]] .. request_context(ctx) .. [[

Before returning "ready", create or update a Markdown plan document at `docs/plans/<snake_case_summary>.md`. Generate `<snake_case_summary>` from the concise plan summary by lowercasing it, removing punctuation, and joining words with underscores. Create `docs/plans` if it does not exist. The plan document must contain these sections exactly: Plan, Changes, Tests to be added/updated, How to verify, and TODO. The TODO section must contain every implementation work item as Markdown task-list items (`- [ ] ...`). Return `plan_doc` exactly as the written path and include that same path in `files`.

Return status "ready" with a short plan once you know which files to change, or "unclear" if the request cannot be planned.]],
    output = {
      status = { "ready", "unclear" },
      fields = { summary = "string", plan_doc = "string", files = "array" },
    },
  }
end

local implement = step("implement", { role = developer })
implement.run = function(ctx)
  return action.agent {
    role = developer,
    prompt = [[Implement this request in the current repository:

]] .. request_context(ctx) .. previous_step_context(ctx, "Previous plan:") .. [[

Make the change now, mark each completed TODO item in the approved plan document as `- [x]`, and leave incomplete items unchecked. Preserve the `Plan doc: ...` path exactly in your output `plan_doc`. Return status "implemented" only when all required TODO items are completed and checked, or "blocked" if you cannot proceed.]],
    output = {
      status = { "implemented", "blocked" },
      fields = { summary = "string", plan_doc = "string", files = "array" },
    },
  }
end

local review = step("review", { role = reviewer })
review.run = function(ctx)
  return action.agent {
    role = reviewer,
    prompt = [[Review the changes currently in the working tree for this request:

]] .. request_context(ctx) .. previous_step_context(ctx, "Implementation result:") .. [[

Inspect the working tree and plan document at the `Plan doc: ...` path. Verify every checked TODO item is actually completed, require unfinished work items to remain unchecked, preserve the plan-doc path in output `plan_doc`, and return status "approved" only if the change is correct, complete, and sufficiently tested. Return "changes_requested" with specific feedback the developer can act on.]],
    output = {
      status = { "approved", "changes_requested" },
      fields = { feedback = "string", plan_doc = "string" },
    },
  }
end

local revise = step("revise", { role = developer })
revise.run = function(ctx)
  return action.agent {
    role = developer,
    prompt = [[The reviewer requested changes for this request:

]] .. request_context(ctx) .. previous_step_context(ctx, "Reviewer feedback:") .. [[

Address only the reviewer feedback above. As you complete follow-up work, update the approved plan document's TODO list so completed items are checked and incomplete items remain unchecked. Preserve the `Plan doc: ...` path exactly in your output `plan_doc`. Return status "implemented" only when the relevant TODO items are completed and checked, or "blocked" if you cannot proceed.]],
    output = {
      status = { "implemented", "blocked" },
      fields = { summary = "string", plan_doc = "string", files = "array" },
    },
  }
end

local done = step("done")
done.run = function(ctx)
  return action.status { status = "success", body = "feature implemented and approved" }
end

local unclear = step("unclear")
unclear.run = function(ctx)
  local answered_prompt_id = "clarification_" .. tostring((ctx.steps_executed or 1) - 1)
  local answer = ctx.resume and ctx.resume[answered_prompt_id]
  if answer and tostring(answer) ~= "" then
    return action.status { status = "clarified", body = "received additional context" }
  end

  local prompt_id = "clarification_" .. tostring(ctx.steps_executed or 0)
  return action.ask_user {
    id = prompt_id,
    message = "The request is too unclear to plan. Please provide more context: user-visible behavior, entrypoint, expected inputs/outputs, and acceptance criteria.",
    choices = {},
  }
end

local blocked = step("blocked")
blocked.run = function(ctx)
  return action.status { status = "success", body = "implementation was blocked" }
end

plan:on("ready", implement)
plan:on("unclear", unclear)
unclear:on("clarified", plan)
implement:on("implemented", review)
implement:on("blocked", blocked)
review:on("approved", done)
review:on("changes_requested", revise)
revise:on("implemented", review)
revise:on("blocked", blocked)

return workflow("00-feature", plan, {
  description = "plan -> implement -> review (loop until approved) -> done",
})
