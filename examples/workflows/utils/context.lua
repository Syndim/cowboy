local M = {}

function M.copy_user_feedback(fields)
  local copied = {}
  local user_feedback = fields and fields.user_feedback
  if type(user_feedback) == "table" then
    for index, entry in ipairs(user_feedback) do
      copied[index] = entry
    end
  end
  return copied
end

function M.append_user_feedback(fields, stage, feedback)
  local appended = M.copy_user_feedback(fields)
  table.insert(appended, tostring(stage) .. ": " .. tostring(feedback))
  return appended
end

function M.preserve_user_feedback_guidance()
  return [[

Preserve `user_feedback` exactly in output fields when present. It is cumulative raw user direction; do not add agent- or reviewer-generated feedback to it.]]
end

function M.review_user_feedback_guidance()
  return [[

Evaluate the revised work against the complete user feedback history above as well as repository rules, document constraints, and test or validation evidence.]]
end

function M.clarification_context(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  if fields.clarification and tostring(fields.clarification) ~= "" then
    return "\nAdditional user context:\n- " .. tostring(fields.clarification)
  end
  return ""
end

function M.request_context(ctx)
  return tostring(ctx.request) .. M.clarification_context(ctx)
end

function M.previous_step_context(ctx, heading)
  local prev = ctx.prev
  if not prev then return "" end
  local lines = { "", heading }
  if prev.step then table.insert(lines, "Step: " .. tostring(prev.step)) end
  if prev.status then table.insert(lines, "Status: " .. tostring(prev.status)) end
  local fields = prev.fields or {}
  if fields.user_feedback and #fields.user_feedback > 0 then
    table.insert(lines, "User feedback history:")
    for _, feedback in ipairs(fields.user_feedback) do
      table.insert(lines, "- " .. tostring(feedback))
    end
  end
  if fields.summary then table.insert(lines, "Summary: " .. tostring(fields.summary)) end
  if fields.feedback then table.insert(lines, "Feedback: " .. tostring(fields.feedback)) end
  if fields.goal then table.insert(lines, "Goal: " .. tostring(fields.goal)) end
  if fields.validation then table.insert(lines, "Validation: " .. tostring(fields.validation)) end
  if fields.work_dir then table.insert(lines, "Work dir: " .. tostring(fields.work_dir)) end
  if fields.plan_doc then table.insert(lines, "Plan doc: " .. tostring(fields.plan_doc)) end
  if fields.validation_doc then table.insert(lines, "Validation doc: " .. tostring(fields.validation_doc)) end
  if fields.rca_doc then table.insert(lines, "RCA doc: " .. tostring(fields.rca_doc)) end
  if fields.repro_test then table.insert(lines, "Repro test: " .. tostring(fields.repro_test)) end
  if fields.blocker_statement then table.insert(lines, "Blocker statement: " .. tostring(fields.blocker_statement)) end
  if fields.blocked_from_step then table.insert(lines, "Blocked from step: " .. tostring(fields.blocked_from_step)) end
  if fields.blocked_from_status then table.insert(lines, "Blocked from status: " .. tostring(fields.blocked_from_status)) end
  if fields.blocker_reason then table.insert(lines, "Blocker reason: " .. tostring(fields.blocker_reason)) end
  if fields.blocker_resolution then table.insert(lines, "Blocker resolution: " .. tostring(fields.blocker_resolution)) end
  if fields.files and #fields.files > 0 then
    table.insert(lines, "Files:")
    for _, file in ipairs(fields.files) do table.insert(lines, "- " .. tostring(file)) end
  end
  if fields.command then table.insert(lines, "Command: " .. tostring(fields.command)) end
  if fields.commands and #fields.commands > 0 then
    table.insert(lines, "Commands:")
    for _, command in ipairs(fields.commands) do table.insert(lines, "- " .. tostring(command)) end
  end
  if fields.failure then table.insert(lines, "Failure: " .. tostring(fields.failure)) end
  if fields.failures and #fields.failures > 0 then
    table.insert(lines, "Failures:")
    for _, failure in ipairs(fields.failures) do table.insert(lines, "- " .. tostring(failure)) end
  end
  if prev.body and tostring(prev.body) ~= "" then
    table.insert(lines, "Body:")
    table.insert(lines, tostring(prev.body))
  end
  return table.concat(lines, "\n")
end

return M
