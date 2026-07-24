local context = require("utils/context.lua")

local function field_text(value)
  if value == nil then return "" end
  return tostring(value)
end

local function files_include(files, path)
  if type(files) ~= "table" or path == "" then return false end
  for _, file in ipairs(files) do
    if tostring(file) == path then return true end
  end
  return false
end

local function is_snake_case_summary(summary)
  return summary ~= ""
    and summary:match("^[a-z0-9_]+$") ~= nil
    and summary:match("^[a-z0-9]") ~= nil
    and summary:match("[a-z0-9]$") ~= nil
    and summary:find("__", 1, true) == nil
end

local function dev_loop_artifact_layout_is_valid(fields)
  fields = fields or {}
  local work_dir = field_text(fields.work_dir)
  local summary = work_dir:match("^docs/plans/(.+)$")
  local plan_doc = field_text(fields.plan_doc)
  local validation_doc = field_text(fields.validation_doc)

  return summary ~= nil
    and is_snake_case_summary(summary)
    and plan_doc == work_dir .. "/plan.md"
    and validation_doc == work_dir .. "/validation.md"
    and files_include(fields.files, plan_doc)
    and files_include(fields.files, validation_doc)
end

return function(roles, opts)
  opts = opts or {}
  local review_plan = step(opts.id or "review_plan", { role = roles.reviewer })
  local validation_guidance = ""
  if opts.require_user_validation then
    validation_guidance = [[

The prior context contains the user's exact `Goal: ...` and `Validation: ...` contract. Require the plan's `How to verify` section to use that Validation method without substitution, and reject a plan that weakens, rewrites, or omits it. Preserve both values exactly in the `goal` and `validation` output fields.]]
  end
  local validation_guide_guidance = ""
  if opts.require_validation_guide then
    validation_guide_guidance = [[

Read and review both `plan_doc` and `validation_doc`. A deterministic pre-review check accepts a dev-loop plan only when `work_dir` is `docs/plans/<snake_case_summary>`, `plan_doc` is `<work_dir>/plan.md`, `validation_doc` is `<work_dir>/validation.md`, both documents share that declared folder, and both document paths are included in `files`. The check rejects every flat, mismatched, or incomplete tuple, regardless of any extra fields supplied by the planner. Do not override its verdict.

Also reject the output unless both paths are stable and the validation guide contains prerequisites, ordered executable steps, evidence requirements, mandatory exit criteria, and continue/revise criteria. Require a stable, unique `VAL-NN` identifier for every ordered validation step and exit criterion, with exact criterion text, an executable command or ordered manual procedure, and an observable expected result. Reject missing, duplicate, renumbered, vague, unsafe, non-executable, or non-reproducible criteria. Require every exit criterion to pass before the development loop can end; reject ambiguous, incomplete, weakened, or substituted validation guidance.

Reject credentials, secrets, personal data, private paths, or proprietary content in either artifact. When redaction is necessary, verify that explicit safe placeholders or environment-variable references preserve the user's Goal and Validation procedure semantically and keep the guide executable. Include the reviewed content of both planning artifacts in the `plan` field shown for confirmation, and preserve `work_dir`, `plan_doc`, and `validation_doc` exactly in output fields.]]
  end
  review_plan.run = function(ctx)
    local fields = (ctx.prev and ctx.prev.fields) or {}
    local layout = "not_applicable"
    if opts.require_validation_guide then
      if not dev_loop_artifact_layout_is_valid(fields) then
        return action.status {
          status = "changes_requested",
          fields = {
            feedback = "Create the required dev-loop tuple: docs/plans/<snake_case_summary>/plan.md and validation.md in the same work_dir, with both paths listed in files.",
            user_feedback = context.copy_user_feedback(fields),
            goal = fields.goal,
            validation = fields.validation,
          },
          body = "invalid dev-loop planning artifact layout",
        }
      end
      layout = "valid_nested"
    end

    local layout_context = opts.require_validation_guide and ("\nArtifact layout check: " .. layout) or ""
    local prompt, errors = context.build_agent_prompt(ctx, {
      objective = "Review this plan before implementation.",
      heading = "Plan output:",
      require_previous = true,
      include_step = true,
      include_status = true,
      fields = {
        "user_feedback", "summary", "goal", "validation", "work_dir", "plan_doc",
        "validation_doc", "rca_doc", "repro_test", "files",
      },
      required_fields = { "plan_doc" },
      include_body = true,
      guidance = {
        layout_context,
        validation_guidance,
        validation_guide_guidance,
        "preserve_user_feedback",
        "review_user_feedback",
      },
      instructions = [[Verify the plan document does not include sensitive user data; require redaction or generalization of secrets, credentials, personal data, private paths, and proprietary customer content.

Require every plan TODO to use a stable, unique `TODO-NN` identifier and retain its exact task text. Each TODO must define an executable command or ordered manual procedure and an observable expected result. During replanning, reject renumbered or reused IDs; new work must receive the next unused ID. Reject missing, duplicate, vague, unsafe, non-executable, or non-reproducible TODO subjects.

Return "approved" only if the plan is specific, scoped, verifiable, and the plan document path is correct. For ordinary feature work, the plan path is `docs/plans/<snake_case_summary>.md`; for dev-loop work requiring a validation guide, every planning pass must use `docs/plans/<snake_case_summary>/` as `work_dir`, `<work_dir>/plan.md` as `plan_doc`, and `<work_dir>/validation.md` as `validation_doc`; for bug fixes with `Work dir: ...`, the plan path is `<work_dir>/plan.md` in the same `docs/plans/<snake_case_bug_summary>/` folder as the RCA. Verify the plan document contains the required Plan, Changes, Tests to be added/updated, How to verify, and TODO sections with Markdown task-list items. For bug fix plans, verify the plan references the reviewed RCA doc and treats the investigator-added repro test as an unchanged regression guard. Return "changes_requested" with feedback otherwise. In both cases, include a concise `plan` field containing the plan content that should be shown to the user for confirmation, preserve `plan_doc` exactly from the plan output, and preserve `work_dir`, `validation_doc`, `rca_doc`, and `repro_test` when present.]],
    })
    if not prompt then return context.invalid_context_action(ctx, "changes_requested", errors) end
    return action.agent {
      role = roles.reviewer,
      prompt = prompt,
      output = {
        status = { "approved", "changes_requested" },
        fields = { feedback = "string", plan = "string", user_feedback = "array", goal = "string", validation = "string", work_dir = "string", plan_doc = "string", validation_doc = "string", rca_doc = "string", repro_test = "string" },
      },
    }
  end
  return review_plan
end
