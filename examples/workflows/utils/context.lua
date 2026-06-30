local M = {}

function M.clarification_context(ctx)
  local resume = ctx.resume or {}
  local clarifications = {}
  for key, value in pairs(resume) do
    local order = tonumber(string.match(tostring(key), "^clarification_(%d+)$"))
    if order and value and tostring(value) ~= "" then
      table.insert(clarifications, { order = order, value = tostring(value) })
    end
  end
  table.sort(clarifications, function(a, b) return a.order < b.order end)
  if #clarifications == 0 then return "" end
  local lines = { "", "Additional user context:" }
  for _, item in ipairs(clarifications) do table.insert(lines, "- " .. item.value) end
  return table.concat(lines, "\n")
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
  if fields.summary then table.insert(lines, "Summary: " .. tostring(fields.summary)) end
  if fields.feedback then table.insert(lines, "Feedback: " .. tostring(fields.feedback)) end
  if fields.files and #fields.files > 0 then
    table.insert(lines, "Files:")
    for _, file in ipairs(fields.files) do table.insert(lines, "- " .. tostring(file)) end
  end
  if prev.body and tostring(prev.body) ~= "" then
    table.insert(lines, "Body:")
    table.insert(lines, tostring(prev.body))
  end
  return table.concat(lines, "\n")
end

return M
