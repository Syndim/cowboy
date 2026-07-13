local roles = {
  planner = require("roles/planner.lua"),
  implementer = require("roles/implementer.lua"),
  tester = require("roles/tester.lua"),
  validator = require("roles/validator.lua"),
  reviewer = require("roles/reviewer.lua"),
  committer = require("roles/committer.lua"),
}

local collect_validation = require("steps/collect_validation.lua")("collect_validation")
local collect_validation_answer = require("steps/collect_validation.lua")("collect_validation_answer")
local plan = require("steps/plan.lua")(roles, { kind = "dev-loop", require_user_validation = true })
local clarify = require("steps/clarify.lua")("clarify")
local clarify_answer = require("steps/clarify.lua")("clarify_answer")
local review_plan = require("steps/review_plan.lua")(roles, { require_user_validation = true })
local confirm_plan = require("steps/confirm_plan.lua")("confirm_plan")
local confirm_plan_answer = require("steps/confirm_plan.lua")("confirm_plan_answer")
local implement = require("steps/implement.lua")(roles, { kind = "dev-loop" })
local test = require("steps/test.lua")(roles, { kind = "dev-loop" })
local validate = require("steps/validate_goal.lua")(roles)
local review = require("steps/review_implementation.lua")(roles, {
  evidence_heading = "Validator result:",
  review_subject = "implementation and validator result",
  require_user_validation = true,
})
local confirm_result = require("steps/confirm_result.lua")("confirm_result")
local confirm_result_answer = require("steps/confirm_result.lua")("confirm_result_answer")
local review_result_feedback = require("steps/review_result_feedback.lua")(roles)
local revise = require("steps/revise.lua")(roles, { feedback_source = "validation or review" })
local commit = require("steps/commit.lua")(roles)
local done = require("steps/done.lua")("goal implemented, validated, reviewed, and committed")
local blocked = require("steps/blocked.lua")("dev-loop workflow blocked")
local blocked_answer = require("steps/blocked.lua")("dev-loop workflow blocked", "blocked_answer")
local triage_blocked = require("steps/triage_blocked.lua")({
  id = "triage_blocked",
  validation_step = "validate",
})

collect_validation:on("answered", collect_validation_answer)
collect_validation_answer:on("answered", collect_validation_answer)
collect_validation_answer:on("captured", plan)
plan:on("ready", review_plan)
plan:on("unclear", clarify)
clarify:on("answered", clarify_answer)
clarify_answer:on("clarified", plan)
review_plan:on("approved", confirm_plan)
review_plan:on("changes_requested", plan)
confirm_plan:on("answered", confirm_plan_answer)
confirm_plan_answer:on("confirmed", implement)
confirm_plan_answer:on("changes_requested", plan)
implement:on("implemented", test)
implement:on("blocked", blocked)
test:on("passed", validate)
test:on("failed", revise)
test:on("blocked", blocked)
validate:on("achieved", review)
validate:on("not_achieved", revise)
validate:on("blocked", blocked)
review:on("approved", confirm_result)
review:on("changes_requested", revise)
review:on("replan_requested", plan)
confirm_result:on("answered", confirm_result_answer)
confirm_result_answer:on("confirmed", commit)
confirm_result_answer:on("changes_requested", review_result_feedback)
review_result_feedback:on("changes_requested", revise)
review_result_feedback:on("replan_requested", plan)
revise:on("implemented", test)
revise:on("blocked", blocked)
commit:on("committed", done)
commit:on("blocked", blocked)
blocked:on("answered", blocked_answer)
blocked_answer:on("triaged", triage_blocked)
triage_blocked:on("plan", plan)
triage_blocked:on("implement", implement)
triage_blocked:on("revise", revise)
triage_blocked:on("validate", validate)

return workflow("dev-loop", collect_validation, {
  description = "Capture a goal and user-defined validation, then plan, implement, validate, review, confirm, and commit",
})
