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

local evidence_fields = {
  "implementation_commands",
  "implementation_evidence",
  "tester_commands",
  "tester_evidence",
  "validator_commands",
  "validator_evidence",
  "reviewer_commands",
  "reviewer_evidence",
  "reviewer_assessments",
}

local evidence_sources = {
  { prefix = "implementation", label = "Implementation" },
  { prefix = "tester", label = "Tester" },
  { prefix = "validator", label = "Validator" },
  { prefix = "reviewer", label = "Reviewer" },
}

function M.copy_evidence_fields(fields, target)
  local copied = target or {}
  fields = fields or {}
  for _, field in ipairs(evidence_fields) do
    local value = fields[field]
    if value ~= nil then
      copied[field] = value
    end
  end
  return copied
end

function M.preserve_evidence_guidance()
  return [[

Preserve every incoming source-specific command/evidence array and `reviewer_assessments` as parsed structured values. Semantic deep equality is required: array length and element order must be unchanged, and every recursively nested value and scalar type/content must compare equal. YAML whitespace, quoting style, and object-key order are irrelevant. Keep raw `user_feedback`, reviewer assessment rationale/issues, and reviewer rerun evidence distinct. Add or replace only the arrays owned by your source.]]
end

function M.evidence_record_guidance()
  return [[

Use exactly one complete evidence record per `(source, subject_kind, subject_id)`; duplicate records are invalid. The single record contains the subject's complete ordered procedure, including every command and manual step:
- `subject_kind`: `todo` or `validation_criterion`
- `subject_id`: the stable `TODO-NN` or `VAL-NN` identifier
- `subject`: the exact task or validation-criterion text
- `source`: `implementer`, `tester`, `validator`, or `reviewer`
- `procedure`: an object with `kind` (`command` or `manual`) and a nonempty ordered `steps` array
- `expected_result`: the observable acceptance result
- `observed_result`: what this source actually observed
- `applicability`: `applicable` or `not_applicable`
- `match`: `matched`, `mismatched`, or `not_run`
- `comparisons`: an ordered array of `{ source, observed_result, match }` objects

An applicable record must contain at least one procedure step. Use `matched` only when the observed result satisfies the expected result and every required comparison matches. A not-applicable record must explain why in `observed_result`, use `not_run`, and cannot silently satisfy a required TODO or criterion. Record every executed command in the matching source command array as `{ subject_kind, subject_id, procedure_index, command, exit_status }`. `procedure_index` is the one-based index of that command in the sole evidence record's procedure steps; reject unmapped command records. Implementer and validator records require `comparisons: []`; tester TODO records require exactly one implementer comparison; reviewer TODO records require exactly one implementer and one tester comparison; reviewer validation records require exactly one validator comparison.]]
end

local function typed_scalar(value)
  local value_type = type(value)
  if value_type == "string" then return "string(" .. string.format("%q", value) .. ")" end
  if value_type == "number" then return "number(" .. tostring(value) .. ")" end
  if value_type == "boolean" then return "boolean(" .. tostring(value) .. ")" end
  return nil
end

local function array_shape(value)
  if type(value) ~= "table" then return "invalid", type(value) end
  local count = 0
  local maximum = 0
  for key, _ in pairs(value) do
    if type(key) ~= "number" or key < 1 or key ~= tonumber(string.format("%.0f", key)) then
      return "malformed", "non-array-key"
    end
    count = count + 1
    if key > maximum then maximum = key end
  end
  if count == 0 then return "empty", nil end
  if count ~= maximum then return "malformed", "gap" end
  return "nonempty", maximum
end

local function append_scalar(lines, indent, label, value)
  table.insert(lines, indent .. label .. ": " .. typed_scalar(value))
end

local function append_array(lines, indent, label, value, append_item)
  local shape, detail = array_shape(value)
  if shape == "empty" then
    table.insert(lines, indent .. label .. ": array(empty)")
    return
  end
  table.insert(lines, indent .. label .. ": array(nonempty,length=" .. tostring(detail) .. ")")
  for index = 1, detail do append_item(value[index], index) end
end

local function append_command_records(lines, label, records)
  append_array(lines, "", label .. " command records", records, function(record, index)
    table.insert(lines, "- Record " .. tostring(index) .. ":")
    append_scalar(lines, "  ", "Subject kind", record.subject_kind)
    append_scalar(lines, "  ", "Subject ID", record.subject_id)
    append_scalar(lines, "  ", "Procedure index", record.procedure_index)
    append_scalar(lines, "  ", "Command", record.command)
    append_scalar(lines, "  ", "Exit status", record.exit_status)
  end)
end

local function append_comparison(lines, comparison, index)
  table.insert(lines, "  - Comparison " .. tostring(index) .. ":")
  append_scalar(lines, "    ", "Source", comparison.source)
  append_scalar(lines, "    ", "Observed result", comparison.observed_result)
  append_scalar(lines, "    ", "Match", comparison.match)
end

local function append_evidence_records(lines, label, records)
  append_array(lines, "", label .. " evidence records", records, function(record, index)
    table.insert(lines, "- Record " .. tostring(index) .. ":")
    append_scalar(lines, "  ", "Subject kind", record.subject_kind)
    append_scalar(lines, "  ", "Subject ID", record.subject_id)
    append_scalar(lines, "  ", "Subject", record.subject)
    append_scalar(lines, "  ", "Source", record.source)
    append_scalar(lines, "  ", "Procedure kind", record.procedure.kind)
    append_array(lines, "  ", "Procedure steps", record.procedure.steps, function(step, step_index)
      table.insert(lines, "  - Step " .. tostring(step_index) .. ": " .. typed_scalar(step))
    end)
    append_scalar(lines, "  ", "Expected result", record.expected_result)
    append_scalar(lines, "  ", "Observed result", record.observed_result)
    append_scalar(lines, "  ", "Applicability", record.applicability)
    append_scalar(lines, "  ", "Match", record.match)
    append_array(lines, "  ", "Comparisons", record.comparisons, function(comparison, comparison_index)
      append_comparison(lines, comparison, comparison_index)
    end)
  end)
end

local function append_submission_issue(lines, issue, index)
  table.insert(lines, "  - Issue " .. tostring(index) .. ":")
  append_scalar(lines, "    ", "Source", issue.source)
  append_scalar(lines, "    ", "Code", issue.code)
  append_scalar(lines, "    ", "Field", issue.field)
  append_scalar(lines, "    ", "Message", issue.message)
end

local function append_reviewer_assessments(lines, assessments)
  append_array(lines, "", "Reviewer soundness assessments", assessments, function(assessment, index)
    table.insert(lines, "- Assessment " .. tostring(index) .. ":")
    append_scalar(lines, "  ", "Subject kind", assessment.subject_kind)
    append_scalar(lines, "  ", "Subject ID", assessment.subject_id)
    append_scalar(lines, "  ", "Subject", assessment.subject)
    append_scalar(lines, "  ", "Source", assessment.source)
    append_scalar(lines, "  ", "Completion state", assessment.completion_state)
    append_scalar(lines, "  ", "Proof verdict", assessment.proof_verdict)
    append_scalar(lines, "  ", "Relevance", assessment.relevance)
    append_scalar(lines, "  ", "Sufficiency", assessment.sufficiency)
    append_scalar(lines, "  ", "Safety and executability", assessment.safety_and_executability)
    append_scalar(lines, "  ", "Currentness", assessment.currentness)
    append_scalar(lines, "  ", "Falsifiability", assessment.falsifiability)
    append_scalar(lines, "  ", "Non-circularity", assessment.non_circularity)
    append_scalar(lines, "  ", "Submission verdict", assessment.submission_verdict)
    append_array(lines, "  ", "Submission issues", assessment.submission_issues, function(issue, issue_index)
      append_submission_issue(lines, issue, issue_index)
    end)
  end)
end

local scalar_fields = {
  summary = "Summary",
  feedback = "Feedback",
  goal = "Goal",
  validation = "Validation",
  work_dir = "Work dir",
  plan_doc = "Plan doc",
  validation_doc = "Validation doc",
  rca_doc = "RCA doc",
  repro_test = "Repro test",
  blocker_statement = "Blocker statement",
  blocked_from_step = "Blocked from step",
  blocked_from_status = "Blocked from status",
  blocker_reason = "Blocker reason",
  blocker_resolution = "Blocker resolution",
  command = "Command",
  failure = "Failure",
  clarification = "Additional user context",
}

local array_fields = {
  files = "Files",
  commands = "Commands",
  failures = "Failures",
  user_feedback = "User feedback history",
}

local function add_error(errors, field, expected, actual)
  table.insert(errors, {
    field = field,
    expected = expected,
    actual = actual,
  })
end

local function validate_scalar(errors, field, value, required)
  if value == nil then
    if required then add_error(errors, field, "scalar", "missing") end
    return false
  end
  if typed_scalar(value) == nil then
    add_error(errors, field, "scalar", type(value))
    return false
  end
  return true
end

local function validate_array(errors, field, value, required)
  if value == nil then
    if required then add_error(errors, field, "array", "missing") end
    return false
  end
  local shape, detail = array_shape(value)
  if shape == "invalid" or shape == "malformed" then
    add_error(errors, field, "array", detail or type(value))
    return false
  end
  return true
end

local function validate_scalar_array_entries(errors, field, value)
  local shape, count = array_shape(value)
  if shape == "empty" then return true end
  if shape ~= "nonempty" then return false end
  local valid = true
  for index = 1, count do
    if typed_scalar(value[index]) == nil then
      add_error(errors, field .. "[" .. tostring(index) .. "]", "scalar", type(value[index]))
      valid = false
    end
  end
  return valid
end

local user_feedback_keys = { "sequence", "kind", "content", "submitted_at" }

local function ordered_user_feedback_keys(entry)
  local keys = {}
  local seen = {}
  for _, key in ipairs(user_feedback_keys) do
    if entry[key] ~= nil then
      table.insert(keys, key)
      seen[key] = true
    end
  end
  local extra = {}
  for key, _ in pairs(entry) do
    if type(key) == "string" and not seen[key] then table.insert(extra, key) end
  end
  table.sort(extra)
  for _, key in ipairs(extra) do table.insert(keys, key) end
  return keys
end

local function validate_user_feedback_entries(errors, field, value)
  local shape, count = array_shape(value)
  if shape == "empty" then return true end
  if shape ~= "nonempty" then return false end
  local valid = true
  for index = 1, count do
    local entry = value[index]
    if typed_scalar(entry) == nil then
      if type(entry) ~= "table" then
        add_error(errors, field .. "[" .. tostring(index) .. "]", "scalar or object", type(entry))
        valid = false
      else
        local member_count = 0
        for key, member in pairs(entry) do
          member_count = member_count + 1
          if type(key) ~= "string" then
            add_error(
              errors,
              field .. "[" .. tostring(index) .. "]",
              "object with string keys",
              "key type " .. type(key)
            )
            valid = false
          elseif typed_scalar(member) == nil then
            add_error(
              errors,
              field .. "[" .. tostring(index) .. "]." .. key,
              "scalar",
              type(member)
            )
            valid = false
          end
        end
        if member_count == 0 then
          add_error(errors, field .. "[" .. tostring(index) .. "]", "nonempty object", "empty object")
          valid = false
        end
      end
    end
  end
  return valid
end

local function append_user_feedback(lines, label, entries)
  append_array(lines, "", label, entries, function(entry, index)
    if typed_scalar(entry) ~= nil then
      table.insert(lines, "- " .. tostring(entry))
      return
    end
    table.insert(lines, "- Entry " .. tostring(index) .. ":")
    for _, key in ipairs(ordered_user_feedback_keys(entry)) do
      table.insert(lines, "  " .. key .. ": " .. typed_scalar(entry[key]))
    end
  end)
end

local function validate_scalar_member(errors, path, value)
  if typed_scalar(value) == nil then
    add_error(errors, path, "scalar", value == nil and "missing" or type(value))
    return false
  end
  return true
end

local function validate_command_records(errors, field, records)
  local shape, count = array_shape(records)
  if shape == "empty" then return true end
  if shape ~= "nonempty" then return false end
  for index = 1, count do
    local record = records[index]
    local path = field .. "[" .. tostring(index) .. "]"
    if type(record) ~= "table" then
      add_error(errors, path, "object", type(record))
    else
      validate_scalar_member(errors, path .. ".subject_kind", record.subject_kind)
      validate_scalar_member(errors, path .. ".subject_id", record.subject_id)
      validate_scalar_member(errors, path .. ".procedure_index", record.procedure_index)
      validate_scalar_member(errors, path .. ".command", record.command)
      validate_scalar_member(errors, path .. ".exit_status", record.exit_status)
    end
  end
end

local function validate_comparisons(errors, path, comparisons)
  if not validate_array(errors, path, comparisons, true) then return end
  local shape, count = array_shape(comparisons)
  if shape ~= "nonempty" then return end
  for index = 1, count do
    local comparison = comparisons[index]
    local item_path = path .. "[" .. tostring(index) .. "]"
    if type(comparison) ~= "table" then
      add_error(errors, item_path, "object", type(comparison))
    else
      validate_scalar_member(errors, item_path .. ".source", comparison.source)
      validate_scalar_member(errors, item_path .. ".observed_result", comparison.observed_result)
      validate_scalar_member(errors, item_path .. ".match", comparison.match)
    end
  end
end

local function validate_evidence_records(errors, field, records)
  local shape, count = array_shape(records)
  if shape == "empty" then return true end
  if shape ~= "nonempty" then return false end
  local seen = {}
  for index = 1, count do
    local record = records[index]
    local path = field .. "[" .. tostring(index) .. "]"
    if type(record) ~= "table" then
      add_error(errors, path, "object", type(record))
    else
      for _, name in ipairs({
        "subject_kind", "subject_id", "subject", "source", "expected_result",
        "observed_result", "applicability", "match",
      }) do
        validate_scalar_member(errors, path .. "." .. name, record[name])
      end
      if type(record.procedure) ~= "table" then
        add_error(errors, path .. ".procedure", "object", type(record.procedure))
      else
        validate_scalar_member(errors, path .. ".procedure.kind", record.procedure.kind)
        if validate_array(errors, path .. ".procedure.steps", record.procedure.steps, true) then
          local steps_shape, steps_count = array_shape(record.procedure.steps)
          if steps_shape == "empty" and record.applicability == "applicable" then
            add_error(errors, path .. ".procedure.steps", "nonempty array", "empty")
          elseif steps_shape == "nonempty" then
            for step_index = 1, steps_count do
              validate_scalar_member(
                errors,
                path .. ".procedure.steps[" .. tostring(step_index) .. "]",
                record.procedure.steps[step_index]
              )
            end
          end
        end
      end
      validate_comparisons(errors, path .. ".comparisons", record.comparisons)
      local key = tostring(record.source) .. "\0" .. tostring(record.subject_kind) .. "\0" .. tostring(record.subject_id)
      if seen[key] then
        add_error(errors, path, "unique (source, subject_kind, subject_id)", "duplicate of " .. seen[key])
      else
        seen[key] = path
      end
    end
  end
end

local function validate_assessments(errors, assessments)
  local shape, count = array_shape(assessments)
  if shape == "empty" then return true end
  if shape ~= "nonempty" then return false end
  local seen = {}
  for index = 1, count do
    local assessment = assessments[index]
    local path = "reviewer_assessments[" .. tostring(index) .. "]"
    if type(assessment) ~= "table" then
      add_error(errors, path, "object", type(assessment))
    else
      for _, name in ipairs({
        "subject_kind", "subject_id", "subject", "source", "completion_state",
        "proof_verdict", "relevance", "sufficiency", "safety_and_executability",
        "currentness", "falsifiability", "non_circularity", "submission_verdict",
      }) do
        validate_scalar_member(errors, path .. "." .. name, assessment[name])
      end
      if validate_array(errors, path .. ".submission_issues", assessment.submission_issues, true) then
        local issues_shape, issues_count = array_shape(assessment.submission_issues)
        if issues_shape == "nonempty" then
          for issue_index = 1, issues_count do
            local issue = assessment.submission_issues[issue_index]
            local issue_path = path .. ".submission_issues[" .. tostring(issue_index) .. "]"
            if type(issue) ~= "table" then
              add_error(errors, issue_path, "object", type(issue))
            else
              for _, name in ipairs({ "source", "code", "field", "message" }) do
                validate_scalar_member(errors, issue_path .. "." .. name, issue[name])
              end
            end
          end
        end
      end
      local key = tostring(assessment.source)
        .. "\0" .. tostring(assessment.subject_kind)
        .. "\0" .. tostring(assessment.subject_id)
      if seen[key] then
        add_error(
          errors,
          path,
          "unique (source, subject_kind, subject_id)",
          "duplicate of " .. seen[key]
        )
      else
        seen[key] = path
      end
    end
  end
end

local function field_is_required(required, name)
  for _, value in ipairs(required or {}) do
    if value == name then return true end
  end
  return false
end

local function source_config(spec, prefix)
  for _, source in ipairs(spec.evidence or {}) do
    if source == prefix then return { name = prefix, required = false } end
    if type(source) == "table" and source.name == prefix then return source end
  end
  return nil
end

local function render_context(ctx, spec)
  local prev = ctx.prev
  local lines = {}
  local errors = {}
  if not prev then
    if spec.require_previous then add_error(errors, "prev", "object", "missing") end
    return lines, errors
  end
  if spec.heading and spec.heading ~= "" then table.insert(lines, spec.heading) end
  if spec.include_step and validate_scalar(errors, "prev.step", prev.step, false) then
    table.insert(lines, "Step: " .. tostring(prev.step))
  end
  if spec.include_status and validate_scalar(errors, "prev.status", prev.status, false) then
    table.insert(lines, "Status: " .. tostring(prev.status))
  end
  local fields = prev.fields or {}
  for _, name in ipairs(spec.fields or {}) do
    local required = field_is_required(spec.required_fields, name)
    local value = fields[name]
    if scalar_fields[name] then
      if validate_scalar(errors, "prev.fields." .. name, value, required) then
        table.insert(lines, scalar_fields[name] .. ": " .. tostring(value))
      end
    elseif array_fields[name] then
      local error_count = #errors
      if validate_array(errors, "prev.fields." .. name, value, required) then
        if name == "user_feedback" then
          validate_user_feedback_entries(errors, "prev.fields." .. name, value)
        else
          validate_scalar_array_entries(errors, "prev.fields." .. name, value)
        end
      end
      if #errors == error_count and value ~= nil then
        if name == "user_feedback" then
          append_user_feedback(lines, array_fields[name], value)
        else
          table.insert(lines, array_fields[name] .. ":")
          for _, item in ipairs(value) do table.insert(lines, "- " .. tostring(item)) end
        end
      end
    else
      add_error(errors, "spec.fields." .. name, "known field", "unknown")
    end
  end
  for _, source in ipairs(evidence_sources) do
    local selected = source_config(spec, source.prefix)
    if selected then
      local error_count = #errors
      local commands_field = source.prefix .. "_commands"
      local evidence_field = source.prefix .. "_evidence"
      local commands = fields[commands_field]
      local records = fields[evidence_field]
      local commands_path = "prev.fields." .. commands_field
      local evidence_path = "prev.fields." .. evidence_field
      if commands == nil and records ~= nil then
        add_error(errors, commands_path, "array paired with present " .. evidence_path, "missing")
      elseif commands ~= nil and records == nil then
        add_error(errors, evidence_path, "array paired with present " .. commands_path, "missing")
      else
        if validate_array(errors, commands_path, commands, selected.required) then
          validate_command_records(errors, commands_field, commands)
        end
        if validate_array(errors, evidence_path, records, selected.required) then
          validate_evidence_records(errors, evidence_field, records)
        end
      end
      if #errors == error_count and commands ~= nil and records ~= nil then
        append_command_records(lines, source.label, commands)
        append_evidence_records(lines, source.label, records)
      end
    end
  end
  if spec.reviewer_assessments then
    local required = spec.reviewer_assessments == "required"
    local error_count = #errors
    if validate_array(errors, "prev.fields.reviewer_assessments", fields.reviewer_assessments, required) then
      validate_assessments(errors, fields.reviewer_assessments)
      if #errors == error_count then
        append_reviewer_assessments(lines, fields.reviewer_assessments)
      end
    end
  end
  if spec.include_body and prev.body and tostring(prev.body) ~= "" then
    table.insert(lines, "Body:")
    table.insert(lines, tostring(prev.body))
  end
  return lines, errors
end

function M.build_agent_prompt(ctx, spec)
  local lines = {}
  local objective = tostring(spec.objective or "")
  if objective ~= "" then table.insert(lines, objective) end
  local context_lines, errors = render_context(ctx, spec)
  if #errors > 0 then return nil, errors end
  if #context_lines > 0 then
    if #lines > 0 then table.insert(lines, "") end
    for _, line in ipairs(context_lines) do table.insert(lines, line) end
  end
  for _, guidance in ipairs(spec.guidance or {}) do
    local text = guidance
    if guidance == "preserve_user_feedback" then text = M.preserve_user_feedback_guidance()
    elseif guidance == "review_user_feedback" then text = M.review_user_feedback_guidance()
    elseif guidance == "preserve_evidence" then text = M.preserve_evidence_guidance()
    elseif guidance == "evidence_records" then text = M.evidence_record_guidance()
    end
    text = tostring(text or ""):gsub("^%s+", ""):gsub("%s+$", "")
    if text ~= "" then
      table.insert(lines, "")
      table.insert(lines, text)
    end
  end
  local instructions = tostring(spec.instructions or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if instructions ~= "" then
    table.insert(lines, "")
    table.insert(lines, instructions)
  end
  return table.concat(lines, "\n"), nil
end

function M.previous_step_context(ctx, heading)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  local evidence = {}
  for _, source in ipairs(evidence_sources) do
    if fields[source.prefix .. "_commands"] ~= nil or fields[source.prefix .. "_evidence"] ~= nil then
      table.insert(evidence, source.prefix)
    end
  end
  local context_lines = render_context(ctx, {
    heading = heading,
    include_step = true,
    include_status = true,
    fields = {
      "user_feedback", "summary", "feedback", "goal", "validation", "work_dir",
      "plan_doc", "validation_doc", "rca_doc", "repro_test", "blocker_statement",
      "blocked_from_step", "blocked_from_status", "blocker_reason", "blocker_resolution",
      "files", "command", "commands", "failure", "failures",
    },
    evidence = evidence,
    reviewer_assessments = fields.reviewer_assessments ~= nil,
    include_body = true,
  })
  return table.concat(context_lines, "\n")
end

function M.invalid_context_action(ctx, status, errors)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  local diagnostics = {}
  for _, error in ipairs(errors or {}) do
    table.insert(
      diagnostics,
      error.field .. ": expected " .. error.expected .. ", got " .. error.actual
    )
  end
  local diagnostic_text = table.concat(diagnostics, "; ")
  local preserved = M.copy_evidence_fields(fields, {
    user_feedback = M.copy_user_feedback(fields),
    goal = fields.goal,
    validation = fields.validation,
    work_dir = fields.work_dir,
    plan_doc = fields.plan_doc,
    validation_doc = fields.validation_doc,
    rca_doc = fields.rca_doc,
    repro_test = fields.repro_test,
    files = fields.files,
    blocker_statement = fields.blocker_statement
      or "Workflow blocker context is missing or malformed.",
    blocked_from_step = fields.blocked_from_step
      or (ctx.prev and ctx.prev.step)
      or "unknown",
    blocked_from_status = fields.blocked_from_status
      or (ctx.prev and ctx.prev.status)
      or "blocked",
    blocker_reason = "Agent dispatch was skipped because required workflow context was invalid: "
      .. diagnostic_text,
    blocker_resolution = "Correct the malformed workflow context and retry the blocked step. "
      .. "Required corrections: " .. diagnostic_text,
    failures = diagnostics,
    feedback = diagnostic_text,
    summary = "Invalid required workflow context",
  })
  return action.status {
    status = status,
    fields = preserved,
    body = "Agent dispatch skipped because selected workflow context was missing or malformed.",
  }
end

function M.review_user_feedback_guidance()
  return [[

Evaluate the revised work against the complete user feedback history above as well as repository rules, document constraints, and test or validation evidence.]]
end

return M
