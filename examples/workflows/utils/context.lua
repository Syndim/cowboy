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
  if value_type == "nil" then return "<missing>" end
  if value_type == "string" then return "string(" .. string.format("%q", value) .. ")" end
  if value_type == "number" then return "number(" .. tostring(value) .. ")" end
  if value_type == "boolean" then return "boolean(" .. tostring(value) .. ")" end
  return "<invalid-scalar:type=" .. value_type .. ">"
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

local function append_required_scalar(lines, indent, label, value)
  table.insert(lines, indent .. label .. ": " .. typed_scalar(value))
end

local function append_required_array(lines, indent, label, value, append_item)
  if value == nil then
    table.insert(lines, indent .. label .. ": <missing-array>")
    return
  end
  local shape, detail = array_shape(value)
  if shape == "invalid" then
    table.insert(lines, indent .. label .. ": <invalid-array:type=" .. detail .. " value=" .. typed_scalar(value) .. ">")
    return
  end
  if shape == "malformed" then
    table.insert(lines, indent .. label .. ": <invalid-array:table-" .. detail .. ">")
    return
  end
  if shape == "empty" then
    table.insert(lines, indent .. label .. ": array(empty)")
    return
  end
  table.insert(lines, indent .. label .. ": array(nonempty,length=" .. tostring(detail) .. ")")
  for index = 1, detail do append_item(value[index], index) end
end

local function append_command_records(lines, label, records)
  append_required_array(lines, "", label .. " command records", records, function(record, index)
    table.insert(lines, "- Record " .. tostring(index) .. ":")
    if type(record) == "table" then
      append_required_scalar(lines, "  ", "Subject kind", record.subject_kind)
      append_required_scalar(lines, "  ", "Subject ID", record.subject_id)
      append_required_scalar(lines, "  ", "Procedure index", record.procedure_index)
      append_required_scalar(lines, "  ", "Command", record.command)
      append_required_scalar(lines, "  ", "Exit status", record.exit_status)
    else
      append_required_scalar(lines, "  ", "Record value", record)
    end
  end)
end

local function append_comparison(lines, comparison, index)
  table.insert(lines, "  - Comparison " .. tostring(index) .. ":")
  if type(comparison) ~= "table" then
    append_required_scalar(lines, "    ", "Record value", comparison)
    return
  end
  append_required_scalar(lines, "    ", "Source", comparison.source)
  append_required_scalar(lines, "    ", "Observed result", comparison.observed_result)
  append_required_scalar(lines, "    ", "Match", comparison.match)
end

local function append_evidence_records(lines, label, records)
  append_required_array(lines, "", label .. " evidence records", records, function(record, index)
    table.insert(lines, "- Record " .. tostring(index) .. ":")
    if type(record) ~= "table" then
      append_required_scalar(lines, "  ", "Record value", record)
    else
      append_required_scalar(lines, "  ", "Subject kind", record.subject_kind)
      append_required_scalar(lines, "  ", "Subject ID", record.subject_id)
      append_required_scalar(lines, "  ", "Subject", record.subject)
      append_required_scalar(lines, "  ", "Source", record.source)
      if type(record.procedure) == "table" then
        append_required_scalar(lines, "  ", "Procedure kind", record.procedure.kind)
        append_required_array(lines, "  ", "Procedure steps", record.procedure.steps, function(step, step_index)
          table.insert(lines, "  - Step " .. tostring(step_index) .. ": " .. typed_scalar(step))
        end)
      else
        table.insert(lines, "  Procedure: " .. typed_scalar(record.procedure))
      end
      append_required_scalar(lines, "  ", "Expected result", record.expected_result)
      append_required_scalar(lines, "  ", "Observed result", record.observed_result)
      append_required_scalar(lines, "  ", "Applicability", record.applicability)
      append_required_scalar(lines, "  ", "Match", record.match)
      append_required_array(lines, "  ", "Comparisons", record.comparisons, function(comparison, comparison_index)
        append_comparison(lines, comparison, comparison_index)
      end)
    end
  end)
end

local function append_submission_issue(lines, issue, index)
  table.insert(lines, "  - Issue " .. tostring(index) .. ":")
  if type(issue) ~= "table" then
    append_required_scalar(lines, "    ", "Record value", issue)
    return
  end
  append_required_scalar(lines, "    ", "Source", issue.source)
  append_required_scalar(lines, "    ", "Code", issue.code)
  append_required_scalar(lines, "    ", "Field", issue.field)
  append_required_scalar(lines, "    ", "Message", issue.message)
end

local function append_reviewer_assessments(lines, assessments)
  append_required_array(lines, "", "Reviewer soundness assessments", assessments, function(assessment, index)
    table.insert(lines, "- Assessment " .. tostring(index) .. ":")
    if type(assessment) ~= "table" then
      append_required_scalar(lines, "  ", "Record value", assessment)
    else
      append_required_scalar(lines, "  ", "Subject kind", assessment.subject_kind)
      append_required_scalar(lines, "  ", "Subject ID", assessment.subject_id)
      append_required_scalar(lines, "  ", "Subject", assessment.subject)
      append_required_scalar(lines, "  ", "Source", assessment.source)
      append_required_scalar(lines, "  ", "Completion state", assessment.completion_state)
      append_required_scalar(lines, "  ", "Proof verdict", assessment.proof_verdict)
      append_required_scalar(lines, "  ", "Relevance", assessment.relevance)
      append_required_scalar(lines, "  ", "Sufficiency", assessment.sufficiency)
      append_required_scalar(lines, "  ", "Safety and executability", assessment.safety_and_executability)
      append_required_scalar(lines, "  ", "Currentness", assessment.currentness)
      append_required_scalar(lines, "  ", "Falsifiability", assessment.falsifiability)
      append_required_scalar(lines, "  ", "Non-circularity", assessment.non_circularity)
      append_required_scalar(lines, "  ", "Submission verdict", assessment.submission_verdict)
      append_required_array(lines, "  ", "Submission issues", assessment.submission_issues, function(issue, issue_index)
        append_submission_issue(lines, issue, issue_index)
      end)
    end
  end)
end

local function append_source_evidence(lines, fields)
  for _, source in ipairs(evidence_sources) do
    append_command_records(lines, source.label, fields[source.prefix .. "_commands"])
    append_evidence_records(lines, source.label, fields[source.prefix .. "_evidence"])
  end
  append_reviewer_assessments(lines, fields.reviewer_assessments)
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
  append_source_evidence(lines, fields)
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


return M
